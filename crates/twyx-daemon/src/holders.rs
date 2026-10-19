//! Holders: starting one, connecting to one, and relaying its messages.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::holder::{FromHolder, Hello, ToHolder, Welcome};
use twyx_protocol::wire;

use crate::daemon::Event;

pub(crate) struct HolderLink {
    out: Sender<ToHolder>,
}

impl std::fmt::Debug for HolderLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HolderLink")
    }
}

impl HolderLink {
    pub(crate) fn connect(
        socket: PathBuf,
        pane: PaneId,
        build: String,
        events: Sender<Event>,
    ) -> Result<(Self, Welcome)> {
        let stream = UnixStream::connect(&socket)
            .with_context(|| format!("connect to {}", socket.display()))?;
        // Read now: a holder that turns us away may close the socket.
        let peer = crate::peer::pid(&stream);
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
                return Err(Legacy { major, pid: peer }.into());
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

    pub(crate) fn send(&self, message: ToHolder) {
        let _ = self.out.send(message);
    }
}

/// A holder of a protocol major this daemon cannot adopt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Legacy {
    pub major: u16,
    pub pid: Option<u32>,
}

impl std::fmt::Display for Legacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a holder of protocol {}", self.major)
    }
}

impl std::error::Error for Legacy {}

/// What a new holder runs, and where.
#[derive(Debug)]
pub(crate) struct Start<'a> {
    pub binary: &'a Path,
    pub socket: PathBuf,
    pub pane: PaneId,
    pub argv: &'a [String],
    pub cwd: &'a str,
    pub cols: u16,
    pub rows: u16,
}

/// Starts a holder and connects to it once it listens.
pub(crate) fn start(
    start: Start<'_>,
    build: &str,
    events: &Sender<Event>,
) -> Result<(HolderLink, Welcome)> {
    if start.argv.is_empty() {
        bail!("a pane needs a command");
    }
    let scrollback_lines = twyx_holder::session::lines(tuning::PANE_SCROLLBACK_BYTES, start.cols);
    Command::new(start.binary)
        .arg("holder")
        .arg("--pane")
        .arg(start.pane.to_string())
        .arg("--socket")
        .arg(&start.socket)
        .arg("--cols")
        .arg(start.cols.to_string())
        .arg("--rows")
        .arg(start.rows.to_string())
        .arg("--scrollback-lines")
        .arg(scrollback_lines.to_string())
        .arg("--cwd")
        .arg(start.cwd)
        .arg("--")
        .args(start.argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start the holder for pane {}", start.pane))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match HolderLink::connect(
            start.socket.clone(),
            start.pane,
            build.to_owned(),
            events.clone(),
        ) {
            Ok(connected) => return Ok(connected),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

pub(crate) fn screen_size(snapshot: &[u8]) -> (u16, u16) {
    match wire::decode_body::<twyx_protocol::grid::Snapshot>(snapshot) {
        Ok(snapshot) => (snapshot.screen.cols, snapshot.screen.rows),
        Err(_) => (80, 24),
    }
}
