//! The daemon: the workspace's tabs and layouts, their panes, the clients
//! attached to them, and one size lease per tab.
//!
//! Threads: each client and each holder gets a reader thread and a writer
//! thread. This thread owns the state and forwards frames without reading
//! them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};

use twyx_core::layout::{Axis, Cells, Layout, Leaf, LeafId};
use twyx_core::pane::PaneId;
use twyx_core::paths::Paths;
use twyx_protocol::client::{
    ClientHello, FromDaemon, Launch, MAJOR, MINOR, PaneInfo, PaneState, ServerHello, TabInfo,
    ToDaemon, WorkspaceInfo,
};
use twyx_protocol::holder::{ExitState, FromHolder, ToHolder};
use twyx_protocol::wire;

use crate::holders::{self, HolderLink};
use crate::peer;
use crate::store::{Saved, SavedPane, SavedTab, Store};

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
}

#[derive(Debug)]
pub(crate) struct Tab {
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
    pub tab: Option<u64>,
    /// This client's focus in each tab it has shown.
    pub focus: BTreeMap<u64, LeafId>,
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
    /// The tabs of the default workspace, in order.
    pub(crate) tabs: Vec<(u64, Tab)>,
    /// The tab a new client shows.
    pub(crate) active_tab: Option<u64>,
    pub(crate) next_leaf: u64,
    pub(crate) store: Store,
    /// The default workspace's row.
    pub(crate) workspace: u64,
    /// Messages for clients, sent once the event that made them is written,
    /// so no client sees a change a crash could still lose.
    pub(crate) outbox: RefCell<Vec<(Sender<FromDaemon>, FromDaemon)>>,
}

