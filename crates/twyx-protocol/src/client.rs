//! The client protocol: one socket, the daemon on the listening end.

use serde::{Deserialize, Serialize};
use twyx_core::agent::{AgentState, Source};
use twyx_core::inbox::Reason;
use twyx_core::keys::Command;
use twyx_core::layout::{Axis, Fractions, Layout, LeafId, Side};
use twyx_core::theme::AgentState as Mark;

use crate::holder::CopyAction;
use crate::input::InputEvent;

pub const MAJOR: u16 = 3;
pub const MINOR: u16 = 3;

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
    /// The workspace to show, when it still exists.
    pub workspace: Option<u64>,
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
    /// The agent session in the pane, if one runs. Since minor 1.
    pub agent: Option<AgentInfo>,
    /// The agent kind an ended pane can resume. Since minor 2.
    pub resume: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub kind: String,
    pub state: AgentState,
    pub source: Source,
    /// When the state was observed, in milliseconds since the Unix epoch.
    pub observed_ms: u64,
    /// The agent's own reference to its session, for resume.
    pub session: Option<String>,
}

/// What an agent's integration reports through `twyx notify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentEvent {
    Working,
    AwaitingApproval,
    AwaitingPrompt,
    TurnFinished,
    Error,
    /// Only the native session reference, with no change of state.
    Session,
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

/// A client's workspace: the tabs for one worktree or plain directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace: u64,
    pub name: String,
    pub directory: String,
    /// `None` for a plain directory and for the default workspace.
    pub worktree: Option<WorktreeInfo>,
    pub tabs: Vec<TabInfo>,
}

/// A worktree's branch and dirty state, as Git last reported them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitState {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Changed, staged and untracked paths that are not ignored.
    pub changed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub worktree: u64,
    pub project: u64,
    /// The Git admin name, and `main` for the main worktree.
    pub name: String,
    pub path: String,
    /// The branch from the last worktree listing; `None` when detached.
    pub branch: Option<String>,
    /// `None` until twyx has read it.
    pub git: Option<GitState>,
    /// The last Git error for this worktree, with its command.
    pub error: Option<String>,
    /// The worktree's workspace, once it has one.
    pub workspace: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project: u64,
    pub name: String,
    /// The common Git dir of a repository, or the directory.
    pub path: String,
    pub git: bool,
    pub worktrees: Vec<WorktreeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub workspace: u64,
    pub name: String,
    pub directory: String,
    pub project: Option<u64>,
    pub worktree: Option<u64>,
    pub tabs: u32,
    /// The panes its tabs show, tab by tab.
    pub panes: Vec<PaneRef>,
    /// Interactive clients that show it.
    pub clients: u32,
}

/// One pane where a workspace shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRef {
    pub pane: u64,
    pub tab: u64,
    pub leaf: LeafId,
    /// The pane's status mark; a plain running pane has none. Since minor 3.
    pub mark: Option<Mark>,
    /// The kind of the agent session in the pane. Since minor 3.
    pub agent: Option<String>,
}

/// One inbox item: an agent session or a pane that needs the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub item: u64,
    pub pane: u64,
    pub reason: Reason,
    /// The pane's agent state and its source now, if an agent runs there.
    pub agent: Option<(AgentState, Source)>,
    pub mark: Option<Mark>,
    /// When the item appeared, in milliseconds since the Unix epoch.
    pub created_ms: u64,
    pub read: bool,
    pub workspace: u64,
    pub project: Option<String>,
    pub worktree: Option<String>,
    pub title: String,
}

/// Every project, worktree and workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub projects: Vec<ProjectInfo>,
    pub workspaces: Vec<WorkspaceSummary>,
}

/// What removing a worktree would lose, for the user to confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPlan {
    /// Confirms this plan, and only this plan.
    pub plan: u64,
    pub worktree: WorktreeInfo,
    /// Changed and untracked paths, with their two-letter status.
    pub dirty: Vec<(String, String)>,
    /// Commits no remote-tracking branch has.
    pub unpushed: Vec<CommitInfo>,
    /// All unpushed commits, of which `unpushed` lists the newest.
    pub unpushed_total: u32,
    /// Commits the base branch does not have, and the base.
    pub unmerged: Vec<CommitInfo>,
    pub base: Option<String>,
    /// The branch a confirmation may delete; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Live panes that the removal kills: those spawned in the worktree and
    /// those in its workspace.
    pub panes: Vec<PaneInfo>,
}

