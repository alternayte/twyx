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
/// Connects to the daemon. An interactive client shows `workspace` when it
/// still exists, and else the one a new client shows.
pub fn connect(
    socket: &Path,
    cols: u16,
    rows: u16,
    interactive: bool,
    workspace: Option<u64>,
) -> Result<Connection> {
    let stream = crate::peer::dial(socket)
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
        workspace,
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
    binary_beside(&std::env::current_exe().context("find the twyx binary")?)
}

fn binary_beside(current: &Path) -> Result<std::path::PathBuf> {
    if let Some(directory) = current.parent() {
        let sibling = directory.join("twyx");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    // Another client binary would start itself, not a daemon.
    if current.file_name() == Some(std::ffi::OsStr::new("twyx")) {
        return Ok(current.to_path_buf());
    }
    bail!(
        "no twyx binary next to {}; build it with cargo build -p twyx, or set TWYX_BINARY",
        current.display()
    )
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
    workspace: Option<u64>,
    ask: impl FnOnce(u32) -> bool,
) -> Result<Connection> {
    ensure_daemon(paths)?;
    let pid = match connect(&paths.socket(), cols, rows, interactive, workspace) {
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
    connect(&paths.socket(), cols, rows, interactive, workspace)
}

/// An API v1 client of this build's daemon. A daemon from another build
/// is replaced when no interactive client is attached, and kept otherwise.
pub fn attach_api(paths: &twyx_core::paths::Paths) -> Result<crate::api::Client> {
    ensure_daemon(paths)?;
    let mut api = match crate::api::Client::connect(&paths.socket()) {
        Ok(api) => api,
        // A daemon from before API v1: replace it as a command client would.
        Err(_) => {
            drop(attach(paths, 80, 24, false, None, |_| false)?);
            crate::api::Client::connect(&paths.socket()).context(
                "the daemon is from another build without API v1; stop it with twyx daemon stop",
            )?
        }
    };
    if api.welcome.build == crate::BUILD_ID || api.welcome.clients > 0 {
        return Ok(api);
    }
    let pid = api.welcome.pid;
    // The daemon closes the connection as it stops.
    let _ = api.call::<serde_json::Value>("daemon.stop", &serde_json::Value::Null);
    stop(paths, pid)?;
    ensure_daemon(paths)?;
    crate::api::Client::connect(&paths.socket())
}

/// Waits for a daemon to go, and ends it if it does not go by itself.
fn stop(paths: &twyx_core::paths::Paths, pid: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while crate::peer::dial(&paths.socket()).is_ok() {
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
    // A runtime dir someone else made, or left open, is never used.
    if std::fs::symlink_metadata(&paths.runtime).is_ok() {
        paths.check_runtime()?;
    }
    if crate::peer::dial(&paths.socket()).is_ok() {
        return Ok(());
    }
    let binary = daemon_binary()?;
    twyx_core::launch::daemon(&binary)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("start the daemon")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if crate::peer::dial(&paths.socket()).is_ok() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_without_the_twyx_binary_beside_it_never_starts_itself_as_the_daemon() {
        let directory = std::env::temp_dir().join(format!("twyx-binary-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let desktop = directory.join("twyx-desktop");
        std::fs::write(&desktop, "").unwrap();
        assert!(
            binary_beside(&desktop).is_err(),
            "the desktop binary would open a window for each start"
        );
        let twyx = directory.join("twyx");
        std::fs::write(&twyx, "").unwrap();
        assert_eq!(binary_beside(&desktop).unwrap(), twyx);
        assert_eq!(binary_beside(&twyx).unwrap(), twyx);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
