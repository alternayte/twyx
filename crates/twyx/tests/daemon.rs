//! The daemon end to end: panes that outlive it, adoption, the size lease and
//! frame forwarding. Drives the real binaries.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use twyx_core::layout::Axis;
use twyx_protocol::client::{
    FromDaemon, Launch, MAJOR, MINOR, PaneState, Place, ServerHello, ToDaemon,
};
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use twyx_protocol::wire;

mod support;

use support::{Client, Sandbox, new_pane_at, sh};

fn new_pane(client: &mut Client, cwd: &str, script: &str) {
    client
        .send(&ToDaemon::NewPane {
            launch: Launch::Command(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                script.to_owned(),
            ]),
            cwd: cwd.to_owned(),
            place: Place::Split(Axis::Row),
            history: true,
        })
        .expect("ask for a pane");
}

#[test]
fn invariant_1_a_pane_outlives_the_daemon_and_a_new_daemon_adopts_it() {
    let mut sandbox = Sandbox::new("adopt");
    sandbox.start();

    let mut client = sandbox.connect(40, 6);
    assert!(client.hello.panes.is_empty());
    new_pane(&mut client, &sandbox.cwd(), "printf 'hello'; exec sleep 8");

    let pane = client.wait_for_pane();
    client.wait_for_text(pane.pane, "hello");

    let first_pid = client.hello.pid;
    sandbox.kill();
    drop(client);

    sandbox.start();
    let mut client = sandbox.connect(40, 6);
    assert_ne!(client.hello.pid, first_pid, "the daemon restarted");
    assert_eq!(
        client.hello.panes.len(),
        1,
        "the pane was adopted; the daemon logged: {}",
        sandbox.daemon_log()
    );
    let adopted = client.hello.panes[0].pane;
    client.wait_for_text(adopted, "hello");

    client
        .send(&ToDaemon::KillPane { pane: adopted })
        .expect("close the pane");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.pump();
        if let Some(info) = client.panes.get(&adopted)
            && info.state != PaneState::Running
        {
            break;
        }
        assert!(Instant::now() < deadline, "the pane never ended");
    }
}

#[test]
fn input_reaches_an_adopted_pane() {
    let mut sandbox = Sandbox::new("input");
    sandbox.start();
    let mut client = sandbox.connect(40, 6);
    new_pane(&mut client, &sandbox.cwd(), "exec cat");
    client.wait_for_pane();

    sandbox.kill();
    drop(client);
    sandbox.start();
    let mut client = sandbox.connect(40, 6);
    let adopted = client.hello.panes[0].pane;

    for ch in "typed-after-adoption".chars() {
        client
            .send(&ToDaemon::Input(InputEvent::Key(KeyEvent::press(
                KeyCode::Char(ch),
                Modifiers::empty(),
            ))))
            .expect("send input");
    }
    client.wait_for_text(adopted, "typed-after-adoption");
}

#[test]
fn invariant_3_only_the_leaseholder_sets_the_pane_size() {
    let mut sandbox = Sandbox::new("lease");
    sandbox.start();

    let mut first = sandbox.connect(40, 6);
    new_pane(&mut first, &sandbox.cwd(), "exec sleep 8");
    let pane = first.wait_for_pane().pane;
    let first_id = first.hello.client;

    let mut second = sandbox.connect(60, 10);
    let second_id = second.hello.client;
    second.wait_for_lease(Some(first_id));

    // Input from another client takes the lease before the input reaches the
    // pane, and with it the size.
    second
        .send(&ToDaemon::Input(InputEvent::Key(KeyEvent::press(
            KeyCode::Char('a'),
            Modifiers::empty(),
        ))))
        .expect("send input");
    first.wait_for_lease(Some(second_id));
    // One pane fills the tab inside its one-cell frame.
    second.wait_for_cols(pane, 58);

    // A resize from the client without the lease changes nothing.
    first
        .send(&ToDaemon::Resize { cols: 70, rows: 12 })
        .expect("send a resize");
    first
        .send(&ToDaemon::Snapshot { pane })
        .expect("ask for the screen");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        second.pump();
        if second.screen_size(pane).0 == 58 {
            break;
        }
        assert!(Instant::now() < deadline, "the pane never arrived");
    }
    assert_eq!(second.screen_size(pane), (58, 8));

    // A disconnect releases the lease to the remaining client, whose size now
    // applies.
    drop(second);
    first.wait_for_lease(Some(first_id));
    first.wait_for_cols(pane, 68);
}