/// A worktree's `.twyx/worktree.toml`, as the trust prompt shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupInfo {
    pub worktree: u64,
    /// Relative to the worktree.
    pub path: String,
    /// The SHA-256 of the content shown here.
    pub hash: String,
    pub trusted: bool,
    /// Paths linked from the main worktree.
    pub shared: Vec<String>,
    pub commands: Vec<String>,
    /// Why the file could not be read; it does not run.
    pub error: Option<String>,
}

/// What setting up a worktree did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupReport {
    /// Gitignored paths copied from the main worktree.
    pub copied: Vec<String>,
    /// Shared paths linked from the main worktree.
    pub linked: Vec<String>,
    /// The pane that runs the setup commands.
    pub pane: Option<u64>,
    /// The setup file is not trusted in its content, so nothing of it ran.
    pub untrusted: bool,
    pub errors: Vec<String>,
}

/// Where a recipe comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecipeSource {
    BuiltIn,
    /// A file in the user's config; it needs no trust.
    User {
        path: String,
    },
    /// A file in the workspace's `.twyx/recipes`; it runs only while its
    /// content is trusted.
    Repository {
        path: String,
        /// The SHA-256 of the content shown here.
        hash: String,
        trusted: bool,
    },
}

/// One pane of a recipe, as the trust prompt lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipePaneInfo {
    pub name: String,
    /// `None` runs the shell.
    pub command: Option<String>,
    pub dir: Option<String>,
    pub env: Vec<(String, String)>,
    pub agent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeInfo {
    pub name: String,
    pub description: String,
    pub source: RecipeSource,
    /// Why the file could not be read; such a recipe cannot run.
    pub error: Option<String>,
    pub tabs: u32,
    pub panes: Vec<RecipePaneInfo>,
    /// Whether it opens in a new worktree.
    pub new_worktree: bool,
    /// A recipe of the same name in a source that wins over this one.
    pub shadowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub oid: String,
    pub subject: String,
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
    /// Interactive clients attached before this one.
    pub clients: u32,
    /// Holders this daemon found but cannot adopt.
    pub legacy: Vec<LegacyHolder>,
}

