//! The daemon: one tab, its panes, the clients attached to them, and the one
//! size lease.
//!
//! Threads: each client and each holder gets a reader thread and a writer
//! thread. This thread owns the state and forwards frames without reading
//! them.

use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::client::{
    ClientHello, FromDaemon, MAJOR, MINOR, PaneInfo, PaneState, ServerHello, ToDaemon,
};
use twyx_protocol::holder::{ExitState, FromHolder, Hello, ToHolder, Welcome};
use twyx_protocol::wire;

use crate::peer;
use twyx_core::paths::Paths;

static CLIENTS: AtomicU64 = AtomicU64::new(1);

enum Event {
    ClientHello {
        client: u64,
        hello: ClientHello,
        out: Sender<FromDaemon>,
    },
    ClientMessage {
        client: u64,
        message: ToDaemon,
    },
    ClientGone {
        client: u64,
    },
    Holder {
        pane: PaneId,
        message: FromHolder,
    },
    HolderGone {
        pane: PaneId,
    },
}

struct HolderLink {
    out: Sender<ToHolder>,
}

impl std::fmt::Debug for HolderLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HolderLink")
    }
}

impl HolderLink {
    fn connect(
        socket: PathBuf,
        pane: PaneId,
        build: String,
        events: Sender<Event>,
    ) -> Result<(Self, Welcome)> {
        let stream = UnixStream::connect(&socket)
            .with_context(|| format!("connect to {}", socket.display()))?;
        let mut reader = BufReader::new(stream.try_clone().context("clone the holder stream")?);
        let mut writer = BufWriter::new(stream);

        wire::write(
            &mut writer,
            &ToHolder::Hello(Hello {
                major: twyx_protocol::holder::MAJOR,
                minor: twyx_protocol::holder::MINOR,
                pane: pane.raw(),
                build: build.clone(),
            }),
        )
        .context("send the holder hello")?;

        let welcome = match wire::read::<FromHolder>(&mut reader).context("read the welcome")? {
            Some(FromHolder::Welcome(welcome)) => welcome,
            Some(FromHolder::Unsupported { major, .. }) => {
                bail!("holder {} speaks protocol {major}", socket.display())
            }
            _ => bail!("holder {} did not welcome us", socket.display()),
        };

        let (out, queued) = mpsc::channel::<ToHolder>();
        thread::Builder::new()
            .name(format!("twyx-holder-out-{pane}"))
            .spawn(move || {
                for message in queued {
                    if wire::write(&mut writer, &message).is_err() {
                        break;
                    }
                }
            })
            .context("start the holder writer")?;

        thread::Builder::new()
            .name(format!("twyx-holder-in-{pane}"))
            .spawn(move || {
                while let Ok(Some(message)) = wire::read::<FromHolder>(&mut reader) {
                    if events.send(Event::Holder { pane, message }).is_err() {
                        return;
                    }
                }
                let _ = events.send(Event::HolderGone { pane });
            })
            .context("start the holder reader")?;

        Ok((Self { out }, welcome))
    }

    fn send(&self, message: ToHolder) {
        let _ = self.out.send(message);
    }
}

#[derive(Debug)]
struct Pane {
    link: Option<HolderLink>,
    cols: u16,
    rows: u16,
    state: PaneState,
    /// The last complete screen, for a client that attaches after the exit.
    screen: Option<Vec<u8>>,
}

#[derive(Debug)]
struct Client {
    out: Sender<FromDaemon>,
    cols: u16,
    rows: u16,
}

#[derive(Debug)]
pub struct Daemon {
    paths: Paths,
    build: String,
    holder_binary: PathBuf,
    events: Sender<Event>,
    panes: BTreeMap<PaneId, Pane>,
    clients: BTreeMap<u64, Client>,
    lease: Option<u64>,
    focused: Option<PaneId>,
    next_pane: u64,
}

/// Starts the daemon and serves it until the socket closes.
pub fn run(paths: Paths) -> Result<()> {
    paths.prepare_runtime()?;
    let listener = bind(&paths.socket())?;
    let holder_binary = std::env::current_exe().context("find the twyx binary")?;
    let build = env!("CARGO_PKG_VERSION").to_owned();
    let (events, inbound) = mpsc::channel();

    let mut daemon = Daemon {
        paths,
        build,
        holder_binary,
        events: events.clone(),
        panes: BTreeMap::new(),
        clients: BTreeMap::new(),
        lease: None,
        focused: None,
        next_pane: 1,
    };
    daemon.adopt();
    spawn_acceptor(listener, events)?;
    daemon.serve(&inbound)
}

fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        // A live daemon keeps its socket; a dead one leaves a file behind.
        if UnixStream::connect(path).is_ok() {
            bail!("a daemon is already listening on {}", path.display());
        }
        std::fs::remove_file(path)
            .with_context(|| format!("remove the stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))
}

impl Daemon {
    /// Takes over every holder that outlived a daemon.
    fn adopt(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.paths.panes_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(pane) = stem.parse::<PaneId>() else {
                continue;
            };
            if self.panes.contains_key(&pane) {
                continue;
            }
            let Ok((link, welcome)) =
                HolderLink::connect(path.clone(), pane, self.build.clone(), self.events.clone())
            else {
                // A holder that will not answer leaves a stale socket.
                let _ = std::fs::remove_file(&path);
                continue;
            };
            let (cols, rows) = screen_size(&welcome.snapshot);
            self.panes.insert(
                pane,
                Pane {
                    link: Some(link),
                    cols,
                    rows,
                    state: PaneState::Running,
                    screen: Some(welcome.snapshot),
                },
            );
            self.next_pane = self.next_pane.max(pane.raw() + 1);
        }
    }

    fn serve(&mut self, inbound: &Receiver<Event>) -> Result<()> {
        while let Ok(event) = inbound.recv() {
            match event {
                Event::ClientHello { client, hello, out } => self.client_hello(client, hello, out),
                Event::ClientMessage { client, message } => {
                    if message == ToDaemon::Shutdown {
                        return Ok(());
                    }
                    self.client_message(client, message)?;
                }
                Event::ClientGone { client } => self.client_gone(client),
                Event::Holder { pane, message } => self.holder_message(pane, message),
                Event::HolderGone { pane } => {
                    let mut lost = false;
                    if let Some(entry) = self.panes.get_mut(&pane)
                        && entry.state == PaneState::Running
                    {
                        entry.state = PaneState::Lost;
                        entry.link = None;
                        lost = true;
                    }
                    if lost {
                        self.broadcast_pane(pane);
                    }
                }
            }
        }
        Ok(())
    }

    fn client_hello(&mut self, id: u64, hello: ClientHello, out: Sender<FromDaemon>) {
        let panes = self.pane_infos();
        let first = self.clients.is_empty();
        self.clients.insert(
            id,
            Client {
                out: out.clone(),
                cols: hello.cols,
                rows: hello.rows,
            },
        );

        let _ = out.send(FromDaemon::Welcome(ServerHello {
            major: MAJOR,
            minor: MINOR,
            build: self.build.clone(),
            pid: std::process::id(),
            client: id,
            panes,
            lease: self.lease,
        }));

        if first {
            self.take_lease(id);
        }

        for (pane, entry) in &self.panes {
            if let Some(screen) = &entry.screen {
                let _ = out.send(FromDaemon::Snapshot {
                    pane: pane.raw(),
                    seq: 0,
                    body: screen.clone(),
                });
            }
            if let Some(link) = &entry.link {
                link.send(ToHolder::Snapshot);
            }
        }
    }

    fn client_message(&mut self, id: u64, message: ToDaemon) -> Result<()> {
        match message {
            ToDaemon::Hello(_) => {}
            ToDaemon::Resize { cols, rows } => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.cols = cols;
                    client.rows = rows;
                }
                if self.lease == Some(id) {
                    self.apply_size(id);
                }
            }
            ToDaemon::Input(event) => {
                // Input from an interactive client takes the lease before the
                // input reaches the pane.
                if self.lease != Some(id) {
                    self.take_lease(id);
                }
                if let Some(pane) = self.focused
                    && let Some(entry) = self.panes.get(&pane)
                    && let Some(link) = &entry.link
                    && entry.state == PaneState::Running
                {
                    link.send(ToHolder::Input(event));
                }
            }
            ToDaemon::TakeLease => self.take_lease(id),
            ToDaemon::Snapshot { pane } => {
                if let Some(entry) = self.panes.get(&PaneId::from_raw(pane)) {
                    match &entry.link {
                        Some(link) => link.send(ToHolder::Snapshot),
                        None => {
                            if let Some(screen) = &entry.screen
                                && let Some(client) = self.clients.get(&id)
                            {
                                let _ = client.out.send(FromDaemon::Snapshot {
                                    pane,
                                    seq: 0,
                                    body: screen.clone(),
                                });
                            }
                        }
                    }
                }
            }
            ToDaemon::NewPane {
                argv,
                cwd,
                cols,
                rows,
            } => {
                self.new_pane(argv, cwd, cols, rows)?;
            }
            ToDaemon::Shutdown => {}
            ToDaemon::KillPane { pane } => {
                if let Some(entry) = self.panes.get(&PaneId::from_raw(pane))
                    && let Some(link) = &entry.link
                {
                    link.send(ToHolder::Kill);
                }
            }
        }
        Ok(())
    }

    fn client_gone(&mut self, id: u64) {
        self.clients.remove(&id);
        if self.lease == Some(id) {
            self.lease = None;
            // A disconnect releases the lease to the next client in line.
            if let Some((next, _)) = self.clients.iter().next() {
                let next = *next;
                self.take_lease(next);
            }
        }
        self.broadcast(FromDaemon::Lease { holder: self.lease });
    }

    fn take_lease(&mut self, id: u64) {
        if !self.clients.contains_key(&id) {
            return;
        }
        self.lease = Some(id);
        self.apply_size(id);
        self.broadcast(FromDaemon::Lease { holder: self.lease });
    }

    fn apply_size(&mut self, id: u64) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        let (cols, rows) = (client.cols, client.rows);
        if cols == 0 || rows == 0 {
            return;
        }
        let mut resized = Vec::new();
        for (pane, entry) in self.panes.iter_mut() {
            if entry.cols == cols && entry.rows == rows {
                continue;
            }
            entry.cols = cols;
            entry.rows = rows;
            if let Some(link) = &entry.link {
                link.send(ToHolder::Resize { cols, rows });
            }
            resized.push(*pane);
        }
        for pane in resized {
            self.broadcast_pane(pane);
        }
    }

    fn holder_message(&mut self, pane: PaneId, message: FromHolder) {
        match message {
            FromHolder::Welcome(welcome) => {
                if welcome.pane != pane.raw() {
                    return;
                }
                let (cols, rows) = screen_size(&welcome.snapshot);
                if let Some(entry) = self.panes.get_mut(&pane) {
                    entry.cols = cols;
                    entry.rows = rows;
                    entry.state = PaneState::Running;
                    entry.screen = Some(welcome.snapshot.clone());
                }
                self.broadcast(FromDaemon::Snapshot {
                    pane: pane.raw(),
                    seq: 0,
                    body: welcome.snapshot,
                });
                self.broadcast_pane(pane);
            }
            FromHolder::Frame { seq, body } => {
                self.broadcast(FromDaemon::Frame {
                    pane: pane.raw(),
                    seq,
                    body,
                });
            }
            FromHolder::Snapshot { seq, body } => {
                if let Some(entry) = self.panes.get_mut(&pane) {
                    entry.screen = Some(body.clone());
                }
                self.broadcast(FromDaemon::Snapshot {
                    pane: pane.raw(),
                    seq,
                    body,
                });
            }
            FromHolder::Signal(signal) => {
                self.signal(pane, signal);
            }
            FromHolder::Exited(state) => {
                let state = match state {
                    ExitState::Exited { code } => PaneState::Exited { code },
                    ExitState::Killed { signal } => PaneState::Killed { signal },
                };
                if let Some(entry) = self.panes.get_mut(&pane) {
                    entry.state = state;
                }
                // The daemon has the exit status, so the holder may go.
                if let Some(entry) = self.panes.get(&pane)
                    && let Some(link) = &entry.link
                {
                    link.send(ToHolder::AckExit);
                }
                self.broadcast_pane(pane);
            }
            FromHolder::Unsupported { .. } | FromHolder::Pong => {}
        }
    }

    fn signal(&mut self, pane: PaneId, signal: twyx_protocol::holder::Signal) {
        match signal {
            twyx_protocol::holder::Signal::ClipboardWrite(text) => {
                for client in self.clients.values() {
                    let _ = client.out.send(FromDaemon::Clipboard {
                        pane: pane.raw(),
                        text: text.clone(),
                    });
                }
            }
            twyx_protocol::holder::Signal::Bell
            | twyx_protocol::holder::Signal::Notification { .. }
            | twyx_protocol::holder::Signal::PromptMark(_)
            | twyx_protocol::holder::Signal::WorkingDirectory(_)
            | twyx_protocol::holder::Signal::Link(_)
            | twyx_protocol::holder::Signal::Title(_)
            | twyx_protocol::holder::Signal::Foreground { .. } => {}
        }
    }

    fn new_pane(&mut self, argv: Vec<String>, cwd: String, cols: u16, rows: u16) -> Result<()> {
        if argv.is_empty() {
            bail!("a pane needs a command");
        }
        let pane = PaneId::from_raw(self.next_pane);
        self.next_pane += 1;
        let socket = self.paths.pane_socket(pane);
        let scrollback_lines = twyx_holder::session::lines(tuning::PANE_SCROLLBACK_BYTES, cols);

        let mut command = Command::new(&self.holder_binary);
        command
            .arg("holder")
            .arg("--pane")
            .arg(pane.to_string())
            .arg("--socket")
            .arg(&socket)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .arg("--scrollback-lines")
            .arg(scrollback_lines.to_string())
            .arg("--cwd")
            .arg(cwd)
            .arg("--")
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn()
            .with_context(|| format!("start the holder for pane {pane}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let (link, welcome) = loop {
            match HolderLink::connect(
                socket.clone(),
                pane,
                self.build.clone(),
                self.events.clone(),
            ) {
                Ok(connected) => break connected,
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
        };

        let (cols, rows) = screen_size(&welcome.snapshot);
        self.panes.insert(
            pane,
            Pane {
                link: Some(link),
                cols,
                rows,
                state: PaneState::Running,
                screen: Some(welcome.snapshot.clone()),
            },
        );
        if self.focused.is_none() {
            self.focused = Some(pane);
        }
        self.broadcast(FromDaemon::Snapshot {
            pane: pane.raw(),
            seq: 0,
            body: welcome.snapshot,
        });
        self.broadcast_pane(pane);
        Ok(())
    }

    fn pane_infos(&self) -> Vec<PaneInfo> {
        self.panes
            .iter()
            .map(|(pane, entry)| PaneInfo {
                pane: pane.raw(),
                cols: entry.cols,
                rows: entry.rows,
                state: entry.state.clone(),
            })
            .collect()
    }

    fn broadcast_pane(&self, pane: PaneId) {
        let Some(entry) = self.panes.get(&pane) else {
            return;
        };
        let info = PaneInfo {
            pane: pane.raw(),
            cols: entry.cols,
            rows: entry.rows,
            state: entry.state.clone(),
        };
        self.broadcast(FromDaemon::Pane(info));
    }

    fn broadcast(&self, message: FromDaemon) {
        for client in self.clients.values() {
            let _ = client.out.send(message.clone());
        }
    }
}

fn screen_size(snapshot: &[u8]) -> (u16, u16) {
    match wire::decode_body::<twyx_protocol::grid::Snapshot>(snapshot) {
        Ok(snapshot) => (snapshot.screen.cols, snapshot.screen.rows),
        Err(_) => (80, 24),
    }
}

fn spawn_acceptor(listener: UnixListener, events: Sender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("twyx-daemon-accept".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let events = events.clone();
                // Each client needs its own reader, or a quiet client blocks
                // every other one.
                thread::spawn(move || serve_client(stream, events));
            }
        })
        .context("start the daemon acceptor")?;
    Ok(())
}

fn serve_client(stream: UnixStream, events: Sender<Event>) {
    if peer::accept(&stream).is_err() {
        return;
    }
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let hello = match wire::read::<ToDaemon>(&mut reader) {
        Ok(Some(ToDaemon::Hello(hello))) => hello,
        _ => return,
    };
    if hello.major != MAJOR {
        let mut writer = BufWriter::new(stream);
        let _ = wire::write(
            &mut writer,
            &FromDaemon::Unsupported {
                major: MAJOR,
                minor: MINOR,
            },
        );
        return;
    }

    let client = CLIENTS.fetch_add(1, Ordering::Relaxed);
    let (out, queued) = mpsc::channel::<FromDaemon>();
    let mut writer = BufWriter::new(stream);
    thread::spawn(move || {
        for message in queued {
            if wire::write(&mut writer, &message).is_err() {
                break;
            }
        }
    });

    if events
        .send(Event::ClientHello {
            client,
            hello,
            out: out.clone(),
        })
        .is_err()
    {
        return;
    }

    while let Ok(Some(message)) = wire::read::<ToDaemon>(&mut reader) {
        if events
            .send(Event::ClientMessage { client, message })
            .is_err()
        {
            return;
        }
    }
    let _ = events.send(Event::ClientGone { client });
}