impl Client {
    fn tab_count(&self) -> usize {
        self.workspace
            .as_ref()
            .map_or(0, |workspace| workspace.tabs.len())
    }

    fn size(&self, pane: u64) -> Option<(u16, u16)> {
        self.panes.get(&pane).map(|info| (info.cols, info.rows))
    }
}

#[test]
fn a_split_sizes_every_pane_to_its_place_in_the_leaseholders_area() {
    let mut sandbox = Sandbox::new("split");
    sandbox.start();
    let mut client = sandbox.connect(81, 10);
    let cwd = sandbox.cwd();
    let left = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    client.wait_until("one pane in its frame", |c| c.size(left) == Some((79, 8)));
    let right = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    // 81 columns hold four rules, one blank and 76 inner columns, half each.
    client.wait_until("two panes side by side", |c| {
        c.size(left) == Some((38, 8)) && c.size(right) == Some((38, 8))
    });
    let below = new_pane_at(
        &mut client,
        &cwd,
        sh("exec cat"),
        Place::Split(Axis::Column),
    );
    // The right pane splits in two: 10 rows hold two boxes a blank apart and
    // 5 inner rows.
    client.wait_until("the right pane split", |c| {
        c.size(right) == Some((38, 3)) && c.size(below) == Some((38, 2))
    });
}

#[test]
fn each_client_has_its_own_focus_and_input_follows_it() {
    let mut sandbox = Sandbox::new("focus");
    sandbox.start();
    let mut first = sandbox.connect(80, 10);
    let cwd = sandbox.cwd();
    let left = new_pane_at(&mut first, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    let right = new_pane_at(&mut first, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    // The first client focused the pane it made last.
    let mut second = sandbox.connect(80, 10);
    second
        .send(&ToDaemon::Focus {
            side: twyx_core::layout::Side::Left,
        })
        .expect("focus left");
    second.type_text("from-second");
    first.type_text("from-first");
    first.wait_for_text(left, "from-second");
    first.wait_for_text(right, "from-first");
    assert!(!first.text(left).contains("from-first"));
}

#[test]
fn a_shell_pane_closes_with_its_shell_and_a_command_pane_stays_to_run_again() {
    let mut sandbox = Sandbox::new("exit");
    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    let cwd = sandbox.cwd();
    let command = new_pane_at(
        &mut client,
        &cwd,
        sh("echo ran; read line; exit 3"),
        Place::Split(Axis::Row),
    );
    let shell = new_pane_at(&mut client, &cwd, Launch::Shell, Place::Split(Axis::Row));
    client.wait_until("the shell prompt", |c| c.panes.contains_key(&shell));
    client.type_text("exit\r");
    client.wait_until("the shell pane closed", |c| !c.panes.contains_key(&shell));
    assert_eq!(client.tab_count(), 1);

    client
        .send(&ToDaemon::FocusLeaf {
            leaf: client
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.tabs[0].layout.leaves().first().copied())
                .expect("a leaf")
                .id,
        })
        .expect("focus the command pane");
    client.wait_for_text(command, "ran");
    client.type_text("\r");
    client.wait_until("the command ended", |c| {
        c.panes.get(&command).map(|info| info.state.clone()) == Some(PaneState::Exited { code: 3 })
    });

    client
        .send(&ToDaemon::RunAgain { pane: command })
        .expect("run again");
    client.wait_until("the old pane replaced", |c| {
        !c.panes.contains_key(&command) && !c.panes.is_empty()
    });
    let again = *client.panes.keys().next().expect("the new pane");
    client.wait_for_text(again, "ran");
    client.wait_until("the same leaf shows the new pane", |c| {
        let leaves = c.workspace.as_ref().expect("a workspace").tabs[0]
            .layout
            .leaves();
        leaves.len() == 1 && leaves[0].terminal().map(|pane| pane.raw()) == Some(again)
    });
}

#[test]
fn tabs_open_rename_and_close_with_their_panes() {
    let mut sandbox = Sandbox::new("tabs");
    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    let cwd = sandbox.cwd();
    let first = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    let second = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::NewTab);
    client.wait_until("two tabs, the new one shown", |c| {
        c.tab_count() == 2 && c.tab == Some(c.workspace.as_ref().unwrap().tabs[1].tab)
    });
    let tabs: Vec<u64> = client
        .workspace
        .as_ref()
        .unwrap()
        .tabs
        .iter()
        .map(|t| t.tab)
        .collect();

    client
        .send(&ToDaemon::RenameTab {
            tab: tabs[1],
            name: "logs".to_owned(),
        })
        .expect("rename");
    client.wait_until("the name", |c| {
        c.workspace.as_ref().unwrap().tabs[1].name == "logs"
    });

    client
        .send(&ToDaemon::CloseTab { tab: tabs[1] })
        .expect("close the tab");
    client.wait_until("one tab and its pane gone", |c| {
        c.tab_count() == 1 && !c.panes.contains_key(&second) && c.tab == Some(tabs[0])
    });
    assert!(client.panes.contains_key(&first));
}