/// A holder that speaks a protocol major this daemon cannot adopt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyHolder {
    pub socket: String,
    pub major: u16,
    pub pid: Option<u32>,
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
        /// Whether the pane keeps a history on disk; config can still turn
        /// history off for every pane.
        history: bool,
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
    /// A copy mode action for the focused pane.
    Copy(CopyAction),
    /// Ends every legacy holder and forgets its socket.
    KillLegacy,
    /// Deletes every pane's history. Running panes start a new one.
    PurgeHistory,
    /// Opens the project a path is in, and the workspace of its worktree or
    /// directory. An interactive client switches to it; a command switches
    /// the interactive client that last sent input when `switch` is set.
    Open {
        path: String,
        switch: bool,
    },
    /// Shows a workspace: in this client, or for a command in the
    /// interactive client that last sent input.
    SwitchWorkspace {
        workspace: u64,
    },
    /// Every project, worktree and workspace; with `refresh`, after Git
    /// has been read again.
    Overview {
        refresh: bool,
    },
    /// Creates a worktree of a project on a branch, new from `start` (the
    /// base branch when `None`) or existing, and its workspace.
    NewWorktree {
        project: u64,
        branch: String,
        start: Option<String>,
    },
    /// Asks what removing a worktree would lose.
    PlanRemoval {
        worktree: u64,
    },
    /// Removes a worktree as the plan listed it: its panes are killed
    /// first, and its changes are discarded only when the plan listed them.
    /// With `delete_branch`, the branch goes too: with `git branch -d`, or
    /// `-D` when the plan listed its unmerged commits.
    RemoveWorktree {
        plan: u64,
        delete_branch: bool,
    },
    /// A worktree's setup file, for the trust prompt.
    Setup {
        worktree: u64,
    },
    /// Trusts a worktree's setup file whose content has this hash.
    TrustSetup {
        worktree: u64,
        hash: String,
    },
    /// Sets a worktree up again: copies, links and, when trusted, the
    /// setup commands in a pane.
    RunSetup {
        worktree: u64,
    },
    /// The recipes a workspace can run: built in, the user's, and its
    /// repository's.
    Recipes {
        workspace: u64,
    },
    /// Opens a recipe's tabs in a workspace, or in a new worktree on
    /// `branch`. `switch` works as for `Open`.
    RunRecipe {
        workspace: u64,
        name: String,
        branch: Option<String>,
        switch: bool,
    },
    /// Trusts a repository recipe whose content has this hash.
    TrustRecipe {
        workspace: u64,
        name: String,
        hash: String,
    },
    /// Ask the daemon to exit. Panes keep running.
    Shutdown,
    /// Sends an `Overview` and the `Inbox` now and after every change to
    /// them, for a sidebar.
    WatchOverview,
    /// Whether this client shows its sidebar: the daemon then keeps every
    /// worktree's branch and dirty state current, not only the shown one's.
    ShowSidebar {
        shown: bool,
    },
    /// An agent integration's event for a pane. Since minor 1.
    Notify {
        pane: u64,
        /// The agent kind, when the integration names it.
        agent: Option<String>,
        event: AgentEvent,
        session: Option<String>,
    },
    /// Resumes the agent sessions of ended panes, each in a new holder in
    /// its leaf. The client asked the user first. Since minor 2.
    Resume {
        panes: Vec<u64>,
    },
    /// Sends the `Inbox` once. Since minor 3.
    Inbox,
    /// Sets an inbox item unread again. Since minor 3.
    MarkUnread {
        item: u64,
    },
    /// Removes an inbox item. Since minor 3.
    Dismiss {
        item: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FromDaemon {
    /// Greets a client this build cannot serve. Every build keeps this
    /// first variant and its fields as they are, so any client can read it
    /// and stop the daemon.
    Unsupported {
        major: u16,
        minor: u16,
        pid: u32,
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
    /// A path opened: its workspace, and the client that now shows it.
    Opened {
        workspace: u64,
        client: Option<u64>,
    },
    /// A workspace switch: the client that now shows it, if any.
    Switched {
        workspace: u64,
        client: Option<u64>,
    },
    Overview(Overview),
    WorktreeAdded {
        worktree: WorktreeInfo,
        workspace: u64,
        setup: SetupReport,
    },
    RemovalPlan(RemovalPlan),
    /// A worktree is gone. `branch` says what became of its branch when
    /// the confirmation asked to delete it.
    WorktreeRemoved {
        worktree: u64,
        branch: Option<Result<String, String>>,
    },
    /// `None` when the worktree has no setup file.
    SetupFile(Option<SetupInfo>),
    SetupTrusted {
        worktree: u64,
    },
    SetupDone {
        worktree: u64,
        report: SetupReport,
    },
    Recipes(Vec<RecipeInfo>),
    /// A recipe opened its tabs in a workspace, shown in `client`.
    RecipeOpened {
        workspace: u64,
        tabs: Vec<u64>,
        client: Option<u64>,
    },
    RecipeTrusted {
        name: String,
    },
    /// The inbox, newest first. Since minor 3.
    Inbox(Vec<InboxItem>),
}

/// The request a keymap command makes of the daemon, for a client whose
/// focused pane is `focused`. `None` for a command the client handles alone.
pub fn request(command: Command, focused: Option<&PaneInfo>) -> Option<ToDaemon> {
    let cwd = || focused.map(|pane| pane.cwd.clone()).unwrap_or_default();
    let new_pane = |place| ToDaemon::NewPane {
        launch: Launch::Shell,
        cwd: cwd(),
        place,
        history: true,
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
        Command::CopyMove(motion) => ToDaemon::Copy(CopyAction::Move(motion)),
        Command::CopySelect => ToDaemon::Copy(CopyAction::Select),
        Command::CopySelectLine => ToDaemon::Copy(CopyAction::SelectLine),
        Command::CopyYank => ToDaemon::Copy(CopyAction::Yank),
        Command::CopyNext => ToDaemon::Copy(CopyAction::Next),
        Command::CopyPrevious => ToDaemon::Copy(CopyAction::Previous),
        Command::Detach
        | Command::Resume
        | Command::Integrations
        | Command::Inbox
        | Command::ToggleSidebar
        | Command::Switcher
        | Command::SidebarUp
        | Command::SidebarDown
        | Command::SidebarOpen
        | Command::NextWorkspace
        | Command::PreviousWorkspace
        | Command::NewWorktree
        | Command::RemoveWorktree
        | Command::SetupWorktree
        | Command::Enter(_)
        | Command::Palette
        | Command::RenameTab
        | Command::CopySearch => {
            return None;
        }
    })
}
