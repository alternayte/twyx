//! The daemon: workspaces with their tabs and layouts, their panes, the
//! clients attached to them, and one size lease per tab.
//!
//! Threads: each client and each holder gets a reader thread and a writer
//! thread, and each Git job its own thread. This thread owns the state and
//! forwards frames without reading them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};

use twyx_core::layout::{Axis, Cells, Content, Layout, Leaf, LeafId};
use twyx_core::pane::PaneId;
use twyx_core::paths::Paths;
use twyx_protocol::client::{
    ClientHello, FromDaemon, Launch, LegacyHolder, MAJOR, MINOR, PaneInfo, PaneState, ServerHello,
    ToDaemon,
};
use twyx_protocol::holder::{ExitState, FromHolder, ToHolder};
use twyx_protocol::input::{InputEvent, KeyKind};
use twyx_protocol::wire;

use crate::agents::Terminal;
use crate::holders::{self, HolderLink, Legacy};
use crate::peer;
use crate::store::{Saved, SavedPane, SavedTab, SavedWorkspace, Store};
use crate::workspaces::Workspace;
use crate::worktrees::{self, Plans, Project, Removal, Waiting, Worktree};

static CLIENTS: AtomicU64 = AtomicU64::new(1);

/// The tab area a tab gets before any interactive client has sized it.
const DEFAULT_AREA: (u16, u16) = (80, 24);

pub(crate) enum Event {
    ClientHello {
        client: u64,
        hello: ClientHello,
        out: Sender<FromDaemon>,
    },
    ClientMessage {
        client: u64,
        message: ToDaemon,
    },
    ClientGone {
        client: u64,
    },
    Holder {
        pane: PaneId,
        message: FromHolder,
    },
    HolderGone {
        pane: PaneId,
    },
    Git(worktrees::Done),
    Tick,
}

#[derive(Debug)]
pub(crate) struct Pane {
    pub link: Option<HolderLink>,
    pub cols: u16,
    pub rows: u16,
    pub state: PaneState,
    /// The last complete screen, for a client that attaches after the exit.
    pub screen: Option<Vec<u8>>,
    pub launch: Launch,
    pub cwd: String,
    pub title: String,
    /// Whether the pane keeps a history on disk.
    pub history: bool,
    pub workspace: u64,
    /// The worktree the pane started in; it never changes.
    pub spawn_worktree: Option<u64>,
    /// Variables the pane's command gets over the daemon's own.
    pub env: Vec<(String, String)>,
    pub agent: crate::agents::PaneAgent,
}

#[derive(Debug)]
pub(crate) struct Tab {
    pub workspace: u64,
    pub name: String,
    pub layout: Layout,
    pub lease: Option<u64>,
    /// The tab area the pane sizes follow.
    pub area: (u16, u16),
    /// The last focus any client set, for a client that arrives later.
    pub focus: Option<LeafId>,
}

#[derive(Debug)]
pub(crate) struct Client {
    pub out: Sender<FromDaemon>,
    pub cols: u16,
    pub rows: u16,
    pub interactive: bool,
    pub workspace: u64,
    pub tab: Option<u64>,
    /// This client's focus in each tab it has shown.
    pub focus: BTreeMap<u64, LeafId>,
    /// When the client last sent input, on the daemon's use counter.
    pub used: u64,
    /// Whether the client gets every change to the overview.
    pub watching: bool,
    /// Whether the client shows its sidebar, and so every worktree.
    pub sidebar: bool,
    /// Whether the client's window has focus, as its last focus report
    /// said; a client that never reports one counts as focused.
    pub focused: bool,
    /// The panes in view when the inbox last looked.
    pub seen: std::collections::BTreeSet<PaneId>,
    /// The paper and ink the client shows panes on.
    pub colours: Option<twyx_protocol::holder::PaneColours>,
}

#[derive(Debug)]
pub struct Daemon {
    pub(crate) paths: Paths,
    pub(crate) build: String,
    pub(crate) holder_binary: PathBuf,
    pub(crate) events: Sender<Event>,
    pub(crate) panes: BTreeMap<PaneId, Pane>,
    /// Panes whose leaf is closed, kept until their holder reports the exit.
    pub(crate) closing: BTreeMap<PaneId, HolderLink>,
    pub(crate) clients: BTreeMap<u64, Client>,
    /// Every workspace's tabs, each workspace's in order.
    pub(crate) tabs: Vec<(u64, Tab)>,
    pub(crate) workspaces: BTreeMap<u64, Workspace>,
    /// The workspace at `$HOME`.
    pub(crate) default_workspace: u64,
    /// The workspace a new client shows: the last one a client switched to.
    pub(crate) active_workspace: u64,
    pub(crate) projects: BTreeMap<u64, Project>,
    pub(crate) worktrees: BTreeMap<u64, Worktree>,
    pub(crate) plans: Plans,
    pub(crate) next_plan: u64,
    pub(crate) removals: Vec<Removal>,
    /// Overviews commands wait for.
    pub(crate) waiting: Vec<Waiting>,
    /// Counts input, to find the client that last sent some.
    pub(crate) uses: u64,
    /// The overview watchers last got.
    pub(crate) overview: Option<twyx_protocol::client::Overview>,
    pub(crate) next_leaf: u64,
    pub(crate) store: Store,
    /// Holders of a protocol major this daemon cannot adopt.
    pub(crate) legacy: Vec<LegacyHolder>,
    /// The last good config.
    pub(crate) config: twyx_core::config::Config,
    /// The last good agent kinds.
    pub(crate) agents: twyx_core::agent::AgentKinds,
    /// Messages for clients, sent once the event that made them is written,
    /// so no client sees a change a crash could still lose.
    pub(crate) outbox: RefCell<Vec<(Sender<FromDaemon>, FromDaemon)>>,
    /// The inbox items not dismissed, by id.
    pub(crate) inbox: BTreeMap<u64, crate::inbox::Item>,
    /// Whether watching clients have yet to get the inbox's last change.
    pub(crate) inbox_changed: bool,
    /// When each pane last notified, and when any did, for the rate limits.
    pub(crate) notified: BTreeMap<PaneId, std::time::Instant>,
    pub(crate) last_notice: Option<std::time::Instant>,
    /// Tasks whose agent waits for a setup pane, by that pane.
    pub(crate) tasks: BTreeMap<PaneId, crate::tasks::Waiting>,
    /// One worktree add at a time per project.
    pub(crate) add_locks: BTreeMap<u64, std::sync::Arc<std::sync::Mutex<()>>>,
    /// Each worktree's review jobs, run one at a time in the order sent.
    pub(crate) review_queues: BTreeMap<u64, Sender<crate::review::Job>>,
    /// Pastes a holder has yet to answer, by id.
    pub(crate) injections: BTreeMap<u64, crate::inject::Pending>,
    pub(crate) next_injection: u64,
    /// Check runs without a result yet, by pane.
    pub(crate) check_runs: BTreeMap<PaneId, crate::checks::Running>,
    /// Checks whose source key is being taken before their pane starts.
    pub(crate) checks_starting: std::collections::BTreeSet<(u64, String)>,
    /// Commit, push and pull request steps running in panes.
    pub(crate) ships: BTreeMap<PaneId, crate::ship::Ship>,
}

