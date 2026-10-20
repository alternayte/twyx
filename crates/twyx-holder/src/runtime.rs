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

use twyx_core::agent::AgentKind;
use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::holder::{
    AgentRules, ExitState, FromHolder, Hello, MAJOR, MINOR, ScreenMatch, Signal, ToHolder, Welcome,
};
use twyx_protocol::wire;

use crate::encode;
use crate::engine::{Engine, EngineSignal};
use crate::frame::FrameBuilder;
use crate::history::History;
use crate::pty::{Master, Pty, Spawn};
use crate::queue::{InputQueue, QueueWriter};
use crate::signals::{PaneSignal, Scanner};

#[derive(Debug, Clone)]
pub struct Config {
    pub socket: PathBuf,
    pub pane: PaneId,
    pub build: String,
    pub spawn: Spawn,
    pub scrollback_lines: u32,
    pub exit_grace: std::time::Duration,
    /// Where the pane's history goes; `None` writes none.
    pub history: Option<PathBuf>,
}

enum Event {
    Output(Vec<u8>),
    Exit(ExitState),
    Attach(Hello, Sender<FromHolder>),
    Command(ToHolder, Sender<FromHolder>),
}

pub fn run(
    config: Config,
    make_engine: impl FnOnce(Box<dyn std::io::Write + Send>) -> Result<Engine>,
) -> Result<()> {
    let listener = listen(&config.socket)?;
    let (events, inbound) = mpsc::channel::<Event>();

    let pty = Pty::spawn(&config.spawn)?;
    let (master, child) = pty.split();
    let killer = crate::pty::killer(child.as_ref());
    // The child leads its own session, so its pid is its process group.
    let group = child.process_id();

    let input = InputQueue::start(master.writer()?)?;
    let engine = make_engine(Box::new(QueueWriter::new(input.clone())))?;

    spawn_reader(master.reader()?, events.clone())?;
    spawn_waiter(child, events.clone())?;
    spawn_acceptor(listener, events)?;

    let history = match &config.history {
        Some(dir) => Some(History::start(dir.clone())?),
        None => None,
    };

    Core {
        config: &config,
        engine,
        history,
        master: &master,
        input: &input,
        killer,
        group,
        builder: FrameBuilder::new(config.spawn.cols, config.spawn.rows),
        scanner: Scanner::default(),
        watch: Watch::new(),
    }
    .run(&inbound)
}

struct Core<'a> {
    config: &'a Config,
    engine: Engine,
    history: Option<History>,
    master: &'a Master,
    input: &'a InputQueue,
    killer: Box<dyn ChildKiller + Send + Sync>,
    group: Option<u32>,
    builder: FrameBuilder,
    scanner: Scanner,
    watch: Watch,
}

/// What the holder watches for the daemon's agent state: the foreground
/// process, and the visible screen against the agent kind's rules.
struct Watch {
    foreground: Option<(u32, String)>,
    polled: Instant,
    agent: Option<AgentKind>,
    /// The match last reported.
    screen: Option<ScreenMatch>,
    /// The screen changed since it was last read.
    changed: bool,
    output: Instant,
    read: Instant,
}

impl Watch {
    fn new() -> Self {
        let long_ago = Instant::now() - tuning::FOREGROUND_POLL;
        Self {
            foreground: None,
            polled: long_ago,
            agent: None,
            screen: None,
            changed: false,
            output: Instant::now(),
            read: Instant::now(),
        }
    }
}

