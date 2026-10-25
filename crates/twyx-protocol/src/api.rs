//! API v1: JSON lines on the daemon socket.
//!
//! A connection whose first byte is `{` speaks API v1; a client protocol
//! frame starts with a zero byte. Each line from the client is a request,
//! `{"id": 1, "method": "check.run", "params": {...}}`. The daemon answers
//! each one in order with `{"id": 1, "result": ...}` or
//! `{"id": 1, "error": {"message": "..."}}`. After `events.subscribe` it
//! also writes `{"event": "check.ended", "data": ...}` lines. The first
//! request must be `hello` with the version the client speaks.
//!
//! The method names, their parameters, the envelope and the event names
//! are API v1. Results and event data are the client protocol's data
//! types as JSON; a change to their shape is an API change.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{AgentEvent, Place};

pub const VERSION: u16 = 1;
pub const MINOR: u16 = 0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    pub data: Value,
}

/// A line from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Line {
    Reply(Reply),
    Event(Event),
}

/// `hello`: the version the client speaks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub version: u16,
}

/// What `hello` answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub version: u16,
    pub minor: u16,
    /// The daemon's build.
    pub build: String,
    pub pid: u32,
    /// Interactive clients attached.
    pub clients: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub path: String,
    /// Show it in the interactive client that last had input.
    #[serde(default)]
    pub switch: bool,
}

