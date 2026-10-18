//! One connection to the daemon, as a client.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use twyx_protocol::client::{ClientHello, FromDaemon, MAJOR, MINOR, ServerHello, ToDaemon};
use twyx_protocol::wire;

/// A connected client, with the daemon's greeting.
pub struct Connection {
    pub reader: BufReader<UnixStream>,
    pub writer: UnixStream,
    pub hello: ServerHello,
}

pub fn connect(paths: &twyx_daemon::paths::Paths, cols: u16, rows: u16) -> Result<Connection> {
    connect_to(&paths.socket(), cols, rows)
}

pub fn connect_to(socket: &Path, cols: u16, rows: u16) -> Result<Connection> {
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
            lease: None,
        },
    };
    connection.send(&ToDaemon::Hello(ClientHello {
        major: MAJOR,
        minor: MINOR,
        build: env!("CARGO_PKG_VERSION").to_owned(),
        cols,
        rows,
    }))?;
    match connection.receive()? {
        FromDaemon::Welcome(hello) => {
            connection.hello = hello;
            Ok(connection)
        }
        FromDaemon::Unsupported { major, minor } => {
            bail!("the daemon speaks client protocol {major}.{minor}")
        }
        other => bail!("the daemon greeted with {other:?}"),
    }
}

/// Waits for a daemon to answer, starting one first when there is none.
pub fn ensure_daemon(paths: &twyx_daemon::paths::Paths) -> Result<()> {
    if UnixStream::connect(paths.socket()).is_ok() {
        return Ok(());
    }
    let binary = std::env::current_exe().context("find the twyx binary")?;
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
}
