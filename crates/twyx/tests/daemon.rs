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

use twyx_protocol::client::{
    ClientHello, FromDaemon, MAJOR, MINOR, PaneInfo, PaneState, ServerHello, ToDaemon,
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
            assert!(Instant::now() < deadline, "the daemon never listened");
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
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    pub hello: ServerHello,
    screens: HashMap<u64, Vec<Vec<String>>>,
    panes: HashMap<u64, PaneInfo>,
    lease: Option<u64>,
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
                lease: None,
            },
            screens: HashMap::new(),
            panes: HashMap::new(),
            lease: None,
        };
        client
            .send(&ToDaemon::Hello(ClientHello {
                major: MAJOR,
                minor: MINOR,
                build: "test".to_owned(),
                cols,
                rows,
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
            FromDaemon::Lease { holder } => self.lease = *holder,
            FromDaemon::Pane(info) => {
                self.panes.insert(info.pane, info.clone());
            }
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

    fn wait_for_lease(&mut self, holder: Option<u64>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.lease != holder {
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

fn new_pane(client: &mut Client, cwd: &str, script: &str, cols: u16, rows: u16) {
    client
        .send(&ToDaemon::NewPane {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: cwd.to_owned(),
            cols,
            rows,
        })
        .expect("ask for a pane");
}

#[test]
fn a_pane_outlives_the_daemon_and_a_new_daemon_adopts_it() {
    let mut sandbox = Sandbox::new("adopt");
    sandbox.start();

    let mut client = sandbox.connect(40, 6);
    assert!(client.hello.panes.is_empty());
    new_pane(
        &mut client,
        &sandbox.cwd(),
        "printf 'hello'; exec sleep 8",
        40,
        6,
    );

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
    new_pane(&mut client, &sandbox.cwd(), "exec cat", 40, 6);
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
    new_pane(&mut first, &sandbox.cwd(), "exec sleep 8", 40, 6);
    let pane = first.wait_for_pane().pane;
    let first_id = first.hello.client;

    let mut second = sandbox.connect(60, 10);
    let second_id = second.hello.client;
    assert_eq!(second.hello.lease, Some(first_id));

    // Input from another client takes the lease before the input reaches the
    // pane, and with it the size.
    second
        .send(&ToDaemon::Input(InputEvent::Key(KeyEvent::press(
            KeyCode::Char('a'),
            Modifiers::empty(),
        ))))
        .expect("send input");
    first.wait_for_lease(Some(second_id));
    second.wait_for_cols(pane, 60);

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
        if second.screen_size(pane).0 == 60 {
            break;
        }
        assert!(Instant::now() < deadline, "the pane never arrived");
    }
    assert_eq!(second.screen_size(pane), (60, 10));

    // A disconnect releases the lease to the remaining client, whose size now
    // applies.
    drop(second);
    first.wait_for_lease(Some(first_id));
    first.wait_for_cols(pane, 70);
}
