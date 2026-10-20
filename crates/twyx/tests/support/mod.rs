//! The daemon test harness: a sandbox with its own runtime, state and
//! config dirs, and a client that speaks the client protocol.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use twyx_protocol::client::{
    ClientHello, FromDaemon, Launch, MAJOR, MINOR, PaneInfo, Place, ServerHello, ToDaemon,
    WorkspaceInfo,
};
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use twyx_protocol::wire;

pub struct Sandbox {
    pub root: PathBuf,
    pub daemon: Option<Child>,
}

impl Sandbox {
    pub fn new(name: &str) -> Self {
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

    pub fn socket(&self) -> PathBuf {
        self.root.join("run/twyx/twyx.sock")
    }

    pub fn command(&self, arguments: &[&str]) -> Command {
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

    pub fn start(&mut self) {
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

    pub fn kill(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }

    pub fn connect(&self, cols: u16, rows: u16) -> Client {
        Client::connect(&self.socket(), cols, rows)
    }

    pub fn cwd(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    pub fn daemon_log(&self) -> String {
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

pub struct Client {
    pub reader: BufReader<UnixStream>,
    pub writer: UnixStream,
    pub hello: ServerHello,
    pub screens: HashMap<u64, Vec<Vec<String>>>,
    pub panes: HashMap<u64, PaneInfo>,
    pub workspace: Option<WorkspaceInfo>,
    pub tab: Option<u64>,
    /// The inbox, once the daemon sent it.
    pub inbox: Vec<twyx_protocol::client::InboxItem>,
    /// The notifications the daemon sent this client: body and sound.
    pub notices: Vec<(String, bool)>,
}

impl Client {
    pub fn connect(socket: &Path, cols: u16, rows: u16) -> Self {
        Self::connect_as(socket, cols, rows, true)
    }

    /// A command's connection: no lease, no frames.
    pub fn command(socket: &Path) -> Self {
        Self::connect_as(socket, 80, 24, false)
    }

    pub fn connect_as(socket: &Path, cols: u16, rows: u16, interactive: bool) -> Self {
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
            inbox: Vec::new(),
            notices: Vec::new(),
        };
        client
            .send(&ToDaemon::Hello(ClientHello {
                major: MAJOR,
                minor: MINOR,
                build: "test".to_owned(),
                cols,
                rows,
                interactive,
                workspace: None,
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

    pub fn send(&mut self, message: &ToDaemon) -> anyhow::Result<()> {
        wire::write(&mut self.writer, message)
    }

    pub fn receive(&mut self) -> FromDaemon {
        wire::read(&mut self.reader)
            .expect("read a message")
            .expect("the daemon stays connected")
    }

    pub fn pump(&mut self) -> FromDaemon {
        let message = self.receive();
        if let FromDaemon::Refused { reason } = &message {
            panic!("the daemon refused: {reason}");
        }
        self.track(&message);
        message
    }

    /// Pumps until `pick` takes a message. A refusal panics unless `pick`
    /// takes it.
    pub fn expect<T>(&mut self, what: &str, mut pick: impl FnMut(&FromDaemon) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "never got {what}");
            let message = self.receive();
            self.track(&message);
            if let Some(found) = pick(&message) {
                return found;
            }
            if let FromDaemon::Refused { reason } = &message {
                panic!("waiting for {what}, the daemon refused: {reason}");
            }
        }
    }

    /// Keeps the screens, panes and workspace a message changes.
    fn track(&mut self, message: &FromDaemon) {
        match message {
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
            FromDaemon::Inbox(items) => self.inbox = items.clone(),
            FromDaemon::Refused { .. } => {}
            FromDaemon::Notify { body, sound, .. } => self.notices.push((body.clone(), *sound)),
            FromDaemon::Clipboard { .. }
            | FromDaemon::TaskStarted { .. }
            | FromDaemon::Review(_)
            | FromDaemon::Unsupported { .. }
            | FromDaemon::Welcome(_)
            | FromDaemon::Opened { .. }
            | FromDaemon::Switched { .. }
            | FromDaemon::Overview(_)
            | FromDaemon::WorktreeAdded { .. }
            | FromDaemon::RemovalPlan(_)
            | FromDaemon::WorktreeRemoved { .. }
            | FromDaemon::Recipes(_)
            | FromDaemon::RecipeOpened { .. }
            | FromDaemon::RecipeTrusted { .. }
            | FromDaemon::SetupFile(_)
            | FromDaemon::SetupTrusted { .. }
            | FromDaemon::SetupDone { .. } => {}
        }
    }

    pub fn type_text(&mut self, text: &str) {
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

    pub fn wait_until(&mut self, what: &str, mut done: impl FnMut(&Client) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            self.pump();
        }
    }

    pub fn wait_for_pane(&mut self) -> PaneInfo {
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

    pub fn text(&self, pane: u64) -> String {
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

    pub fn wait_for_text(&mut self, pane: u64, needle: &str) {
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

    pub fn screen_size(&self, pane: u64) -> (usize, usize) {
        match self.screens.get(&pane) {
            Some(rows) if !rows.is_empty() => (rows[0].len(), rows.len()),
            _ => (0, 0),
        }
    }

    /// The lease of the tab this client shows.
    pub fn lease(&self) -> Option<u64> {
        self.workspace
            .as_ref()?
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == self.tab)?
            .lease
    }

    pub fn wait_for_lease(&mut self, holder: Option<u64>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.lease() != holder {
            assert!(Instant::now() < deadline, "the lease never moved");
            self.pump();
        }
    }

    pub fn wait_for_cols(&mut self, pane: u64, cols: u16) {
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

pub fn new_pane_at(client: &mut Client, cwd: &str, launch: Launch, place: Place) -> u64 {
    let known: Vec<u64> = client.panes.keys().copied().collect();
    client
        .send(&ToDaemon::NewPane {
            launch,
            cwd: cwd.to_owned(),
            place,
            history: true,
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

pub fn sh(script: &str) -> Launch {
    Launch::Command(vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ])
}
