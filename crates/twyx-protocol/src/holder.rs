//! The holder protocol: one socket per pane, the holder on the listening end.
//!
//! The daemon relays frame bodies to clients without parsing them.

use serde::{Deserialize, Serialize};

use crate::input::InputEvent;

pub const MAJOR: u16 = 1;
pub const MINOR: u16 = 0;

/// Sent by the daemon when it connects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub major: u16,
    pub minor: u16,
    pub pane: u64,
    pub build: String,
}

/// The holder's answer, and the screen it replays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub major: u16,
    pub minor: u16,
    pub pane: u64,
    pub build: String,
    pub snapshot: Vec<u8>,
}

/// Why a pane's process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitState {
    Exited { code: u32 },
    Killed { signal: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptMark {
    PromptStart,
    CommandStart,
    /// The command ended with this exit code; `None` while it still runs.
    CommandExecuted(Option<u32>),
}

/// What the holder reports from the pane's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Signal {
    Bell,
    Title(String),
    Notification { title: String, body: String },
    PromptMark(PromptMark),
    WorkingDirectory(String),
    ClipboardWrite(String),
    Link(String),
    Foreground { pid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToHolder {
    Hello(Hello),
    Resize {
        cols: u16,
        rows: u16,
    },
    Input(InputEvent),
    /// Ask for a full snapshot instead of the next frame.
    Snapshot,
    /// The daemon took the exit status and the final screen.
    AckExit,
    Kill,
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromHolder {
    /// A `Hello` arrived with a major this build cannot speak.
    Unsupported {
        major: u16,
        minor: u16,
    },
    Welcome(Welcome),
    /// A frame body, opaque to the daemon.
    Frame {
        seq: u64,
        body: Vec<u8>,
    },
    /// A snapshot body, opaque to the daemon.
    Snapshot {
        seq: u64,
        body: Vec<u8>,
    },
    Signal(Signal),
    Exited(ExitState),
    Pong,
}