impl Sandbox {
    fn panes_dir(&self) -> PathBuf {
        self.root.join("run/twyx/panes")
    }

    /// Kills one holder the way a crash or a reboot would.
    fn kill_holder(&self, pane: u64) {
        let socket = self.panes_dir().join(format!("{}.sock", stem(pane)));
        let status = Command::new("pkill")
            .args(["-9", "-f"])
            .arg(&socket)
            .status()
            .expect("run pkill");
        assert!(status.success(), "no holder for pane {pane}");
    }

    /// Starts a holder the daemon has no row for.
    fn stray_holder(&self, pane: u64) {
        let socket = self.panes_dir().join(format!("{}.sock", stem(pane)));
        let mut child = self
            .command(&[
                "holder",
                "--pane",
                &stem(pane),
                "--socket",
                &socket.to_string_lossy(),
                "--",
                "/bin/sh",
                "-c",
                "exec cat",
            ])
            .stderr(Stdio::null())
            .spawn()
            .expect("start a holder");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "the stray holder never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        // The sandbox ends it; this reaps it.
        std::thread::spawn(move || child.wait());
    }
}

fn stem(pane: u64) -> String {
    twyx_core::pane::PaneId::from_raw(pane).stem()
}

#[test]
fn invariant_8_after_a_daemon_start_running_means_a_live_holder_owned_by_one_pane() {
    let mut sandbox = Sandbox::new("inv8");
    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    let cwd = sandbox.cwd();
    let live = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    let crashed = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::Split(Axis::Row));
    client.wait_until("both panes saved", |c| {
        c.workspace.as_ref().is_some_and(|w| {
            w.tabs
                .iter()
                .map(|t| t.layout.leaves().len())
                .sum::<usize>()
                == 2
        })
    });
    drop(client);
    sandbox.kill();
    sandbox.kill_holder(crashed);
    sandbox.stray_holder(999);

    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    client.wait_until("the workspace", |c| c.workspace.is_some());
    let states: HashMap<u64, PaneState> = client
        .hello
        .panes
        .iter()
        .map(|info| (info.pane, info.state.clone()))
        .collect();
    assert_eq!(states.get(&live), Some(&PaneState::Running));
    assert_eq!(states.get(&crashed), Some(&PaneState::Lost));
    assert_eq!(states.get(&999), Some(&PaneState::Running));

    let workspace = client.workspace.clone().expect("a workspace");
    let mut shown: Vec<u64> = workspace
        .tabs
        .iter()
        .flat_map(|tab| tab.layout.leaves())
        .filter_map(|leaf| leaf.terminal().map(|pane| pane.raw()))
        .collect();
    shown.sort_unstable();
    let mut expected = vec![live, crashed, 999];
    expected.sort_unstable();
    assert_eq!(shown, expected, "every pane in exactly one leaf");
    let recovered = workspace
        .tabs
        .iter()
        .find(|tab| tab.name == "Recovered")
        .expect("a Recovered tab");
    assert_eq!(recovered.layout.leaves()[0].terminal().unwrap().raw(), 999);
}