/// `document.open`: a file shown as a Markdown document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenDocument {
    /// An absolute path.
    pub path: String,
    /// Show it in the interactive client that last had input.
    #[serde(default)]
    pub switch: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverviewParams {
    /// Read every worktree's Git state again first.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRef {
    pub workspace: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRef {
    pub worktree: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneRef {
    pub pane: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabRef {
    pub tab: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanePlace {
    #[default]
    Right,
    Down,
    Stack,
    Float,
    Tab,
}

impl PanePlace {
    pub fn place(self) -> Place {
        match self {
            PanePlace::Right => Place::Split(twyx_core::layout::Axis::Row),
            PanePlace::Down => Place::Split(twyx_core::layout::Axis::Column),
            PanePlace::Stack => Place::Stack,
            PanePlace::Float => Place::Floating,
            PanePlace::Tab => Place::NewTab,
        }
    }
}

/// `pane.new`: in the workspace the interactive client that last had
/// input shows, beside its focused pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPane {
    /// The command and its arguments; the user's shell when empty.
    #[serde(default)]
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub place: PanePlace,
    #[serde(default = "yes")]
    pub history: bool,
}

fn yes() -> bool {
    true
}

/// `pane.send`: text, typed or pasted, then keys such as `enter` or
/// `ctrl-c`, in that order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendToPane {
    pub pane: u64,
    #[serde(default)]
    pub text: Option<String>,
    /// Send `text` as one bracketed paste instead of typing it.
    #[serde(default)]
    pub paste: bool,
    #[serde(default)]
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewWorktree {
    pub project: u64,
    pub branch: String,
    #[serde(default)]
    pub start: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveWorktree {
    /// From `worktree.plan_removal`.
    pub plan: u64,
    #[serde(default)]
    pub delete_branch: bool,
}

/// Trust a file at the hash the user saw.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustFile {
    pub worktree: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecipe {
    pub workspace: u64,
    pub name: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub switch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRecipe {
    pub workspace: u64,
    pub name: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTask {
    pub workspace: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub start: Option<String>,
    /// One worktree each: an agent kind, or `null` for a shell. Empty is
    /// one worktree with the default agent.
    #[serde(default)]
    pub agents: Vec<Option<String>>,
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRef {
    pub item: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkReviewed {
    pub worktree: u64,
    pub path: String,
    /// Hunk ids from `review.status`; the whole file without them.
    #[serde(default)]
    pub hunks: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub worktree: u64,
    /// Every change without a path.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub hunks: Option<Vec<u64>>,
    #[serde(default)]
    pub unstage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub worktree: u64,
    pub message: String,
    /// Answer when the pane exits, not when it starts.
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Push {
    pub worktree: u64,
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub worktree: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddNote {
    pub worktree: u64,
    pub path: String,
    /// First and last line in the working file, from 1.
    pub lines: (u32, u32),
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendNotes {
    pub worktree: u64,
    pub pane: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRef {
    pub worktree: u64,
    pub name: String,
    /// `check.run`: answer with the result, not when the run starts.
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    pub pane: u64,
    #[serde(default)]
    pub agent: Option<String>,
    pub event: AgentEvent,
    #[serde(default)]
    pub session: Option<String>,
}

/// `events.subscribe`: the event kinds to get; every kind when empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscribe {
    #[serde(default)]
    pub kinds: Vec<String>,
}

/// The event kinds `events.subscribe` takes. An event's name starts with
/// its kind: `agent`, `pane.closed`, `check.ended`.
pub const EVENT_KINDS: [&str; 7] = [
    "agent",
    "pane",
    "workspace",
    "overview",
    "inbox",
    "check",
    "ship",
];

/// A key by name, as config files write it: `enter`, `ctrl-c`, `alt-x`.
pub fn key(name: &str) -> std::result::Result<crate::input::KeyEvent, String> {
    use crate::input::{KeyCode, KeyEvent, Modifiers};
    use twyx_core::keys::{Chord, Key};
    let chord: Chord = name.parse()?;
    let code = match chord.key {
        Key::Char(ch) => KeyCode::Char(ch),
        Key::Enter => KeyCode::Enter,
        Key::Tab => KeyCode::Tab,
        Key::Backspace => KeyCode::Backspace,
        Key::Escape => KeyCode::Escape,
        Key::Delete => KeyCode::Delete,
        Key::Insert => KeyCode::Insert,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::F(n) => KeyCode::F(n),
    };
    let mut modifiers = Modifiers::empty();
    modifiers.set(Modifiers::CTRL, chord.ctrl);
    modifiers.set(Modifiers::ALT, chord.alt);
    modifiers.set(Modifiers::SHIFT, chord.shift);
    Ok(KeyEvent::press(code, modifiers))
}

/// A blocking API v1 client.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
    /// Events read while waiting for a reply.
    events: std::collections::VecDeque<Event>,
    pub welcome: Welcome,
}

impl Client {
    /// Connects and says `hello`.
    pub fn connect(socket: &Path) -> Result<Self> {
        let stream = crate::peer::dial(socket)
            .with_context(|| format!("connect to the daemon on {}", socket.display()))?;
        let mut client = Client {
            reader: BufReader::new(stream.try_clone().context("clone the socket")?),
            writer: stream,
            next: 0,
            events: std::collections::VecDeque::new(),
            welcome: Welcome {
                version: VERSION,
                minor: MINOR,
                build: String::new(),
                pid: 0,
                clients: 0,
            },
        };
        client.welcome = client.call("hello", &Hello { version: VERSION })?;
        Ok(client)
    }

    /// Calls `method` and waits for its answer.
    pub fn call<R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<R> {
        self.next += 1;
        let id = self.next;
        let request = Request {
            id,
            method: method.to_owned(),
            params: serde_json::to_value(params).context("encode the parameters")?,
        };
        let mut line = serde_json::to_string(&request).context("encode a request")?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .with_context(|| format!("send {method}"))?;
        loop {
            match self.read()? {
                Line::Reply(reply) if reply.id == id => {
                    if let Some(error) = reply.error {
                        bail!("{}", error.message);
                    }
                    let result = reply.result.unwrap_or(Value::Null);
                    return serde_json::from_value(result)
                        .with_context(|| format!("read the answer to {method}"));
                }
                Line::Reply(_) => {}
                Line::Event(event) => self.events.push_back(event),
            }
        }
    }

    /// The next event, after `events.subscribe`.
    pub fn event(&mut self) -> Result<Event> {
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        loop {
            if let Line::Event(event) = self.read()? {
                return Ok(event);
            }
        }
    }

    fn read(&mut self) -> Result<Line> {
        let mut line = String::new();
        if self
            .reader
            .read_line(&mut line)
            .context("read from the daemon")?
            == 0
        {
            bail!("the daemon closed the connection");
        }
        serde_json::from_str(&line).with_context(|| format!("read the daemon's line {line:?}"))
    }
}
