//! The daemon end to end: panes that outlive it, adoption, the size lease and
//! frame forwarding. Drives the real binaries.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use twyx_core::layout::Axis;
use twyx_protocol::client::{
    ClientHello, FromDaemon, Launch, MAJOR, MINOR, PaneInfo, PaneState, Place, ServerHello,
    ToDaemon, WorkspaceInfo,
};
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use twyx_protocol::wire;

struct Sandbox {
    root: PathBuf,
    daemon: Option<Child>,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("twyx-daemon-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let runtime = root.join("run/twyx");
        std::fs::create_dir_all(&runtime).expect("create the runtime directory");
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
            .expect("set the runtime mode");
        std::fs::create_dir_all(root.join("state")).expect("create the state directory");
        std::fs::create_dir_all(root.join("config")).expect("create the config directory");
        Self { root, daemon: None }
    }

    fn socket(&self) -> PathBuf {
        self.root.join("run/twyx/twyx.sock")
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twyx"));
        command
            .args(arguments)
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
    }

    fn start(&mut self) {
        let log = std::fs::File::create(self.root.join("daemon.log")).expect("create the log");
        let child = self
            .command(&["daemon", "run"])
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start the daemon");
        self.daemon = Some(child);
        let deadline = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(self.socket()).is_err() {
            assert!(
                Instant::now() < deadline,
                "the daemon never listened: {}",
                self.daemon_log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn kill(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    fn connect(&self, cols: u16, rows: u16) -> Client {
        Client::connect(&self.socket(), cols, rows)
    }

    fn cwd(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.root.join("daemon.log")).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.kill();
        // Holders outlive the daemon by design; the sandbox's own end here.
        let _ = Command::new("pkill")
            .arg("-f")
            .arg(self.root.join("run/twyx/panes"))
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    pub hello: ServerHello,
    screens: HashMap<u64, Vec<Vec<String>>>,
    panes: HashMap<u64, PaneInfo>,
    workspace: Option<WorkspaceInfo>,
    tab: Option<u64>,
}

impl Client {
    fn connect(socket: &Path, cols: u16, rows: u16) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(stream) => break stream,
                Err(error) => {
                    assert!(Instant::now() < deadline, "connect: {error}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set a read timeout");
        let mut client = Self {
            reader: BufReader::new(stream.try_clone().expect("clone the socket")),
            writer: stream,
            hello: ServerHello {
                major: MAJOR,
                minor: MINOR,
                build: String::new(),
                pid: 0,
                client: 0,
                panes: Vec::new(),
                clients: 0,
                legacy: Vec::new(),
            },
            screens: HashMap::new(),
            panes: HashMap::new(),
            workspace: None,
            tab: None,
        };
        client
            .send(&ToDaemon::Hello(ClientHello {
                major: MAJOR,
                minor: MINOR,
                build: "test".to_owned(),
                cols,
                rows,
                interactive: true,
            }))
            .expect("send the hello");
        match client.receive() {
            FromDaemon::Welcome(hello) => client.hello = hello,
            other => panic!("expected a welcome, got {other:?}"),
        }
        for pane in client.hello.panes.clone() {
            client.panes.insert(pane.pane, pane);
        }
        client
    }

    fn send(&mut self, message: &ToDaemon) -> anyhow::Result<()> {
        wire::write(&mut self.writer, message)
    }

    fn receive(&mut self) -> FromDaemon {
        wire::read(&mut self.reader)
            .expect("read a message")
            .expect("the daemon stays connected")
    }

    fn pump(&mut self) -> FromDaemon {
        let message = self.receive();
        match &message {
            FromDaemon::Snapshot { pane, body, .. } => {
                let snapshot: Snapshot = wire::decode_body(body).expect("decode a snapshot");
                let rows = snapshot
                    .rows
                    .into_iter()
                    .map(|row| row.cells.into_iter().map(|cell| cell.text).collect())
                    .collect();
                self.screens.insert(*pane, rows);
            }
            FromDaemon::Frame { pane, body, .. } => {
                let frame: Frame = wire::decode_body(body).expect("decode a frame");
                let cols = frame.screen.cols as usize;
                let rows = frame.screen.rows as usize;
                let screen = self
                    .screens
                    .entry(*pane)
                    .or_insert_with(|| vec![vec![String::new(); cols]; rows]);
                screen.resize(rows, vec![String::new(); cols]);
                for row in screen.iter_mut() {
                    row.resize(cols, String::new());
                }
                for update in frame.changed {
                    if let Some(row) = screen.get_mut(update.row as usize) {
                        *row = update.cells.into_iter().map(|cell| cell.text).collect();
                    }
                }
            }
            FromDaemon::Workspace(workspace) => self.workspace = Some(workspace.clone()),
            FromDaemon::Focus { tab, .. } => self.tab = *tab,
            FromDaemon::Pane(info) => {
                self.panes.insert(info.pane, info.clone());
            }
            FromDaemon::PaneClosed { pane } => {
                self.panes.remove(pane);
            }
            FromDaemon::Refused { reason } => panic!("the daemon refused: {reason}"),
            FromDaemon::Clipboard { .. }
            | FromDaemon::Unsupported { .. }
            | FromDaemon::Welcome(_) => {}
        }
        message
    }

    fn wait_for_pane(&mut self) -> PaneInfo {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(info) = self.panes.values().next().cloned() {
                return info;
            }
            assert!(
                Instant::now() < deadline,
                "no pane arrived; the daemon logged: {}",
                ""
            );
            self.pump();
        }
    }

    fn text(&self, pane: u64) -> String {
        self.screens
            .get(&pane)
            .map(|rows| {
                rows.iter()
                    .map(|row| row.concat().trim_end().to_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn wait_for_text(&mut self, pane: u64, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.text(pane).contains(needle) {
            assert!(
                Instant::now() < deadline,
                "the screen never showed {needle:?}: {:?}",
                self.text(pane)
            );
            self.pump();
        }
    }

    fn screen_size(&self, pane: u64) -> (usize, usize) {
        match self.screens.get(&pane) {
            Some(rows) if !rows.is_empty() => (rows[0].len(), rows.len()),
            _ => (0, 0),
        }
    }

    /// The lease of the tab this client shows.
    fn lease(&self) -> Option<u64> {
        self.workspace
            .as_ref()?
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == self.tab)?
            .lease
    }

    fn wait_for_lease(&mut self, holder: Option<u64>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.lease() != holder {
            assert!(Instant::now() < deadline, "the lease never moved");
            self.pump();
        }
    }

    fn wait_for_cols(&mut self, pane: u64, cols: u16) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.panes.get(&pane).map(|info| info.cols) != Some(cols) {
            assert!(
                Instant::now() < deadline,
                "the pane never reached {cols} columns"
            );
            self.pump();
        }
    }
}

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
        })
        .expect("ask for a pane");
}

#[test]
fn a_pane_outlives_the_daemon_and_a_new_daemon_adopts_it() {
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
    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&Client) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            self.pump();
        }
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let code = if ch == '\r' {
                KeyCode::Enter
            } else {
                KeyCode::Char(ch)
            };
            self.send(&ToDaemon::Input(InputEvent::Key(KeyEvent::press(
                code,
                Modifiers::empty(),
            ))))
            .expect("send input");
        }
    }

    fn tab_count(&self) -> usize {
        self.workspace
            .as_ref()
            .map_or(0, |workspace| workspace.tabs.len())
    }

    fn size(&self, pane: u64) -> Option<(u16, u16)> {
        self.panes.get(&pane).map(|info| (info.cols, info.rows))
    }
}