/// Starts the daemon and serves it until the socket closes.
pub fn run(paths: Paths) -> Result<()> {
    paths.prepare_runtime()?;
    let listener = bind(&paths.socket())?;
    let holder_binary = std::env::current_exe().context("find the twyx binary")?;
    let build = env!("CARGO_PKG_VERSION").to_owned();
    let (events, inbound) = mpsc::channel();
    let store = Store::open(&paths.database())?;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
    let saved = store.load(&home)?;

    let mut daemon = Daemon {
        paths,
        build,
        holder_binary,
        events: events.clone(),
        panes: BTreeMap::new(),
        closing: BTreeMap::new(),
        clients: BTreeMap::new(),
        tabs: Vec::new(),
        active_tab: None,
        next_leaf: saved.next_leaf,
        store,
        workspace: saved.workspace,
        outbox: RefCell::new(Vec::new()),
    };
    daemon.restore(saved);
    daemon.adopt();
    daemon.reconcile();
    daemon.save();
    spawn_acceptor(listener, events)?;
    daemon.serve(&inbound)
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
    /// Brings back the tabs, layouts and panes the last daemon wrote. A pane
    /// that was running shows as lost until its holder is adopted.
    fn restore(&mut self, saved: Saved) {
        self.active_tab = saved.active_tab;
        for tab in saved.tabs {
            self.tabs.push((
                tab.id,
                Tab {
                    name: tab.name,
                    layout: tab.layout,
                    lease: None,
                    area: DEFAULT_AREA,
                    focus: tab.focus,
                },
            ));
        }
        for pane in saved.panes {
            self.panes.insert(
                pane.id,
                Pane {
                    link: None,
                    cols: pane.cols,
                    rows: pane.rows,
                    state: match pane.state {
                        PaneState::Running => PaneState::Lost,
                        state => state,
                    },
                    screen: None,
                    launch: pane.launch,
                    cwd: pane.cwd,
                    title: pane.title,
                },
            );
        }
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
            let Ok((link, welcome)) =
                HolderLink::connect(path.clone(), pane, self.build.clone(), self.events.clone())
            else {
                // A holder that will not answer leaves a stale socket.
                let _ = std::fs::remove_file(&path);
                continue;
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
                cwd: String::new(),
                launch: Launch::Unknown,
                state: PaneState::Running,
                title: String::new(),
                cols,
                rows,
            };
            if let Err(error) = self.store.new_pane(self.workspace, &row, Some(pane)) {
                eprintln!("twyx daemon: {error:#}");
            }
            self.panes.insert(
                pane,
                Pane {
                    link: Some(link),
                    cols,
                    rows,
                    state: PaneState::Running,
                    screen: Some(welcome.snapshot),
                    launch: Launch::Unknown,
                    cwd: String::new(),
                    title: String::new(),
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
                if !self.panes.contains_key(&leaf.pane) {
                    tab.layout.remove(leaf.id);
                }
            }
        }
        let shown: Vec<PaneId> = self
            .tabs
            .iter()
            .flat_map(|(_, tab)| tab.layout.leaves())
            .map(|leaf| leaf.pane)
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
            && let Some(tab) = self.add_tab("Recovered".to_owned())
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
        if !self.active_tab.is_some_and(|tab| self.tab(tab).is_some()) {
            self.active_tab = self.tabs.first().map(|(id, _)| *id);
        }
    }

    /// Writes the workspace. A failed write keeps the daemon running; the
    /// next change writes again.
    pub(crate) fn save(&mut self) {
        let saved = Saved {
            workspace: self.workspace,
            active_tab: self.active_tab,
            tabs: self
                .tabs
                .iter()
                .map(|(id, tab)| SavedTab {
                    id: *id,
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
                    cwd: pane.cwd.clone(),
                    launch: pane.launch.clone(),
                    state: pane.state.clone(),
                    title: pane.title.clone(),
                    cols: pane.cols,
                    rows: pane.rows,
                })
                .collect(),
            next_leaf: self.next_leaf,
        };
        if let Err(error) = self.store.save(&saved) {
            eprintln!("twyx daemon: {error:#}");
        }
    }

    /// Reserves the row, and so the id, of a new pane.
    pub(crate) fn reserve_pane(
        &self,
        launch: &Launch,
        cwd: &str,
        cols: u16,
        rows: u16,
    ) -> Result<PaneId> {
        let row = SavedPane {
            id: PaneId::from_raw(0),
            cwd: cwd.to_owned(),
            launch: launch.clone(),
            state: PaneState::Running,
            title: String::new(),
            cols,
            rows,
        };
        self.store.new_pane(self.workspace, &row, None)
    }

    fn serve(&mut self, inbound: &Receiver<Event>) -> Result<()> {
        while let Ok(event) = inbound.recv() {
            // Screens are not state; everything else is written after it lands.
            let persist = !matches!(
                &event,
                Event::Holder {
                    message: FromHolder::Frame { .. } | FromHolder::Snapshot { .. },
                    ..
                }
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
            }
            if persist {
                self.save();
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
            }),
        );
        let tab = self
            .active_tab
            .or_else(|| self.tabs.first().map(|(id, _)| *id));
        self.clients.insert(
            id,
            Client {
                out: out.clone(),
                cols: hello.cols,
                rows: hello.rows,
                interactive: hello.interactive,
                tab,
                focus: BTreeMap::new(),
            },
        );
        if !hello.interactive {
            self.post(&out, FromDaemon::Workspace(self.workspace_info()));
            return;
        }
        if let Some(tab) = tab
            && self.tab(tab).is_some_and(|tab| tab.lease.is_none())
        {
            self.take_lease(tab, id);
        }
        self.post(&out, FromDaemon::Workspace(self.workspace_info()));
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
            ToDaemon::Input(event) => {
                let Some(tab) = self.client(id).and_then(|client| client.tab) else {
                    return;
                };
                // Input from an interactive client takes the lease before the
                // input reaches the pane.
                if self.tab(tab).is_some_and(|tab| tab.lease != Some(id)) {
                    self.take_lease(tab, id);
                }
                if let Some(pane) = self.focused_pane(id)
                    && let Some(entry) = self.panes.get(&pane)
                    && let Some(link) = &entry.link
                    && entry.state == PaneState::Running
                {
                    link.send(ToHolder::Input(event));
                }
            }
            ToDaemon::TakeLease => {
                if let Some(tab) = self.client(id).and_then(|client| client.tab) {
                    self.take_lease(tab, id);
                }
            }
            ToDaemon::Snapshot { pane } => self.snapshot(id, PaneId::from_raw(pane)),
            ToDaemon::NewPane { launch, cwd, place } => self.new_pane(id, launch, cwd, place),
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
            ToDaemon::Focus { side } => self.focus_side(id, side),
            ToDaemon::FocusLeaf { leaf } => self.focus_leaf(id, leaf),
            ToDaemon::ResizePane { side } => self.resize_pane(id, side),
            ToDaemon::SwapPane { side } => self.swap_pane(id, side),
            ToDaemon::ToggleZoom => self.edit_focused(id, |layout, leaf| layout.toggle_zoom(leaf)),
            ToDaemon::ToggleFloating => {
                self.edit_focused(id, |layout, leaf| layout.toggle_floating(leaf));
            }
            ToDaemon::MoveFloating { geometry } => {
                self.edit_focused(id, |layout, leaf| layout.move_floating(leaf, geometry));
            }
            ToDaemon::SelectTab { tab } => self.select_tab(id, tab),
            ToDaemon::RenameTab { tab, name } => {
                if let Some(entry) = self.tab_mut(tab) {
                    entry.name = name;
                }
                self.broadcast_workspace();
            }
            ToDaemon::CloseTab { tab } => self.close_tab(tab),
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
            let Some(pane) = self.panes.get_mut(&placed.leaf.pane) else {
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
            resized.push(placed.leaf.pane);
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
        // The daemon has the exit status, so the holder may go.
        if let Some(link) = entry.link.take() {
            link.send(ToHolder::AckExit);
        }
        if entry.launch == Launch::Shell
            && let Some((tab, leaf)) = self.leaf_of(pane)
        {
            self.close_leaf(tab, leaf.id);
            return;
        }
        self.broadcast_pane(pane);
    }

    fn holder_gone(&mut self, pane: PaneId) {
        self.closing.remove(&pane);
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        if entry.state == PaneState::Running {
            entry.state = PaneState::Lost;
            entry.link = None;
            self.broadcast_pane(pane);
        }
    }

    fn signal(&mut self, pane: PaneId, signal: twyx_protocol::holder::Signal) {
        use twyx_protocol::holder::Signal;
        match signal {
            Signal::ClipboardWrite(text) => {
                self.broadcast_frames(FromDaemon::Clipboard {
                    pane: pane.raw(),
                    text,
                });
            }
            Signal::Title(title) => {
                if let Some(entry) = self.panes.get_mut(&pane) {
                    entry.title = title;
                }
                self.broadcast_pane(pane);
            }
            Signal::Bell
            | Signal::Notification { .. }
            | Signal::PromptMark(_)
            | Signal::WorkingDirectory(_)
            | Signal::Link(_)
            | Signal::Foreground { .. } => {}
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

    pub(crate) fn add_tab(&mut self, name: String) -> Option<u64> {
        let id = match self.store.new_tab(self.workspace) {
            Ok(id) => id,
            Err(error) => {
                eprintln!("twyx daemon: {error:#}");
                return None;
            }
        };
        self.tabs.push((
            id,
            Tab {
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
        Leaf { id, pane }
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
        let tab_id = client.tab.or(self.active_tab)?;
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
        self.tab(tab)?.layout.leaf(leaf).map(|leaf| leaf.pane)
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

    fn pane_info(&self, pane: PaneId, entry: &Pane) -> PaneInfo {
        PaneInfo {
            pane: pane.raw(),
            cols: entry.cols,
            rows: entry.rows,
            state: entry.state.clone(),
            launch: entry.launch.clone(),
            cwd: entry.cwd.clone(),
            title: entry.title.clone(),
        }
    }

    fn pane_infos(&self) -> Vec<PaneInfo> {
        self.panes
            .iter()
            .map(|(pane, entry)| self.pane_info(*pane, entry))
            .collect()
    }

    pub(crate) fn workspace_info(&self) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace: 1,
            name: "default".to_owned(),
            tabs: self
                .tabs
                .iter()
                .map(|(id, tab)| TabInfo {
                    tab: *id,
                    name: tab.name.clone(),
                    layout: tab.layout.clone(),
                    lease: tab.lease,
                    cols: tab.area.0,
                    rows: tab.area.1,
                })
                .collect(),
        }
    }

    pub(crate) fn broadcast_workspace(&self) {
        self.broadcast(FromDaemon::Workspace(self.workspace_info()));
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
            },
        );
        return;
    }

    let client = CLIENTS.fetch_add(1, Ordering::Relaxed);
    let (out, queued) = mpsc::channel::<FromDaemon>();
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
