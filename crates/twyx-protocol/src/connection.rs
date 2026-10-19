//! One connection to the daemon, as a client.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::client::{ClientHello, FromDaemon, MAJOR, MINOR, ServerHello, ToDaemon};
use crate::wire;

/// A connected client, with the daemon's greeting.
#[derive(Debug)]
pub struct Connection {
    pub reader: BufReader<UnixStream>,
    pub writer: UnixStream,
    pub hello: ServerHello,
}

/// A connection whose two directions run on their own threads.
#[derive(Debug)]
pub struct Parts {
    pub incoming: Receiver<FromDaemon>,
    pub outgoing: Sender<ToDaemon>,
    pub hello: ServerHello,
}

/// Connects as a client. An interactive client is a TUI or desktop client;
/// a CLI command is not.
pub fn connect(socket: &Path, cols: u16, rows: u16, interactive: bool) -> Result<Connection> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to the daemon on {}", socket.display()))?;
    let reader = BufReader::new(stream.try_clone().context("clone the socket")?);
    let mut connection = Connection {
        reader,
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
    };
    connection.send(&ToDaemon::Hello(ClientHello {
        major: MAJOR,
        minor: MINOR,
        build: crate::BUILD_ID.to_owned(),
        cols,
        rows,
        interactive,
    }))?;
    match connection.receive()? {
        FromDaemon::Welcome(hello) => {
            connection.hello = hello;
            Ok(connection)
        }
        FromDaemon::Unsupported { major, minor, pid } => {
            Err(Unsupported { major, minor, pid }.into())
        }
        other => bail!("the daemon greeted with {other:?}"),
    }
}

/// The `twyx` binary, which carries the daemon. A client binary sits next to
/// it, and `TWYX_BINARY` overrides the search.
fn daemon_binary() -> Result<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("TWYX_BINARY") {
        return Ok(path.into());
    }
    let current = std::env::current_exe().context("find the twyx binary")?;
    if let Some(directory) = current.parent() {
        let sibling = directory.join("twyx");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    Ok(current)
}

/// Waits for a daemon to answer, starting one first when there is none.
/// A daemon that speaks another major of the client protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    pub major: u16,
    pub minor: u16,
    pub pid: u32,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the daemon (pid {}) speaks client protocol {}.{}",
            self.pid, self.major, self.minor
        )
    }
}

impl std::error::Error for Unsupported {}

/// Connects to this build's daemon. A daemon from another build is stopped
/// and a new one started, so the panes it adopts keep running: at once when
/// no other interactive client is attached, or when it cannot serve this
/// client at all, and otherwise only after `ask` agrees. `ask` gets the
/// number of other clients; when it declines, the client keeps the daemon
/// it found.
pub fn attach(
    paths: &twyx_core::paths::Paths,
    cols: u16,
    rows: u16,
    interactive: bool,
    ask: impl FnOnce(u32) -> bool,
) -> Result<Connection> {
    ensure_daemon(paths)?;
    let pid = match connect(&paths.socket(), cols, rows, interactive) {
        Ok(connection) if connection.hello.build == crate::BUILD_ID => return Ok(connection),
        Ok(connection) => {
            let others = connection.hello.clients;
            if others > 0 && !ask(others) {
                return Ok(connection);
            }
            let pid = connection.hello.pid;
            let mut connection = connection;
            let _ = connection.send(&ToDaemon::Shutdown);
            pid
        }
        Err(error) => match error.downcast_ref::<Unsupported>() {
            Some(unsupported) => unsupported.pid,
            None => return Err(error),
        },
    };
    stop(paths, pid)?;
    ensure_daemon(paths)?;
    connect(&paths.socket(), cols, rows, interactive)
}

/// Waits for a daemon to go, and ends it if it does not go by itself.
fn stop(paths: &twyx_core::paths::Paths, pid: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while UnixStream::connect(paths.socket()).is_ok() {
        if Instant::now() >= deadline {
            if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // A daemon that was killed leaves its socket; the next one replaces it.
    Ok(())
}

pub fn ensure_daemon(paths: &twyx_core::paths::Paths) -> Result<()> {
    if UnixStream::connect(paths.socket()).is_ok() {
        return Ok(());
    }
    let binary = daemon_binary()?;
    std::process::Command::new(binary)
        .arg("daemon")
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start the daemon")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if UnixStream::connect(paths.socket()).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("the daemon did not answer on {}", paths.socket().display())
}

impl Connection {
    pub fn send(&mut self, message: &ToDaemon) -> Result<()> {
        wire::write(&mut self.writer, message)
    }

    pub fn receive(&mut self) -> Result<FromDaemon> {
        match wire::read::<FromDaemon>(&mut self.reader)? {
            Some(message) => Ok(message),
            None => bail!("the daemon closed the connection"),
        }
    }

    /// Runs each direction on its own thread, so the caller never blocks on
    /// the socket while it waits for a key.
    pub fn split(self) -> Parts {
        let (outgoing, outbound) = channel::<ToDaemon>();
        let (inbound, incoming) = channel::<FromDaemon>();

        let mut writer = self.writer;
        std::thread::spawn(move || {
            for message in outbound {
                if wire::write(&mut writer, &message).is_err() {
                    break;
                }
            }
        });

        let mut reader = self.reader;
        std::thread::spawn(move || {
            while let Ok(Some(message)) = wire::read::<FromDaemon>(&mut reader) {
                if inbound.send(message).is_err() {
                    break;
                }
            }
        });

        Parts {
            incoming,
            outgoing,
            hello: self.hello,
        }
    }
}
