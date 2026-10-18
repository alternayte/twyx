//! The client protocol: one socket, the daemon on the listening end.

use serde::{Deserialize, Serialize};

use crate::input::InputEvent;

pub const MAJOR: u16 = 1;
pub const MINOR: u16 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub major: u16,
    pub minor: u16,
    pub build: String,
    pub cols: u16,
    pub rows: u16,
}

/// What a pane's process is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneState {
    Running,
    Exited {
        code: u32,
    },
    Killed {
        signal: String,
    },
    /// The holder is gone; the pane's process state is unknown.
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: u64,
    pub cols: u16,
    pub rows: u16,
    pub state: PaneState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub major: u16,
    pub minor: u16,
    pub build: String,
    /// The daemon's process id.
    pub pid: u32,
    /// The identity of this client for the rest of the connection.
    pub client: u64,
    pub panes: Vec<PaneInfo>,
    /// The client that sets the pane sizes, if any.
    pub lease: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToDaemon {
    Hello(ClientHello),
    /// The client's viewport changed.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// An input event for the focused pane.
    Input(InputEvent),
    TakeLease,
    /// Ask for a full screen instead of the next frame.
    Snapshot {
        pane: u64,
    },
    NewPane {
        argv: Vec<String>,
        cwd: String,
        cols: u16,
        rows: u16,
    },
    KillPane {
        pane: u64,
    },
    /// Ask the daemon to exit. Panes keep running.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromDaemon {
    /// Greets a client this build cannot serve.
    Unsupported {
        major: u16,
        minor: u16,
    },
    Welcome(ServerHello),
    /// A frame body, as the holder published it.
    Frame {
        pane: u64,
        seq: u64,
        body: Vec<u8>,
    },
    /// A snapshot body, as the holder published it.
    Snapshot {
        pane: u64,
        seq: u64,
        body: Vec<u8>,
    },
    Lease {
        holder: Option<u64>,
    },
    Pane(PaneInfo),
    /// A pane asked to put text in the user's clipboard.
    Clipboard {
        pane: u64,
        text: String,
    },
}