impl Core<'_> {
    fn run(mut self, inbound: &Receiver<Event>) -> Result<()> {
        let mut out: Option<Sender<FromHolder>> = None;
        let mut dirty = true;
        let mut last_frame = Instant::now();
        let mut exited: Option<ExitState> = None;
        let mut deadline: Option<Instant> = None;

        loop {
            // A pane that ended writes no more history; its log has it all.
            if exited.is_none() {
                self.checkpoint();
            }
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
                        self.resend_watch(&mut out);
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

            if exited.is_none() {
                self.poll_foreground(&mut out);
                self.read_screen(&mut out);
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

    /// Writes a checkpoint when one is due. Encoding happens here, on the
    /// engine's thread; the disk is the writer's.
    fn checkpoint(&mut self) {
        let Some(history) = &mut self.history else {
            return;
        };
        if history.due()
            && let Some(snapshot) = self.engine.encode_snapshot()
        {
            history.checkpoint(snapshot);
        }
    }

    /// A daemon that attaches learns what an earlier one was told.
    fn resend_watch(&mut self, out: &mut Option<Sender<FromHolder>>) {
        if let Some((pid, name)) = &self.watch.foreground {
            send(
                out,
                FromHolder::Signal(Signal::Foreground {
                    pid: *pid,
                    name: name.clone(),
                }),
            );
        }
        if self.watch.screen.is_some() {
            send(
                out,
                FromHolder::Signal(Signal::Screen(self.watch.screen.clone())),
            );
        }
    }

    fn poll_foreground(&mut self, out: &mut Option<Sender<FromHolder>>) {
        if self.watch.polled.elapsed() < tuning::FOREGROUND_POLL {
            return;
        }
        self.watch.polled = Instant::now();
        let Some(pid) = self.master.foreground() else {
            return;
        };
        if self
            .watch
            .foreground
            .as_ref()
            .is_some_and(|(seen, _)| *seen == pid)
        {
            return;
        }
        let Some(name) = crate::process::name(pid) else {
            return;
        };
        self.watch.foreground = Some((pid, name.clone()));
        send(out, FromHolder::Signal(Signal::Foreground { pid, name }));
    }

    /// Reads the screen once output pauses, or at least every so often while
    /// it keeps changing.
    fn read_screen(&mut self, out: &mut Option<Sender<FromHolder>>) {
        let Some(agent) = &self.watch.agent else {
            return;
        };
        if !self.watch.changed
            || self.engine.in_copy_mode()
            || (self.watch.output.elapsed() < tuning::SCREEN_SETTLE
                && self.watch.read.elapsed() < tuning::SCREEN_READ_MAX)
        {
            return;
        }
        let rows = screen_rows(&mut self.engine);
        let found = agent.read_screen(&rows).map(|rule| ScreenMatch {
            state: rule.state,
            rule: rule.name.clone(),
        });
        self.watch.changed = false;
        self.watch.read = Instant::now();
        if found != self.watch.screen {
            self.watch.screen = found.clone();
            send(out, FromHolder::Signal(Signal::Screen(found)));
        }
    }

    fn agent(&mut self, rules: Option<AgentRules>, out: &mut Option<Sender<FromHolder>>) {
        self.watch.agent = rules.and_then(|rules| {
            AgentKind::from_source(&rules.kind, &rules.rules)
                .inspect_err(|error| eprintln!("twyx holder: agent rules: {error}"))
                .ok()
        });
        self.watch.changed = true;
        if self.watch.agent.is_none() && self.watch.screen.take().is_some() {
            send(out, FromHolder::Signal(Signal::Screen(None)));
        }
    }

    fn output(&mut self, bytes: &[u8], out: &mut Option<Sender<FromHolder>>) {
        self.watch.changed = true;
        self.watch.output = Instant::now();
        if let Some(history) = &mut self.history {
            history.output(bytes);
        }
        for signal in self.scanner.scan(bytes) {
            let signal = match signal {
                PaneSignal::Notification { title, body } => Signal::Notification {
                    title: trim_to(&title, tuning::NOTIFICATION_CHARS),
                    body: trim_to(&body, tuning::NOTIFICATION_CHARS),
                },
                PaneSignal::PromptMark(mark) => Signal::PromptMark(mark),
                PaneSignal::WorkingDirectory(directory) => Signal::WorkingDirectory(directory),
                PaneSignal::Link(target) => Signal::Link(target),
            };
            send(out, FromHolder::Signal(signal));
        }

        for signal in self.engine.advance(bytes) {
            match signal {
                EngineSignal::Title(title) => {
                    send(out, FromHolder::Signal(Signal::Title(trim(&title))));
                }
                EngineSignal::Bell => send(out, FromHolder::Signal(Signal::Bell)),
                EngineSignal::ClipboardWrite(text) => {
                    send(
                        out,
                        FromHolder::Signal(Signal::ClipboardWrite(trim_to(
                            &text,
                            tuning::CLIPBOARD_CHARS,
                        ))),
                    );
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
                self.watch.changed = true;
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
            ToHolder::KillGroup => match self
                .group
                .and_then(|pid| rustix::process::Pid::from_raw(i32::try_from(pid).ok()?))
            {
                Some(group) => {
                    let _ =
                        rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                }
                None => {
                    let _ = self.killer.kill();
                }
            },
            ToHolder::Tail { lines } => {
                let rows = screen_rows(&mut self.engine);
                let mut rows: Vec<&str> = rows
                    .iter()
                    .map(|row| row.trim_end())
                    .rev()
                    .skip_while(|row| row.is_empty())
                    .take(usize::from(lines))
                    .collect();
                rows.reverse();
                send(out, FromHolder::Tail(rows.join("\n")));
            }
            ToHolder::Ping => send(out, FromHolder::Pong),
            ToHolder::Copy(action) => {
                if let Some(text) = self.engine.copy(action) {
                    send(out, FromHolder::Copied(text));
                }
                *dirty = true;
                self.watch.changed = true;
            }
            ToHolder::Agent(rules) => self.agent(rules, out),
            ToHolder::Inject { id, text } => {
                let result = match exited {
                    Some(_) => Err("the pane ended".to_owned()),
                    None => self.inject(&text),
                };
                send(out, FromHolder::Injected { id, result });
            }
        }
        None
    }

    /// Invariant 5, for the holder's part: bracketed paste on, no approval
    /// rule on the screen, one paste, no line break at its end.
    fn inject(&mut self, text: &str) -> Result<(), String> {
        if !self.engine.modes().bracketed_paste {
            return Err("the pane has bracketed paste off".to_owned());
        }
        if let Some(agent) = &self.watch.agent {
            let rows = screen_rows(&mut self.engine);
            if let Some(rule) = agent
                .read_screen(&rows)
                .filter(|rule| rule.state == twyx_core::agent::AgentState::AwaitingApproval)
            {
                return Err(format!("the agent shows {}", rule.name));
            }
        }
        let text = twyx_core::agent::paste_body(text);
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        self.input.send(bytes);
        Ok(())
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
    twyx_protocol::BUILD_ID.to_owned()
}

fn send(out: &mut Option<Sender<FromHolder>>, message: FromHolder) {
    if let Some(sender) = out
        && sender.send(message).is_err()
    {
        *out = None;
    }
}

fn trim(text: &str) -> String {
    trim_to(text, tuning::TITLE_CHARS)
}

fn trim_to(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
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

fn screen_rows(engine: &mut Engine) -> Vec<String> {
    engine
        .view()
        .rows
        .iter()
        .map(|row| row.cells.iter().map(|cell| cell.text.as_str()).collect())
        .collect()
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