#[test]
fn tabs_and_layouts_survive_a_host_restart_and_ended_panes_run_again() {
    let mut sandbox = Sandbox::new("reboot");
    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    let cwd = sandbox.cwd();
    let left = new_pane_at(
        &mut client,
        &cwd,
        sh("echo left; exec cat"),
        Place::Split(Axis::Row),
    );
    let right = new_pane_at(
        &mut client,
        &cwd,
        sh("exec cat"),
        Place::Split(Axis::Column),
    );
    let logs = new_pane_at(&mut client, &cwd, sh("exec cat"), Place::NewTab);
    client.wait_until("two tabs", |c| c.tab_count() == 2);
    let logs_tab = client.workspace.as_ref().unwrap().tabs[1].tab;
    client
        .send(&ToDaemon::RenameTab {
            tab: logs_tab,
            name: "logs".to_owned(),
        })
        .expect("rename");
    client.wait_until("the name", |c| {
        c.workspace.as_ref().unwrap().tabs[1].name == "logs"
    });
    let before = client.workspace.clone().expect("a workspace");
    drop(client);

    // A host restart ends the daemon and every holder.
    sandbox.kill();
    for pane in [left, right, logs] {
        sandbox.kill_holder(pane);
    }
    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    client.wait_until("the workspace", |c| c.workspace.is_some());
    let after = client.workspace.clone().expect("a workspace");
    assert_eq!(
        after
            .tabs
            .iter()
            .map(|t| (&t.name, &t.layout))
            .collect::<Vec<_>>(),
        before
            .tabs
            .iter()
            .map(|t| (&t.name, &t.layout))
            .collect::<Vec<_>>(),
    );
    for info in &client.hello.panes {
        assert_eq!(info.state, PaneState::Lost, "pane {}", info.pane);
    }
    assert!(client.panes[&left].cwd == cwd);

    client
        .send(&ToDaemon::RunAgain { pane: left })
        .expect("run again");
    client.wait_until("a new pane in the left leaf", |c| {
        !c.panes.contains_key(&left) && c.panes.len() == 3
    });
    let again = *client
        .panes
        .keys()
        .find(|pane| ![right, logs].contains(pane))
        .expect("the new pane");
    client.wait_for_text(again, "left");
}

#[test]
fn invariant_9_only_the_daemon_opens_the_database() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for entry in std::fs::read_dir(&crates)
        .expect("list the crates")
        .flatten()
    {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        assert_eq!(
            text.contains("rusqlite"),
            name == "twyx-daemon",
            "{name} and the SQLite dependency"
        );
    }
}

/// A daemon from another build: it greets every client, says how many other
/// interactive clients it has, and reports whether a client asked it to stop.
fn old_daemon(sandbox: &Sandbox, clients: u32) -> std::sync::mpsc::Receiver<bool> {
    use std::os::unix::net::UnixListener;
    let socket = sandbox.socket();
    let listener = UnixListener::bind(&socket).expect("listen as the old daemon");
    let (told, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = stream;
            if reader
                .fill_buf()
                .ok()
                .and_then(|bytes| bytes.first().copied())
                == Some(b'{')
            {
                if old_api(&mut reader, &mut writer, clients, &socket) {
                    let _ = told.send(true);
                    return;
                }
                let _ = told.send(false);
                continue;
            }
            if !matches!(
                wire::read::<ToDaemon>(&mut reader),
                Ok(Some(ToDaemon::Hello(_)))
            ) {
                continue;
            }
            let _ = wire::write(
                &mut writer,
                &FromDaemon::Welcome(ServerHello {
                    major: MAJOR,
                    minor: MINOR,
                    build: "an-older-build".to_owned(),
                    pid: std::process::id(),
                    client: 1,
                    panes: Vec::new(),
                    clients,
                    legacy: Vec::new(),
                }),
            );
            if let Ok(Some(ToDaemon::Shutdown)) = wire::read::<ToDaemon>(&mut reader) {
                let _ = told.send(true);
                // An old daemon that stops takes its socket with it.
                let _ = std::fs::remove_file(&socket);
                return;
            }
            let _ = told.send(false);
        }
    });
    stopped
}

/// The old daemon's side of API v1: `hello`, an empty `pane.list`, and
/// `daemon.stop`, which it reports.
fn old_api(
    reader: &mut BufReader<std::os::unix::net::UnixStream>,
    writer: &mut std::os::unix::net::UnixStream,
    clients: u32,
    socket: &Path,
) -> bool {
    use std::io::Write;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let request: serde_json::Value = serde_json::from_str(&line).expect("a request");
        let result = match request["method"].as_str() {
            Some("hello") => serde_json::json!({
                "version": 1, "minor": 0, "build": "an-older-build",
                "pid": std::process::id(), "clients": clients,
            }),
            Some("daemon.stop") => {
                // An old daemon that stops takes its socket with it.
                let _ = std::fs::remove_file(socket);
                return true;
            }
            _ => serde_json::json!([]),
        };
        let reply = serde_json::json!({ "id": request["id"], "result": result });
        let _ = writeln!(writer, "{reply}");
    }
    false
}