/// Starts the daemon and serves it until the socket closes.
pub fn run(paths: Paths) -> Result<()> {
    paths.prepare_runtime()?;
    let listener = bind(&paths.socket())?;
    let holder_binary = std::env::current_exe().context("find the twyx binary")?;
    let build = twyx_protocol::BUILD_ID.to_owned();
    let (events, inbound) = mpsc::channel();
    let store = Store::open(&paths.database())?;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
    let saved = store.load(&home)?;
    let config_file = paths.config.join("config.toml");
    let agents_dir = paths.config.join("agents");

    let mut daemon = Daemon {
        paths,
        build,
        holder_binary,
        events: events.clone(),
        panes: BTreeMap::new(),
        closing: BTreeMap::new(),
        clients: BTreeMap::new(),
        tabs: Vec::new(),
        workspaces: BTreeMap::new(),
        default_workspace: saved.default_workspace,
        active_workspace: saved.active_workspace,
        projects: BTreeMap::new(),
        worktrees: BTreeMap::new(),
        plans: Plans::new(),
        next_plan: 0,
        removals: Vec::new(),
        waiting: Vec::new(),
        uses: 0,
        overview: None,
        next_leaf: saved.next_leaf,
        store,
        outbox: RefCell::new(Vec::new()),
        inbox: BTreeMap::new(),
        inbox_changed: false,
        notified: BTreeMap::new(),
        last_notice: None,
        tasks: BTreeMap::new(),
        review_queues: BTreeMap::new(),
        injections: BTreeMap::new(),
        next_injection: 0,
        check_runs: BTreeMap::new(),
        add_locks: BTreeMap::new(),
        checks_starting: std::collections::BTreeSet::new(),
        ships: BTreeMap::new(),
        legacy: Vec::new(),
        config: twyx_core::config::load(&config_file).unwrap_or_else(|error| {
            eprintln!("twyx daemon: {error}");
            twyx_core::config::Config::default()
        }),
        agents: twyx_core::agent::AgentKinds::load(&agents_dir).unwrap_or_else(|error| {
            eprintln!("twyx daemon: {error}");
            twyx_core::agent::AgentKinds::built_in()
        }),
    };
    let agents = daemon.restore(saved);
    daemon.adopt();
    daemon.restore_agents(agents);
    daemon.restore_checks();
    daemon.sweep_history();
    match daemon.store.inbox() {
        Ok(items) => {
            daemon.inbox = items
                .into_iter()
                .map(|item| (item.id, item.into()))
                .collect();
        }
        Err(error) => eprintln!("twyx daemon: {error:#}"),
    }
    daemon.reconcile();
    daemon.save();
    spawn_ticker(events.clone())?;
    spawn_acceptor(listener, events)?;
    daemon.serve(&inbound)
}

fn spawn_ticker(events: Sender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("twyx-daemon-tick".to_owned())
        .spawn(move || {
            while events.send(Event::Tick).is_ok() {
                thread::sleep(twyx_core::tuning::GIT_POLL);
            }
        })
        .context("start the daemon ticker")?;
    Ok(())
}

fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        // A live daemon keeps its socket; a dead one leaves a file behind.
        if UnixStream::connect(path).is_ok() {
            bail!("a daemon is already listening on {}", path.display());
        }
        std::fs::remove_file(path)
            .with_context(|| format!("remove the stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))
}

impl Daemon {
    /// A changed config or agent rule file applies from the next pane; a bad
    /// one keeps the last good.
    pub(crate) fn reload_config(&mut self) {
        if let Ok(config) = twyx_core::config::load(&self.paths.config.join("config.toml")) {
            self.config = config;
        }
        if let Ok(agents) = twyx_core::agent::AgentKinds::load(&self.paths.config.join("agents")) {
            self.agents = agents;
        }
    }

    /// Brings back the tabs, layouts and panes the last daemon wrote. A pane
    /// that was running shows as lost until its holder is adopted.
    /// Returns the agent sessions to bring back once holders are adopted.
    fn restore(&mut self, saved: Saved) -> Vec<(PaneId, crate::store::SavedAgent)> {
        let mut agents = Vec::new();
        self.restore_projects(saved.projects, saved.worktrees);
        for workspace in saved.workspaces {
            self.workspaces.insert(
                workspace.id,
                Workspace {
                    directory: workspace.directory,
                    name: workspace.name,
                    project: workspace.project,
                    worktree: workspace
                        .worktree
                        .filter(|worktree| self.worktrees.contains_key(worktree)),
                    active_tab: workspace.active_tab,
                },
            );
        }
        for tab in saved.tabs {
            self.tabs.push((
                tab.id,
                Tab {
                    workspace: tab.workspace,
                    name: tab.name,
                    layout: tab.layout,
                    lease: None,
                    area: DEFAULT_AREA,
                    focus: tab.focus,
                },
            ));
        }
        for mut pane in saved.panes {
            if let Some(agent) = pane.agent.take() {
                agents.push((pane.id, agent));
            }
            // Until a holder is adopted, the pane shows its history.
            let screen = history_screen(&self.paths.pane_history(pane.id), pane.cols, pane.rows);
            self.panes.insert(
                pane.id,
                Pane {
                    agent: crate::agents::PaneAgent::default(),
                    link: None,
                    cols: pane.cols,
                    rows: pane.rows,
                    state: match pane.state {
                        PaneState::Running => PaneState::Lost,
                        state => state,
                    },
                    screen,
                    launch: pane.launch,
                    cwd: pane.cwd,
                    title: pane.title,
                    history: pane.history,
                    workspace: pane.workspace,
                    spawn_worktree: pane.spawn_worktree,
                    env: pane.env,
                },
            );
        }
        agents
    }

    /// Takes over every holder that outlived a daemon. A holder with no
    /// pane row gets one, and is shown in the "Recovered" tab.
    fn adopt(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.paths.panes_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(pane) = stem.parse::<PaneId>() else {
                continue;
            };
            let (link, welcome) = match HolderLink::connect(
                path.clone(),
                pane,
                self.build.clone(),
                self.events.clone(),
            ) {
                Ok(connected) => connected,
                Err(error) => {
                    match error.downcast_ref::<Legacy>() {
                        // A holder from a build too old to adopt keeps
                        // running; the user decides when it goes.
                        Some(legacy) => self.legacy.push(LegacyHolder {
                            socket: path.to_string_lossy().into_owned(),
                            major: legacy.major,
                            pid: legacy.pid,
                        }),
                        // A holder that will not answer leaves a stale socket.
                        None => {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                    continue;
                }
            };
            let (cols, rows) = holders::screen_size(&welcome.snapshot);
            if let Some(entry) = self.panes.get_mut(&pane) {
                entry.link = Some(link);
                entry.state = PaneState::Running;
                entry.screen = Some(welcome.snapshot);
                entry.cols = cols;
                entry.rows = rows;
                continue;
            }
            let row = SavedPane {
                id: pane,
                workspace: self.default_workspace,
                cwd: String::new(),
                spawn_worktree: None,
                env: Vec::new(),
                launch: Launch::Unknown,
                state: PaneState::Running,
                title: String::new(),
                cols,
                rows,
                history: false,
                agent: None,
            };
            if let Err(error) = self.store.new_pane(&row, Some(pane)) {
                eprintln!("twyx daemon: {error:#}");
            }
            self.panes.insert(
                pane,
                Pane {
                    agent: crate::agents::PaneAgent::default(),
                    link: Some(link),
                    cols,
                    rows,
                    state: PaneState::Running,
                    screen: Some(welcome.snapshot),
                    launch: Launch::Unknown,
                    cwd: String::new(),
                    title: String::new(),
                    history: false,
                    workspace: self.default_workspace,
                    spawn_worktree: None,
                    env: Vec::new(),
                },
            );
        }
    }

    /// Makes the layouts and the panes agree: a leaf without a pane goes, a
    /// live pane without a leaf goes into the "Recovered" tab, and an ended
    /// one without a leaf is forgotten.
    fn reconcile(&mut self) {
        for (_, tab) in &mut self.tabs {
            for leaf in tab.layout.leaves() {
                let known = match leaf.content {
                    Content::Pane(pane) => self.panes.contains_key(&pane),
                    Content::Review(worktree) => self.worktrees.contains_key(&worktree),
                };
                if !known {
                    tab.layout.remove(leaf.id);
                }
            }
        }
        let shown: Vec<PaneId> = self
            .tabs
            .iter()
            .flat_map(|(_, tab)| tab.layout.panes())
            .collect();
        let unshown: Vec<PaneId> = self
            .panes
            .keys()
            .filter(|pane| !shown.contains(pane))
            .copied()
            .collect();
        let mut recovered = Vec::new();
        for pane in unshown {
            if self
                .panes
                .get(&pane)
                .is_some_and(|entry| entry.link.is_some())
            {
                recovered.push(pane);
            } else {
                self.panes.remove(&pane);
            }
        }
        if !recovered.is_empty()
            && let Some(tab) = self.add_tab(self.default_workspace, "Recovered".to_owned())
        {
            for pane in recovered {
                let leaf = self.new_leaf(pane);
                if let Some(tab) = self.tab_mut(tab) {
                    match tab.focus {
                        Some(last) => tab.layout.stack(last, leaf),
                        None => tab.layout.split(None, leaf, Axis::Row),
                    }
                    tab.focus = Some(leaf.id);
                }
            }
        }
        self.tabs.retain(|(_, tab)| !tab.layout.is_empty());
        let workspaces: Vec<u64> = self.workspaces.keys().copied().collect();
        for workspace in workspaces {
            let tab = self.landing_tab(workspace);
            if let Some(entry) = self.workspaces.get_mut(&workspace) {
                entry.active_tab = tab;
            }
        }
    }

    /// Writes the workspace. A failed write keeps the daemon running; the
    /// next change writes again.
    pub(crate) fn save(&mut self) {
        let saved = Saved {
            default_workspace: self.default_workspace,
            active_workspace: self.active_workspace,
            projects: Vec::new(),
            worktrees: Vec::new(),
            workspaces: self
                .workspaces
                .iter()
                .map(|(id, workspace)| SavedWorkspace {
                    id: *id,
                    directory: workspace.directory.clone(),
                    name: workspace.name.clone(),
                    project: workspace.project,
                    worktree: workspace.worktree,
                    active_tab: workspace.active_tab,
                })
                .collect(),
            tabs: self
                .tabs
                .iter()
                .map(|(id, tab)| SavedTab {
                    id: *id,
                    workspace: tab.workspace,
                    name: tab.name.clone(),
                    layout: tab.layout.clone(),
                    focus: tab.focus,
                })
                .collect(),
            panes: self
                .panes
                .iter()
                .map(|(id, pane)| SavedPane {
                    id: *id,
                    workspace: pane.workspace,
                    cwd: pane.cwd.clone(),
                    spawn_worktree: pane.spawn_worktree,
                    env: pane.env.clone(),
                    launch: pane.launch.clone(),
                    state: pane.state.clone(),
                    title: pane.title.clone(),
                    cols: pane.cols,
                    rows: pane.rows,
                    history: pane.history,
                    agent: pane
                        .agent
                        .session
                        .as_ref()
                        .or(pane.agent.ended.as_ref())
                        .and_then(crate::agents::Session::to_saved),
                })
                .collect(),
            next_leaf: self.next_leaf,
        };
        if let Err(error) = self.store.save(&saved) {
            eprintln!("twyx daemon: {error:#}");
        }
    }

    /// Reserves the row, and so the id, of a new pane.
    pub(crate) fn reserve_pane(&self, pane: &Pane) -> Result<PaneId> {
        let row = SavedPane {
            id: PaneId::from_raw(0),
            workspace: pane.workspace,
            cwd: pane.cwd.clone(),
            spawn_worktree: pane.spawn_worktree,
            env: pane.env.clone(),
            launch: pane.launch.clone(),
            state: PaneState::Running,
            title: String::new(),
            cols: pane.cols,
            rows: pane.rows,
            history: pane.history,
            agent: None,
        };
        self.store.new_pane(&row, None)
    }

    fn serve(&mut self, inbound: &Receiver<Event>) -> Result<()> {
        while let Ok(event) = inbound.recv() {
            // Screens are not state; everything else is written after it lands.
            let persist = !matches!(
                &event,
                Event::Holder {
                    message: FromHolder::Frame { .. } | FromHolder::Snapshot { .. },
                    ..
                } | Event::Tick
            );
            match event {
                Event::ClientHello { client, hello, out } => self.client_hello(client, hello, out),
                Event::ClientMessage { client, message } => {
                    if message == ToDaemon::Shutdown {
                        return Ok(());
                    }
                    self.client_message(client, message);
                }
                Event::ClientGone { client } => self.client_gone(client),
                Event::Holder { pane, message } => self.holder_message(pane, message),
                Event::HolderGone { pane } => self.holder_gone(pane),
                Event::Git(done) => self.git_done(done),
                Event::Tick => {
                    self.tick();
                    self.check_deadlines();
                }
            }
            if persist {
                self.save();
                self.settle_inbox();
                self.send_overview();
            }
            for (out, message) in self.outbox.take() {
                let _ = out.send(message);
            }
        }
        Ok(())
    }

    fn client_hello(&mut self, id: u64, hello: ClientHello, out: Sender<FromDaemon>) {
        self.post(
            &out,
            FromDaemon::Welcome(ServerHello {
                major: MAJOR,
                minor: MINOR,
                build: self.build.clone(),
                pid: std::process::id(),
                client: id,
                panes: self.pane_infos(),
                clients: self
                    .clients
                    .values()
                    .filter(|client| client.interactive)
                    .count() as u32,
                legacy: self.legacy.clone(),
            }),
        );
        let workspace = hello
            .workspace
            .filter(|workspace| hello.interactive && self.workspaces.contains_key(workspace))
            .unwrap_or(self.active_workspace);
        // The workspace an interactive client shows is the one the next
        // client shows, as after a switch.
        if hello.interactive {
            self.active_workspace = workspace;
        }
        let tab = self.landing_tab(workspace);
        self.uses += 1;
        self.clients.insert(
            id,
            Client {
                out: out.clone(),
                cols: hello.cols,
                rows: hello.rows,
                interactive: hello.interactive,
                workspace,
                tab,
                focus: BTreeMap::new(),
                used: self.uses,
                watching: false,
                sidebar: false,
                focused: true,
                seen: std::collections::BTreeSet::new(),
                colours: None,
            },
        );
        if !hello.interactive {
            self.post(&out, FromDaemon::Workspace(self.workspace_info(workspace)));
            return;
        }
        if let Some(tab) = tab
            && self.tab(tab).is_some_and(|tab| tab.lease.is_none())
        {
            self.take_lease(tab, id);
        }
        self.post(&out, FromDaemon::Workspace(self.workspace_info(workspace)));
        self.send_focus(id);

        for (pane, entry) in &self.panes {
            if let Some(screen) = &entry.screen {
                self.post(
                    &out,
                    FromDaemon::Snapshot {
                        pane: pane.raw(),
                        seq: 0,
                        body: screen.clone(),
                    },
                );
            }
            if let Some(link) = &entry.link {
                link.send(ToHolder::Snapshot);
            }
        }
    }

    fn client_message(&mut self, id: u64, message: ToDaemon) {
        match message {
            ToDaemon::Hello(_) | ToDaemon::Shutdown => {}
            ToDaemon::Notify {
                pane,
                agent,
                event,
                session,
            } => {
                if let Err(reason) = self.notify(PaneId::from_raw(pane), agent, event, session) {
                    self.refuse(id, reason);
                }
            }
            ToDaemon::ShowSidebar { shown } => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.sidebar = shown;
                }
                if shown {
                    let all: Vec<u64> = self.worktrees.keys().copied().collect();
                    for worktree in all {
                        self.request_status(worktree);
                    }
                }
            }
            ToDaemon::WatchOverview => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.watching = true;
                }
                self.reply(id, FromDaemon::Overview(self.overview_now()));
                self.reply(id, FromDaemon::Inbox(self.inbox_items()));
            }
            ToDaemon::Inbox => self.reply(id, FromDaemon::Inbox(self.inbox_items())),
            ToDaemon::MarkUnread { item } => self.set_read(item, false),
            ToDaemon::Dismiss { item } => self.dismiss(item),
            ToDaemon::Review { worktree } => self.review(id, worktree, None),
            ToDaemon::Files { workspace } => self.files(id, workspace),
            ToDaemon::FindInFiles { workspace, query } => self.find_in_files(id, workspace, query),
            ToDaemon::OpenInEditor { path, line } => self.open_in_editor(id, path, line),
            ToDaemon::Colours(colours) => self.set_client_colours(id, colours),
            ToDaemon::OpenReview { worktree } => self.open_review(id, worktree),
            ToDaemon::Notes { worktree } => self.notes(id, worktree),
            ToDaemon::AddNote {
                worktree,
                path,
                lines,
                body,
            } => self.add_note(id, worktree, path, lines, body),
            ToDaemon::SendNotes { worktree, pane } => {
                self.send_notes(id, worktree, PaneId::from_raw(pane));
            }
            ToDaemon::Checks { worktree } => self.checks(id, worktree),
            ToDaemon::RunCheck { worktree, name } => self.run_check(id, worktree, name),
            ToDaemon::CancelCheck { worktree, name } => self.cancel_check(id, worktree, &name),
            ToDaemon::TrustChecks { worktree, hash } => self.trust_checks(id, worktree, hash),
            ToDaemon::ShowPane { pane } => self.show_pane(id, PaneId::from_raw(pane)),
            ToDaemon::Stage {
                worktree,
                path,
                hunks,
                unstage,
            } => self.review(
                id,
                worktree,
                Some(crate::review::Change::Stage {
                    path,
                    hunks,
                    unstage,
                }),
            ),
            ToDaemon::StageReviewed { worktree } => {
                self.review(id, worktree, Some(crate::review::Change::StageReviewed));
            }
            ToDaemon::Sync { token } => self.reply(id, FromDaemon::Synced { token }),
            ToDaemon::WorkspaceInfo { workspace } => {
                if self.workspaces.contains_key(&workspace) {
                    let info = self.workspace_info(workspace);
                    self.reply(id, FromDaemon::Workspace(info));
                } else {
                    self.refuse(id, format!("no workspace {workspace}"));
                }
            }
            ToDaemon::ShowTab { tab } => {
                let Some((workspace, leaf)) = self.tab(tab).map(|entry| {
                    (
                        entry.workspace,
                        entry
                            .focus
                            .or_else(|| entry.layout.leaves().first().map(|leaf| leaf.id)),
                    )
                }) else {
                    self.refuse(id, format!("no tab {tab}"));
                    return;
                };
                match leaf {
                    Some(leaf) => self.show_leaf(id, workspace, leaf),
                    None => self.refuse(id, format!("tab {tab} is empty")),
                }
            }
            ToDaemon::SendToPane { pane, input } => {
                let pane = PaneId::from_raw(pane);
                let Some(link) = self
                    .panes
                    .get(&pane)
                    .filter(|entry| entry.state == PaneState::Running)
                    .and_then(|entry| entry.link.as_ref())
                else {
                    self.refuse(id, format!("pane {pane} is not running"));
                    return;
                };
                // Not the user's own keys, so an integration's state stays.
                for event in input {
                    link.send(ToHolder::Input(event));
                }
            }
            ToDaemon::Commit { worktree, message } => self.commit(id, worktree, message),
            ToDaemon::Push { worktree } => self.push(id, worktree),
            ToDaemon::OpenPullRequest {
                worktree,
                title,
                body,
            } => self.open_pull_request(id, worktree, title, body),
            ToDaemon::MarkReviewed {
                worktree,
                path,
                hunks,
            } => self.review(id, worktree, Some(crate::review::Change::Mark(path, hunks))),
            ToDaemon::NewTask {
                workspace,
                name,
                start,
                agents,
                prompt,
            } => {
                let project = self
                    .workspaces
                    .get(&workspace)
                    .and_then(|entry| entry.project)
                    .filter(|project| self.projects.get(project).is_some_and(|p| p.git));
                match project {
                    Some(project) => self.new_task(id, project, &name, start, agents, prompt),
                    None => self.refuse(id, "this workspace is not in a Git repository".to_owned()),
                }
            }
            ToDaemon::Resize { cols, rows } => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.cols = cols;
                    client.rows = rows;
                }
                let leased: Vec<u64> = self
                    .tabs
                    .iter()
                    .filter(|(_, tab)| tab.lease == Some(id))
                    .map(|(tab, _)| *tab)
                    .collect();
                for tab in leased {
                    self.apply_size(tab);
                }
                self.broadcast_workspace();
            }
            ToDaemon::Input(InputEvent::Focus(focused)) => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.focused = focused;
                }
                if let Some(pane) = self.focused_pane(id)
                    && let Some(link) = self.panes.get(&pane).and_then(|entry| entry.link.as_ref())
                {
                    link.send(ToHolder::Input(InputEvent::Focus(focused)));
                }
            }
            ToDaemon::Input(event) => {
                self.uses += 1;
                if let Some(client) = self.clients.get_mut(&id) {
                    client.used = self.uses;
                }
                let Some(tab) = self.client(id).and_then(|client| client.tab) else {
                    return;
                };
                // Input from an interactive client takes the lease before the
                // input reaches the pane.
                if self.tab(tab).is_some_and(|tab| tab.lease != Some(id)) {
                    self.take_lease(tab, id);
                }
                let answers = match &event {
                    InputEvent::Key(key) => key.kind != KeyKind::Release,
                    InputEvent::Paste(_) => true,
                    InputEvent::Mouse(_) | InputEvent::Focus(_) => false,
                };
                if let Some(pane) = self.focused_pane(id)
                    && let Some(entry) = self.panes.get(&pane)
                    && let Some(link) = &entry.link
                    && entry.state == PaneState::Running
                {
                    link.send(ToHolder::Input(event));
                    if answers {
                        self.user_input(pane);
                    }
                }
            }
            ToDaemon::TakeLease => {
                if let Some(tab) = self.client(id).and_then(|client| client.tab) {
                    self.take_lease(tab, id);
                }
            }
            ToDaemon::Snapshot { pane } => self.snapshot(id, PaneId::from_raw(pane)),
            ToDaemon::NewPane {
                launch,
                cwd,
                place,
                history,
            } => self.new_pane(id, launch, cwd, place, history),
            ToDaemon::PurgeHistory => {
                // Running holders write a new history from their next
                // checkpoint.
                let _ = std::fs::remove_dir_all(self.paths.history());
            }
            ToDaemon::KillPane { pane } => {
                if let Some(entry) = self.panes.get(&PaneId::from_raw(pane))
                    && let Some(link) = &entry.link
                {
                    link.send(ToHolder::Kill);
                }
            }
            ToDaemon::ClosePane => {
                if let Some((tab, leaf)) = self.client_focus(id) {
                    self.close_leaf(tab, leaf);
                }
            }
            ToDaemon::RunAgain { pane } => self.run_again(id, PaneId::from_raw(pane)),
            ToDaemon::Resume { panes } => {
                for pane in panes {
                    self.resume(id, PaneId::from_raw(pane));
                }
            }
            ToDaemon::Focus { side } => self.focus_side(id, side),
            ToDaemon::FocusLeaf { leaf } => self.focus_leaf(id, leaf),
            ToDaemon::ResizePane { side } => self.resize_pane(id, side),
            ToDaemon::SwapPane { side } => self.swap_pane(id, side),
            ToDaemon::ToggleZoom => self.edit_focused(id, |layout, leaf| layout.toggle_zoom(leaf)),
            ToDaemon::ToggleFloating => {
                self.edit_focused(id, |layout, leaf| layout.toggle_floating(leaf));
            }
            ToDaemon::MoveFloating { leaf, geometry } => {
                self.edit_leaf(leaf, |layout, _| layout.move_floating(leaf, geometry));
            }
            ToDaemon::DropLeaf { leaf, drop } => {
                self.edit_leaf(leaf, |layout, _| layout.drop_leaf(leaf, drop));
            }
            ToDaemon::DragRule { leaf, side, cells } => {
                self.edit_leaf(leaf, |layout, area| {
                    layout.drag_rule(leaf, side, cells, area)
                });
            }
            ToDaemon::SelectTab { tab } => self.select_tab(id, tab),
            ToDaemon::RenameTab { tab, name } => {
                if let Some(entry) = self.tab_mut(tab) {
                    entry.name = name;
                }
                self.broadcast_workspace();
            }
            ToDaemon::CloseTab { tab } => self.close_tab(tab),
            ToDaemon::CloseFocusedTab => {
                if let Some(tab) = self.client(id).and_then(|client| client.tab) {
                    self.close_tab(tab);
                }
            }
            ToDaemon::Copy(action) => {
                if let Some(pane) = self.focused_pane(id)
                    && let Some(entry) = self.panes.get(&pane)
                    && let Some(link) = &entry.link
                {
                    link.send(ToHolder::Copy(action));
                }
            }
            ToDaemon::KillLegacy => {
                for holder in std::mem::take(&mut self.legacy) {
                    if let Some(pid) = holder
                        .pid
                        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
                    {
                        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
                    }
                    let _ = std::fs::remove_file(&holder.socket);
                }
            }
            ToDaemon::CycleTab { forward } => self.cycle_tab(id, forward),
            ToDaemon::SelectTabAt { index } => {
                if let Some(tab) = self.workspace_tabs(self.client_workspace(id)).get(index) {
                    self.select_tab(id, *tab);
                }
            }
            ToDaemon::NudgeFloating { side } => {
                self.edit_focused(id, |layout, leaf| layout.nudge_floating(leaf, side));
            }
            ToDaemon::Open { path, switch } => self.open(id, path, switch),
            ToDaemon::SwitchWorkspace { workspace } => {
                if !self.workspaces.contains_key(&workspace) {
                    self.refuse(id, format!("no workspace {workspace}"));
                    return;
                }
                let shown = self.switch_for(id, workspace, true);
                self.reply(
                    id,
                    FromDaemon::Switched {
                        workspace,
                        client: shown,
                    },
                );
            }
            ToDaemon::Overview { refresh } => self.overview(id, refresh),
            ToDaemon::NewWorktree {
                project,
                branch,
                start,
            } => self.new_worktree(id, project, branch, start, None),
            ToDaemon::PlanRemoval { worktree } => self.plan_removal(id, worktree),
            ToDaemon::RemoveWorktree {
                plan,
                delete_branch,
            } => self.remove_worktree(id, plan, delete_branch),
            ToDaemon::Setup { worktree } => self.setup_file(id, worktree),
            ToDaemon::TrustSetup { worktree, hash } => self.trust_setup(id, worktree, hash),
            ToDaemon::RunSetup { worktree } => {
                if self.worktrees.contains_key(&worktree) {
                    self.prepare(worktree, crate::setup::After::Again { client: id });
                } else {
                    self.refuse(id, format!("no worktree {worktree}"));
                }
            }
            ToDaemon::Recipes { workspace } => self.list_recipes(id, workspace),
            ToDaemon::RunRecipe {
                workspace,
                name,
                branch,
                switch,
            } => self.run_recipe(id, workspace, name, branch, switch),
            ToDaemon::TrustRecipe {
                workspace,
                name,
                hash,
            } => self.trust_recipe(id, workspace, name, hash),
        }
    }

    fn snapshot(&self, id: u64, pane: PaneId) {
        let Some(entry) = self.panes.get(&pane) else {
            return;
        };
        match &entry.link {
            Some(link) => link.send(ToHolder::Snapshot),
            None => {
                if let Some(screen) = &entry.screen
                    && let Some(client) = self.clients.get(&id)
                {
                    self.post(
                        &client.out,
                        FromDaemon::Snapshot {
                            pane: pane.raw(),
                            seq: 0,
                            body: screen.clone(),
                        },
                    );
                }
            }
        }
    }

    fn client_gone(&mut self, id: u64) {
        self.waiting.retain(|waiting| waiting.client != id);
        let Some(gone) = self.clients.remove(&id) else {
            return;
        };
        if !gone.interactive {
            return;
        }
        let leased: Vec<u64> = self
            .tabs
            .iter()
            .filter(|(_, tab)| tab.lease == Some(id))
            .map(|(tab, _)| *tab)
            .collect();
        for tab in leased {
            if let Some(entry) = self.tab_mut(tab) {
                entry.lease = None;
            }
            // A disconnect releases the lease to the next client on the tab.
            let next = self
                .clients
                .iter()
                .find(|(_, client)| client.interactive && client.tab == Some(tab))
                .map(|(next, _)| *next);
            if let Some(next) = next {
                self.take_lease(tab, next);
            }
        }
        self.broadcast_workspace();
    }

    pub(crate) fn take_lease(&mut self, tab: u64, id: u64) {
        if !self
            .clients
            .get(&id)
            .is_some_and(|client| client.interactive)
        {
            return;
        }
        let Some(entry) = self.tab_mut(tab) else {
            return;
        };
        entry.lease = Some(id);
        self.apply_size(tab);
        self.send_tab_colours(tab);
        self.broadcast_workspace();
    }

    /// Sizes every pane in a tab to its place in the leaseholder's area.
    pub(crate) fn apply_size(&mut self, tab: u64) {
        let lease_area = self
            .tab(tab)
            .and_then(|entry| entry.lease)
            .and_then(|lease| self.clients.get(&lease))
            .map(|client| (client.cols, client.rows))
            .filter(|(cols, rows)| *cols > 0 && *rows > 0);
        let Some(entry) = self.tabs.iter_mut().find(|(id, _)| *id == tab) else {
            return;
        };
        let entry = &mut entry.1;
        if let Some(area) = lease_area {
            entry.area = area;
        }
        let placed = entry.layout.place(area_cells(entry.area));
        let mut resized = Vec::new();
        for placed in placed {
            let cols = placed.inner.width.max(2);
            let rows = placed.inner.height.max(1);
            let Some(id) = placed.leaf.terminal() else {
                continue;
            };
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            if pane.cols == cols && pane.rows == rows {
                continue;
            }
            pane.cols = cols;
            pane.rows = rows;
            if let Some(link) = &pane.link {
                link.send(ToHolder::Resize { cols, rows });
            }
            resized.push(id);
        }
        for pane in resized {
            self.broadcast_pane(pane);
        }
    }

    fn holder_message(&mut self, pane: PaneId, message: FromHolder) {
        if self.closing.contains_key(&pane) {
            if let FromHolder::Exited(_) = message
                && let Some(link) = self.closing.remove(&pane)
            {
                link.send(ToHolder::AckExit);
                // A checkpoint between the close and the exit may have
                // written the history again.
                let _ = std::fs::remove_dir_all(self.paths.pane_history(pane));
                self.advance_removals();
            }
            return;
        }
        match message {
            FromHolder::Welcome(_) => {}
            FromHolder::Frame { seq, body } => {
                self.broadcast_frames(FromDaemon::Frame {
                    pane: pane.raw(),
                    seq,
                    body,
                });
            }
            FromHolder::Snapshot { seq, body } => {
                if let Some(entry) = self.panes.get_mut(&pane) {
                    entry.screen = Some(body.clone());
                }
                self.broadcast_frames(FromDaemon::Snapshot {
                    pane: pane.raw(),
                    seq,
                    body,
                });
            }
            FromHolder::Signal(signal) => self.signal(pane, signal),
            FromHolder::Exited(state) => self.exited(pane, state),
            FromHolder::Copied(text) => {
                self.broadcast_frames(FromDaemon::Clipboard {
                    pane: pane.raw(),
                    text,
                });
            }
            FromHolder::Injected { id, result } => self.injected(id, result),
            FromHolder::Tail(tail) if self.ships(pane) => self.ship_tail(pane, tail),
            FromHolder::Tail(tail) => self.check_tail(pane, tail),
            FromHolder::Unsupported { .. } | FromHolder::Pong => {}
        }
    }

    fn exited(&mut self, pane: PaneId, state: ExitState) {
        let state = match state {
            ExitState::Exited { code } => PaneState::Exited { code },
            ExitState::Killed { signal } => PaneState::Killed { signal },
        };
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        entry.state = state;
        let link = entry.link.take();
        let shell = entry.launch == Launch::Shell;
        let failed = !matches!(entry.state, PaneState::Exited { code: 0 });
        let ended = entry.state.clone();
        let check = self.check_runs.contains_key(&pane);
        let exit = match ended {
            PaneState::Exited { code } => Some(code),
            _ => None,
        };
        // The daemon has the exit status, so the holder may go; a check's
        // holder first sends the output's tail.
        let link = self.check_exited(pane, exit, link);
        if let Some(link) = self.ship_exited(pane, exit, link) {
            link.send(ToHolder::AckExit);
        }
        // Process exit always ends the agent session.
        self.pane_ended(pane);
        if failed && !shell && !check {
            self.raise(pane, twyx_core::inbox::Reason::Failed);
        }
        self.setup_ended(pane, Some(&ended));
        if shell && let Some((tab, leaf)) = self.leaf_of(pane) {
            self.close_leaf(tab, leaf.id);
            return;
        }
        self.broadcast_pane(pane);
    }

    /// Deletes the history of panes that no longer exist, such as one
    /// closed while the daemon went down (Invariant 22).
    fn sweep_history(&self) {
        let Ok(entries) = std::fs::read_dir(self.paths.history()) else {
            return;
        };
        for entry in entries.flatten() {
            let known = entry
                .file_name()
                .to_str()
                .and_then(|stem| stem.parse::<PaneId>().ok())
                .is_some_and(|pane| self.panes.contains_key(&pane));
            if !known {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    fn holder_gone(&mut self, pane: PaneId) {
        if self.closing.remove(&pane).is_some() {
            // A checkpoint after the close may have written the history again.
            let _ = std::fs::remove_dir_all(self.paths.pane_history(pane));
            self.advance_removals();
        }
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        if entry.state == PaneState::Running {
            entry.state = PaneState::Lost;
            entry.link = None;
            self.pane_ended(pane);
            self.check_gone(pane);
            self.ship_gone(pane);
            // A holder that went without an exit status crashed or was killed.
            if self.panes.contains_key(&pane) {
                self.raise(pane, twyx_core::inbox::Reason::Failed);
            }
            self.broadcast_pane(pane);
        }
    }

    fn signal(&mut self, pane: PaneId, signal: twyx_protocol::holder::Signal) {
        use twyx_core::config::Osc52;
        use twyx_protocol::holder::Signal;
        match signal {
            Signal::ClipboardWrite(text) => {
                let message = FromDaemon::Clipboard {
                    pane: pane.raw(),
                    text,
                };
                match self.config.osc52 {
                    Osc52::Deny => {}
                    Osc52::Allow => self.broadcast_frames(message),
                    Osc52::Focused => {
                        for (id, client) in &self.clients {
                            if client.interactive && self.focused_pane(*id) == Some(pane) {
                                self.post(&client.out, message.clone());
                            }
                        }
                    }
                }
            }
            Signal::Title(title) => {
                if let Some(entry) = self.panes.get_mut(&pane) {
                    entry.title = title;
                }
                self.broadcast_pane(pane);
            }
            Signal::Bell | Signal::Notification { .. } => {
                self.terminal(pane, Terminal::Attention);
            }
            Signal::PromptMark(mark) => self.terminal(pane, Terminal::Mark(mark)),
            Signal::Foreground { pid, name } => self.foreground(pane, pid, &name),
            Signal::Screen(found) => self.screen(pane, found),
            Signal::WorkingDirectory(_) | Signal::Link(_) => {}
        }
    }

    pub(crate) fn client(&self, id: u64) -> Option<&Client> {
        self.clients.get(&id)
    }

    pub(crate) fn tab(&self, tab: u64) -> Option<&Tab> {
        self.tabs
            .iter()
            .find(|(id, _)| *id == tab)
            .map(|(_, tab)| tab)
    }

    pub(crate) fn tab_mut(&mut self, tab: u64) -> Option<&mut Tab> {
        self.tabs
            .iter_mut()
            .find(|(id, _)| *id == tab)
            .map(|(_, tab)| tab)
    }

    pub(crate) fn add_tab(&mut self, workspace: u64, name: String) -> Option<u64> {
        let id = match self.store.new_tab(workspace) {
            Ok(id) => id,
            Err(error) => {
                eprintln!("twyx daemon: {error:#}");
                return None;
            }
        };
        self.tabs.push((
            id,
            Tab {
                workspace,
                name,
                layout: Layout::default(),
                lease: None,
                area: DEFAULT_AREA,
                focus: None,
            },
        ));
        Some(id)
    }

    pub(crate) fn new_leaf(&mut self, pane: PaneId) -> Leaf {
        let id = LeafId(self.next_leaf);
        self.next_leaf += 1;
        Leaf::pane(id, pane)
    }

    pub(crate) fn new_review_leaf(&mut self, worktree: u64) -> Leaf {
        let id = LeafId(self.next_leaf);
        self.next_leaf += 1;
        Leaf {
            id,
            content: Content::Review(worktree),
        }
    }

    /// The tab and leaf that show a pane.
    pub(crate) fn leaf_of(&self, pane: PaneId) -> Option<(u64, Leaf)> {
        self.tabs
            .iter()
            .find_map(|(id, tab)| tab.layout.find(pane).map(|leaf| (*id, leaf)))
    }

    /// A client's tab and its focus there: its own, else the tab's last, else
    /// the first leaf.
    pub(crate) fn client_focus(&self, id: u64) -> Option<(u64, LeafId)> {
        let client = self.clients.get(&id)?;
        let tab_id = client.tab.or_else(|| self.landing_tab(client.workspace))?;
        let tab = self.tab(tab_id)?;
        let leaf = client
            .focus
            .get(&tab_id)
            .copied()
            .filter(|leaf| tab.layout.contains(*leaf))
            .or(tab.focus.filter(|leaf| tab.layout.contains(*leaf)))
            .or_else(|| tab.layout.leaves().first().map(|leaf| leaf.id))?;
        Some((tab_id, leaf))
    }

    fn focused_pane(&self, id: u64) -> Option<PaneId> {
        let (tab, leaf) = self.client_focus(id)?;
        self.tab(tab)?.layout.leaf(leaf)?.terminal()
    }

    pub(crate) fn send_focus(&self, id: u64) {
        let Some(client) = self.clients.get(&id) else {
            return;
        };
        let focus = self.client_focus(id);
        self.post(
            &client.out,
            FromDaemon::Focus {
                tab: focus.map(|(tab, _)| tab).or(client.tab),
                leaf: focus.map(|(_, leaf)| leaf),
            },
        );
    }

    pub(crate) fn pane_info(&self, pane: PaneId, entry: &Pane) -> PaneInfo {
        PaneInfo {
            pane: pane.raw(),
            cols: entry.cols,
            rows: entry.rows,
            state: entry.state.clone(),
            launch: entry.launch.clone(),
            cwd: entry.cwd.clone(),
            title: entry.title.clone(),
            agent: entry
                .agent
                .session
                .as_ref()
                .map(crate::agents::Session::info),
            resume: self.resume_argv(pane).map(|(kind, ..)| kind),
        }
    }

    fn pane_infos(&self) -> Vec<PaneInfo> {
        self.panes
            .iter()
            .map(|(pane, entry)| self.pane_info(*pane, entry))
            .collect()
    }

    pub(crate) fn broadcast_pane(&self, pane: PaneId) {
        if let Some(entry) = self.panes.get(&pane) {
            self.broadcast(FromDaemon::Pane(self.pane_info(pane, entry)));
        }
    }

    pub(crate) fn post(&self, out: &Sender<FromDaemon>, message: FromDaemon) {
        self.outbox.borrow_mut().push((out.clone(), message));
    }

    pub(crate) fn broadcast(&self, message: FromDaemon) {
        for client in self.clients.values() {
            self.post(&client.out, message.clone());
        }
    }

    /// Screens go only to interactive clients.
    pub(crate) fn broadcast_frames(&self, message: FromDaemon) {
        for client in self.clients.values().filter(|client| client.interactive) {
            self.post(&client.out, message.clone());
        }
    }
}

/// A pane's last screen from its history, as a snapshot body, for a pane
/// whose holder is gone.
fn history_screen(dir: &Path, cols: u16, rows: u16) -> Option<Vec<u8>> {
    let (checkpoint, log) = twyx_holder::history::read(dir).ok()??;
    let geometry = twyx_holder::engine::Geometry {
        cols: cols.max(2),
        rows: rows.max(1),
        scrollback_lines: 0,
    };
    let mut engine = twyx_holder::engine::Engine::restore(
        geometry,
        Box::new(std::io::sink()),
        &checkpoint,
        &log,
    )?;
    let view = engine.view();
    let snapshot =
        twyx_holder::frame::FrameBuilder::new(view.cols, view.rows.len() as u16).snapshot(&view);
    wire::encode_body(&snapshot).ok()
}

pub(crate) fn area_cells((cols, rows): (u16, u16)) -> Cells {
    Cells {
        x: 0,
        y: 0,
        width: cols,
        height: rows,
    }
}

fn spawn_acceptor(listener: UnixListener, events: Sender<Event>) -> Result<()> {
    thread::Builder::new()
        .name("twyx-daemon-accept".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let events = events.clone();
                // Each client needs its own reader, or a quiet client blocks
                // every other one.
                thread::spawn(move || serve_client(stream, events));
            }
        })
        .context("start the daemon acceptor")?;
    Ok(())
}