fn new_pane_at(client: &mut Client, cwd: &str, launch: Launch, place: Place) -> u64 {
    let known: Vec<u64> = client.panes.keys().copied().collect();
    client
        .send(&ToDaemon::NewPane {
            launch,
            cwd: cwd.to_owned(),
            place,
        })
        .expect("ask for a pane");
    let mut created = None;
    client.wait_until("a new pane", |client| {
        created = client
            .panes
            .keys()
            .copied()
            .find(|pane| !known.contains(pane));
        created.is_some()
    });
    created.expect("a pane")
}

fn sh(script: &str) -> Launch {
    Launch::Command(vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ])
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
    // 81 columns hold three rules and 78 inner columns, half each.
    client.wait_until("two panes side by side", |c| {
        c.size(left) == Some((39, 8)) && c.size(right) == Some((39, 8))
    });
    let below = new_pane_at(
        &mut client,
        &cwd,
        sh("exec cat"),
        Place::Split(Axis::Column),
    );
    // The right pane splits in two: 10 rows hold three rules and 7 inner rows.
    client.wait_until("the right pane split", |c| {
        c.size(right) == Some((39, 4)) && c.size(below) == Some((39, 3))
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
        leaves.len() == 1 && leaves[0].pane.raw() == again
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
        .map(|leaf| leaf.pane.raw())
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
    assert_eq!(recovered.layout.leaves()[0].pane.raw(), 999);
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
