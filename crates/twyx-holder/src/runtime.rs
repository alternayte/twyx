//! The holder process: one pane, from spawn until its exit is acknowledged.
//!
//! Threads: the reader pulls PTY output, the waiter collects the exit status,
//! the acceptor serves the daemon's socket, and this thread owns the engine.

use std::io::{BufReader, BufWriter, Read};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use portable_pty::{Child, ChildKiller};

use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::holder::{
    ExitState, FromHolder, Hello, MAJOR, MINOR, Signal, ToHolder, Welcome,
};
use twyx_protocol::wire;

use crate::encode;
use crate::engine::{Engine, EngineSignal};
use crate::frame::FrameBuilder;
use crate::pty::{Master, Pty, Spawn};
use crate::queue::InputQueue;

#[derive(Debug, Clone)]
pub struct Config {
    pub socket: PathBuf,
    pub pane: PaneId,
    pub build: String,
    pub spawn: Spawn,
    pub scrollback_lines: u32,
    pub exit_grace: std::time::Duration,
}

enum Event {
    Output(Vec<u8>),
    Exit(ExitState),
    Attach(Hello, Sender<FromHolder>),
    Command(ToHolder, Sender<FromHolder>),
}

pub fn run(config: Config, engine: Box<dyn Engine>) -> Result<()> {
    let listener = listen(&config.socket)?;
    let (events, inbound) = mpsc::channel::<Event>();

    let pty = Pty::spawn(&config.spawn)?;
    let (master, child) = pty.split();
    let killer = crate::pty::killer(child.as_ref());

    let input = InputQueue::start(master.writer()?)?;

    spawn_reader(master.reader()?, events.clone())?;
    spawn_waiter(child, events.clone())?;
    spawn_acceptor(listener, events)?;

    Core {
        config: &config,
        engine,
        master: &master,
        input: &input,
        killer,
        builder: FrameBuilder::new(config.spawn.cols, config.spawn.rows),
    }
    .run(&inbound)
}

struct Core<'a> {
    config: &'a Config,
    engine: Box<dyn Engine>,
    master: &'a Master,
    input: &'a InputQueue,
    killer: Box<dyn ChildKiller + Send + Sync>,
    builder: FrameBuilder,
}

impl Core<'_> {
    fn run(mut self, inbound: &Receiver<Event>) -> Result<()> {
        let mut out: Option<Sender<FromHolder>> = None;
        let mut dirty = true;
        let mut last_frame = Instant::now();
        let mut exited: Option<ExitState> = None;
        let mut deadline: Option<Instant> = None;

        loop {
            let mut events = Vec::new();
            match inbound.recv_timeout(tuning::FRAME_INTERVAL) {
                Ok(event) => events.push(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(event) = inbound.try_recv() {
                events.push(event);
            }

            for event in events {
                match event {
                    Event::Output(bytes) => {
                        if exited.is_some() {
                            continue;
                        }
                        self.output(&bytes, &mut out);
                        dirty = true;
                    }
                    Event::Exit(state) => {
                        if exited.is_none() {
                            send(&mut out, FromHolder::Exited(state.clone()));
                            exited = Some(state);
                            deadline = Some(Instant::now() + self.config.exit_grace);
                            dirty = true;
                        }
                    }
                    Event::Attach(hello, sender) => {
                        if hello.major != MAJOR {
                            let _ = sender.send(FromHolder::Unsupported {
                                major: MAJOR,
                                minor: MINOR,
                            });
                            continue;
                        }
                        self.builder.reset_consumer();
                        let view = self.engine.view();
                        let snapshot = self.builder.snapshot(&view);
                        let welcome = Welcome {
                            major: MAJOR,
                            minor: MINOR,
                            pane: self.config.pane.raw(),
                            build: self.config.build.clone(),
                            snapshot: wire::encode_body(&snapshot)?,
                        };
                        let _ = sender.send(FromHolder::Welcome(welcome));
                        out = Some(sender);
                        // A pane that ended before the daemon attached still
                        // owes it the exit status.
                        if let Some(state) = &exited {
                            send(&mut out, FromHolder::Exited(state.clone()));
                        }
                        dirty = true;
                        last_frame = Instant::now() - tuning::FRAME_INTERVAL;
                    }
                    Event::Command(command, sender) => {
                        if out.is_none() {
                            out = Some(sender);
                        }
                        if let Some(result) = self.command(command, &mut out, &mut dirty, &exited) {
                            return result;
                        }
                    }
                }
            }

            if dirty && last_frame.elapsed() >= tuning::FRAME_INTERVAL {
                self.publish_frame(&mut out);
                dirty = false;
                last_frame = Instant::now();
            }

            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                return Ok(());
            }
        }

        Ok(())
    }

    fn output(&mut self, bytes: &[u8], out: &mut Option<Sender<FromHolder>>) {
        for signal in self.engine.advance(bytes) {
            match signal {
                EngineSignal::WriteToPane(bytes) => self.input.send(bytes),
                EngineSignal::Title(title) => {
                    send(out, FromHolder::Signal(Signal::Title(trim(&title))));
                }
                EngineSignal::Bell => send(out, FromHolder::Signal(Signal::Bell)),
                EngineSignal::ClipboardWrite(text) => {
                    send(out, FromHolder::Signal(Signal::ClipboardWrite(trim(&text))));
                }
            }
        }
    }

    fn command(
        &mut self,
        command: ToHolder,
        out: &mut Option<Sender<FromHolder>>,
        dirty: &mut bool,
        exited: &Option<ExitState>,
    ) -> Option<Result<()>> {
        match command {
            ToHolder::Hello(hello) => {
                if hello.major != MAJOR {
                    send(
                        out,
                        FromHolder::Unsupported {
                            major: MAJOR,
                            minor: MINOR,
                        },
                    );
                }
            }
            ToHolder::Input(event) => {
                if exited.is_none() {
                    let mut bytes = Vec::new();
                    encode::encode(&event, &self.engine.modes(), &mut bytes);
                    self.input.send(bytes);
                }
            }
            ToHolder::Resize { cols, rows } => {
                self.engine.resize(cols, rows);
                if let Err(error) = self.master.resize(cols, rows) {
                    return Some(Err(error));
                }
                *dirty = true;
            }
            ToHolder::Snapshot => {
                self.builder.reset_consumer();
                let view = self.engine.view();
                let snapshot = self.builder.snapshot(&view);
                match wire::encode_body(&snapshot) {
                    Ok(body) => send(
                        out,
                        FromHolder::Snapshot {
                            seq: snapshot.seq,
                            body,
                        },
                    ),
                    Err(error) => return Some(Err(error)),
                }
            }
            ToHolder::AckExit => {
                if exited.is_some() {
                    return Some(Ok(()));
                }
            }
            ToHolder::Kill => {
                let _ = self.killer.kill();
            }
            ToHolder::Ping => send(out, FromHolder::Pong),
        }
        None
    }

    fn publish_frame(&mut self, out: &mut Option<Sender<FromHolder>>) {
        let view = self.engine.view();
        let frame = self.builder.frame(&view);
        let Some(sender) = out else {
            return;
        };
        let Ok(body) = wire::encode_body(&frame) else {
            return;
        };
        if sender
            .send(FromHolder::Frame {
                seq: frame.seq,
                body,
            })
            .is_err()
        {
            *out = None;
        }
    }
}

