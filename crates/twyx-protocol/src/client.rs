//! The client protocol: one socket, the daemon on the listening end.

use serde::{Deserialize, Serialize};
use twyx_core::keys::Command;
use twyx_core::layout::{Axis, Fractions, Layout, LeafId, Side};

use crate::input::InputEvent;

pub const MAJOR: u16 = 2;
pub const MINOR: u16 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub major: u16,
    pub minor: u16,
    pub build: String,
    /// The client's tab area, in cells.
    pub cols: u16,
    pub rows: u16,
    /// A TUI or desktop client. A CLI command is not interactive: it never
    /// takes a size lease and gets no frames.
    pub interactive: bool,
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

/// What a pane was started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Launch {
    /// The user's shell. The pane closes when the shell exits.
    Shell,
    /// An explicit command. The pane stays after it ends and can run again.
    Command(Vec<String>),
    /// A holder the daemon adopted with no record of its start.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: u64,
    pub cols: u16,
    pub rows: u16,
    pub state: PaneState,
    pub launch: Launch,
    pub cwd: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab: u64,
    /// Empty until the user names the tab.
    pub name: String,
    pub layout: Layout,
    /// The client that sets the pane sizes, if any.
    pub lease: Option<u64>,
    /// The tab area the pane sizes follow: the leaseholder's.
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace: u64,
    pub name: String,
    pub tabs: Vec<TabInfo>,
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
}

/// Where a new pane goes, relative to the client's focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Place {
    Split(Axis),
    Stack,
    Floating,
    NewTab,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToDaemon {
    Hello(ClientHello),
    /// The client's tab area changed.
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
        launch: Launch,
        cwd: String,
        place: Place,
    },
    /// Ends a pane's process. Its leaf stays and shows the exit.
    KillPane {
        pane: u64,
    },
    /// Ends the focused pane's process and removes its leaf.
    ClosePane,
    /// Starts an ended pane's command again in a new holder, in its leaf.
    RunAgain {
        pane: u64,
    },
    Focus {
        side: Side,
    },
    FocusLeaf {
        leaf: LeafId,
    },
    ResizePane {
        side: Side,
    },
    SwapPane {
        side: Side,
    },
    ToggleZoom,
    ToggleFloating,
    MoveFloating {
        leaf: LeafId,
        geometry: Fractions,
    },
    /// Moves the rule on `side` of a tiled leaf by whole cells.
    DragRule {
        leaf: LeafId,
        side: Side,
        cells: i32,
    },
    SelectTab {
        tab: u64,
    },
    /// The next or previous tab, wrapping at the ends.
    CycleTab {
        forward: bool,
    },
    /// The tab at a zero-based position.
    SelectTabAt {
        index: usize,
    },
    /// Moves the focused floating pane one step.
    NudgeFloating {
        side: Side,
    },
    RenameTab {
        tab: u64,
        name: String,
    },
    /// Ends every pane in the tab and removes it.
    CloseTab {
        tab: u64,
    },
    /// Closes the tab this client shows.
    CloseFocusedTab,
    /// Ask the daemon to exit. Panes keep running.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The workspace's tabs and layouts, after any change.
    Workspace(WorkspaceInfo),
    /// This client's tab and focused leaf.
    Focus {
        tab: Option<u64>,
        leaf: Option<LeafId>,
    },
    Pane(PaneInfo),
    /// A pane is gone: its leaf was closed.
    PaneClosed {
        pane: u64,
    },
    /// The daemon could not do what this client asked.
    Refused {
        reason: String,
    },
    /// A pane asked to put text in the user's clipboard.
    Clipboard {
        pane: u64,
        text: String,
    },
}

/// The request a keymap command makes of the daemon, for a client whose
/// focused pane is `focused`. `None` for a command the client handles alone.
pub fn request(command: Command, focused: Option<&PaneInfo>) -> Option<ToDaemon> {
    let cwd = || focused.map(|pane| pane.cwd.clone()).unwrap_or_default();
    let new_pane = |place| ToDaemon::NewPane {
        launch: Launch::Shell,
        cwd: cwd(),
        place,
    };
    Some(match command {
        Command::SplitRight => new_pane(Place::Split(Axis::Row)),
        Command::SplitDown => new_pane(Place::Split(Axis::Column)),
        Command::NewStacked => new_pane(Place::Stack),
        Command::NewFloating => new_pane(Place::Floating),
        Command::NewTab => new_pane(Place::NewTab),
        Command::ClosePane => ToDaemon::ClosePane,
        Command::RunAgain => ToDaemon::RunAgain {
            pane: focused?.pane,
        },
        Command::Focus(side) => ToDaemon::Focus { side },
        Command::Resize(side) => ToDaemon::ResizePane { side },
        Command::Swap(side) => ToDaemon::SwapPane { side },
        Command::MoveFloating(side) => ToDaemon::NudgeFloating { side },
        Command::ToggleZoom => ToDaemon::ToggleZoom,
        Command::ToggleFloating => ToDaemon::ToggleFloating,
        Command::NextTab => ToDaemon::CycleTab { forward: true },
        Command::PreviousTab => ToDaemon::CycleTab { forward: false },
        Command::GoToTab(number) => ToDaemon::SelectTabAt {
            index: usize::from(number.saturating_sub(1)),
        },
        Command::CloseTab => ToDaemon::CloseFocusedTab,
        Command::Detach | Command::Enter(_) => return None,
    })
}