fn serve_client(stream: UnixStream, events: Sender<Event>) {
    if peer::accept(&stream).is_err() {
        return;
    }
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    // A client protocol frame starts with the high byte of its length,
    // which is zero for any message the daemon accepts; API v1 starts
    // with a JSON object.
    if reader
        .fill_buf()
        .ok()
        .and_then(|bytes| bytes.first().copied())
        == Some(b'{')
    {
        let client = CLIENTS.fetch_add(1, Ordering::Relaxed);
        crate::api::serve(stream, reader, events, client);
        return;
    }
    let hello = match wire::read::<ToDaemon>(&mut reader) {
        Ok(Some(ToDaemon::Hello(hello))) => hello,
        _ => return,
    };
    if hello.major != MAJOR {
        let mut writer = BufWriter::new(stream);
        let _ = wire::write(
            &mut writer,
            &FromDaemon::Unsupported {
                major: MAJOR,
                minor: MINOR,
                pid: std::process::id(),
            },
        );
        return;
    }

    let client = CLIENTS.fetch_add(1, Ordering::Relaxed);
    let (out, queued) = mpsc::channel::<FromDaemon>();
    // A client that stops reading is dropped, not buffered without end.
    let _ = stream.set_write_timeout(Some(twyx_core::tuning::CLIENT_WRITE_TIMEOUT));
    let mut writer = BufWriter::new(stream);
    thread::spawn(move || {
        for message in queued {
            if wire::write(&mut writer, &message).is_err() {
                break;
            }
        }
    });

    if events
        .send(Event::ClientHello {
            client,
            hello,
            out: out.clone(),
        })
        .is_err()
    {
        return;
    }

    while let Ok(Some(message)) = wire::read::<ToDaemon>(&mut reader) {
        if events
            .send(Event::ClientMessage { client, message })
            .is_err()
        {
            return;
        }
    }
    let _ = events.send(Event::ClientGone { client });
}