/// A build stamp, so a client can tell a stale holder from a fresh one.
pub fn build_id() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

fn send(out: &mut Option<Sender<FromHolder>>, message: FromHolder) {
    if let Some(sender) = out
        && sender.send(message).is_err()
    {
        *out = None;
    }
}

fn trim(text: &str) -> String {
    text.chars().take(tuning::TITLE_CHARS).collect()
}

fn listen(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove the stale socket {}", path.display()));
        }
    }
    UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, events: Sender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("twyx-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if events
                            .send(Event::Output(buffer[..count].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    // EIO once the child's side of the pty is gone.
                    Err(_) => break,
                }
            }
        })
        .context("start the pty reader")?;
    Ok(())
}

fn spawn_waiter(mut child: Box<dyn Child + Send + Sync>, events: Sender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("twyx-pty-waiter".to_owned())
        .spawn(move || {
            let state = match child.wait() {
                Ok(status) => match status.signal() {
                    Some(signal) => ExitState::Killed {
                        signal: signal.to_owned(),
                    },
                    None => ExitState::Exited {
                        code: status.exit_code(),
                    },
                },
                Err(error) => ExitState::Killed {
                    signal: format!("wait failed: {error}"),
                },
            };
            let _ = events.send(Event::Exit(state));
        })
        .context("start the pty waiter")?;
    Ok(())
}

fn spawn_acceptor(listener: UnixListener, events: Sender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("twyx-holder-socket".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let events = events.clone();
                thread::spawn(move || serve_daemon(stream, events));
            }
        })
        .context("start the holder socket")?;
    Ok(())
}

fn serve_daemon(stream: std::os::unix::net::UnixStream, events: Sender<Event>) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let hello = match wire::read::<ToHolder>(&mut reader) {
        Ok(Some(ToHolder::Hello(hello))) => hello,
        _ => return,
    };
    if hello.major != MAJOR {
        let mut writer = BufWriter::new(stream);
        let _ = wire::write(
            &mut writer,
            &FromHolder::Unsupported {
                major: MAJOR,
                minor: MINOR,
            },
        );
        return;
    }

    let (out, queued) = mpsc::channel::<FromHolder>();
    let mut writer = BufWriter::new(stream);
    thread::spawn(move || {
        for message in queued {
            if wire::write(&mut writer, &message).is_err() {
                break;
            }
        }
    });

    if events.send(Event::Attach(hello, out.clone())).is_err() {
        return;
    }

    // A daemon that goes away leaves the pane running.
    while let Ok(Some(command)) = wire::read::<ToHolder>(&mut reader) {
        if events.send(Event::Command(command, out.clone())).is_err() {
            return;
        }
    }
}