#[test]
fn a_client_alone_replaces_a_daemon_from_another_build() {
    let sandbox = Sandbox::new("upgrade");
    let stopped = old_daemon(&sandbox, 0);
    let output = sandbox
        .command(&["pane", "list"])
        .stdout(Stdio::piped())
        .output()
        .expect("run twyx pane list");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stopped.recv_timeout(Duration::from_secs(5)), Ok(true));
    let mut client = sandbox.connect(80, 10);
    assert_ne!(client.hello.build, "an-older-build");
    client.wait_until("this build's daemon", |c| c.workspace.is_some());
    let _ = client.send(&ToDaemon::Shutdown);
}

#[test]
fn a_command_keeps_a_daemon_from_another_build_that_clients_use() {
    let sandbox = Sandbox::new("keep");
    let stopped = old_daemon(&sandbox, 1);
    let output = sandbox
        .command(&["pane", "list"])
        .stdout(Stdio::piped())
        .output()
        .expect("run twyx pane list");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stopped.recv_timeout(Duration::from_secs(5)), Ok(false));
}

#[test]
fn a_holder_too_old_to_adopt_is_listed_with_its_pid() {
    use std::os::unix::net::UnixListener;
    use twyx_protocol::holder::{FromHolder, ToHolder};
    let mut sandbox = Sandbox::new("legacy");
    std::fs::create_dir_all(sandbox.panes_dir()).expect("create the panes directory");
    let socket = sandbox.panes_dir().join(format!("{}.sock", stem(7)));
    let listener = UnixListener::bind(&socket).expect("listen as an old holder");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = stream;
            if let Ok(Some(ToHolder::Hello(_))) = wire::read::<ToHolder>(&mut reader) {
                let _ = wire::write(&mut writer, &FromHolder::Unsupported { major: 0, minor: 9 });
            }
        }
    });
    sandbox.start();
    let client = sandbox.connect(80, 10);
    assert_eq!(client.hello.legacy.len(), 1);
    let legacy = &client.hello.legacy[0];
    assert_eq!((legacy.major, legacy.pid), (0, Some(std::process::id())));
    assert!(socket.exists(), "a legacy holder keeps its socket");
}

impl Sandbox {
    fn history(&self, pane: u64) -> PathBuf {
        self.root.join("state/twyx/history").join(stem(pane))
    }

    fn wait_for_history(&self, pane: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.history(pane).join("checkpoint-1").exists() {
            assert!(Instant::now() < deadline, "no history for pane {pane}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the history files stop changing: the history writer runs
    /// behind the screen.
    fn wait_for_history_writes(&self, pane: u64) {
        let sizes = || -> Vec<(std::ffi::OsString, u64)> {
            let mut sizes: Vec<_> = std::fs::read_dir(self.history(pane))
                .map(|entries| {
                    entries
                        .filter_map(|entry| {
                            let entry = entry.ok()?;
                            Some((entry.file_name(), entry.metadata().ok()?.len()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            sizes.sort();
            sizes
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = sizes();
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(500) {
            assert!(Instant::now() < deadline, "the history never settled");
            std::thread::sleep(Duration::from_millis(20));
            let now = sizes();
            if now != last {
                last = now;
                since = Instant::now();
            }
        }
    }
}

#[test]
fn invariant_21_a_pane_whose_holder_died_shows_its_last_screen_from_history() {
    let mut sandbox = Sandbox::new("inv21");
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    let cwd = sandbox.cwd();
    let pane = new_pane_at(
        &mut client,
        &cwd,
        sh("for n in 1 2 3; do echo \"history line $n\"; done; exec cat"),
        Place::Split(Axis::Row),
    );
    client.wait_for_text(pane, "history line 3");
    sandbox.wait_for_history(pane);
    // Output after the checkpoint lands in the log.
    // The terminal echoes the line, and cat writes it again.
    client.type_text("logged-after\r");
    client.wait_for_text(pane, "logged-after\nlogged-after");
    sandbox.wait_for_history_writes(pane);
    let before = client.text(pane);
    drop(client);

    sandbox.kill();
    sandbox.kill_holder(pane);
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    client.wait_until("the restored screen", |c| !c.text(pane).is_empty());
    assert_eq!(client.text(pane), before);
    assert_eq!(client.panes[&pane].state, PaneState::Lost);
}

#[test]
fn invariant_22_closing_a_pane_deletes_its_history_and_a_pane_without_one_writes_none() {
    let mut sandbox = Sandbox::new("inv22");
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    let cwd = sandbox.cwd();
    let kept = new_pane_at(
        &mut client,
        &cwd,
        sh("echo kept; exec cat"),
        Place::Split(Axis::Row),
    );
    sandbox.wait_for_history(kept);
    client
        .send(&ToDaemon::NewPane {
            launch: sh("echo private; exec cat"),
            cwd: cwd.clone(),
            place: Place::Split(Axis::Row),
            history: false,
        })
        .expect("ask for a pane");
    let mut private = 0;
    client.wait_until("the private pane", |c| {
        private = c.panes.keys().copied().find(|p| *p != kept).unwrap_or(0);
        private != 0
    });
    client.wait_for_text(private, "private");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !sandbox.history(private).exists(),
        "a pane without history wrote some"
    );

    client
        .send(&ToDaemon::ClosePane)
        .expect("close the focused pane");
    client.wait_until("the private pane closed", |c| {
        !c.panes.contains_key(&private)
    });
    client
        .send(&ToDaemon::FocusLeaf {
            leaf: client.workspace.as_ref().unwrap().tabs[0].layout.leaves()[0].id,
        })
        .expect("focus the kept pane");
    client.send(&ToDaemon::ClosePane).expect("close it");
    client.wait_until("the kept pane closed", |c| !c.panes.contains_key(&kept));
    let deadline = Instant::now() + Duration::from_secs(5);
    while sandbox.history(kept).exists() {
        assert!(Instant::now() < deadline, "the history outlived its pane");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn run_again_shows_the_old_run_above_a_divider() {
    let mut sandbox = Sandbox::new("again");
    sandbox.start();
    let mut client = sandbox.connect(60, 12);
    let cwd = sandbox.cwd();
    let pane = new_pane_at(
        &mut client,
        &cwd,
        sh("echo \"run $(cat count 2>/dev/null || echo 1)\"; echo 2 > count; exec cat"),
        Place::Split(Axis::Row),
    );
    client.wait_for_text(pane, "run 1");
    sandbox.wait_for_history(pane);
    drop(client);
    sandbox.kill();
    sandbox.kill_holder(pane);
    sandbox.start();

    let mut client = sandbox.connect(60, 12);
    client.wait_until("the lost pane", |c| c.panes.contains_key(&pane));
    client
        .send(&ToDaemon::RunAgain { pane })
        .expect("run again");
    client.wait_until("the new run", |c| {
        !c.panes.contains_key(&pane) && c.panes.len() == 1
    });
    let again = *client.panes.keys().next().expect("the new pane");
    client.wait_for_text(again, "run 2");
    let text = client.text(again);
    let old = text.find("run 1").expect("the old run is there");
    let divider = text.find("ran again").expect("a divider");
    assert!(
        old < divider && divider < text.find("run 2").expect("the new run"),
        "{text}"
    );
}

#[test]
fn invariant_14_only_the_focused_pane_sets_the_clipboard_by_default() {
    let mut sandbox = Sandbox::new("osc52");
    sandbox.start();
    let mut client = sandbox.connect(80, 10);
    let cwd = sandbox.cwd();
    // The pane made last has the focus.
    let hidden = new_pane_at(
        &mut client,
        &cwd,
        sh("sleep 0.5; printf '\\033]52;c;aGlkZGVu\\007'; exec cat"),
        Place::Split(Axis::Row),
    );
    let focused = new_pane_at(
        &mut client,
        &cwd,
        sh("sleep 1.5; printf '\\033]52;c;Zm9jdXNlZA==\\007'; exec cat"),
        Place::Split(Axis::Row),
    );
    let mut from_hidden = false;
    let text = client.expect(
        "the focused pane's clipboard write",
        |message| match message {
            FromDaemon::Clipboard { pane, .. } if *pane == hidden => {
                from_hidden = true;
                None
            }
            FromDaemon::Clipboard { pane, text } if *pane == focused => Some(text.clone()),
            _ => None,
        },
    );
    assert_eq!(text, "focused");
    assert!(!from_hidden, "a pane without the focus set the clipboard");
}
