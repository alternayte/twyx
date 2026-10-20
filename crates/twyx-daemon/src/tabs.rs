//! Tab and layout commands: new, close and run-again panes, focus, resize,
//! swap, zoom, float, and the tabs themselves.

use std::path::PathBuf;

use anyhow::Context;

use twyx_core::layout::{Axis, Cells, Layout, LeafId, Side};
use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::client::{FromDaemon, Launch, PaneState, Place};
use twyx_protocol::holder::ToHolder;

use crate::daemon::{Daemon, Pane, area_cells};
use crate::holders;

impl Daemon {
    pub(crate) fn new_pane(
        &mut self,
        id: u64,
        launch: Launch,
        cwd: String,
        place: Place,
        history: bool,
    ) {
        self.reload_config();
        let history = history && self.config.history;
        if argv(&launch).is_none() {
            self.refuse(id, "a pane needs a command".to_owned());
            return;
        }
        let interactive = self.client(id).is_some_and(|client| client.interactive);
        let workspace = self.client_workspace(id);
        let existing = self
            .client(id)
            .and_then(|client| client.tab)
            .or_else(|| self.landing_tab(workspace))
            .filter(|tab| self.tab(*tab).is_some_and(|tab| tab.workspace == workspace));
        // A pane with no directory starts in its workspace's.
        let cwd = match cwd.is_empty() {
            true => self
                .workspaces
                .get(&workspace)
                .map(|entry| entry.directory.clone())
                .unwrap_or_default(),
            false => cwd,
        };
        let (tab, created) = match (place, existing) {
            (Place::NewTab, _) | (_, None) => match self.add_tab(workspace, String::new()) {
                Some(tab) => (tab, true),
                None => {
                    self.refuse(id, "could not record a new tab".to_owned());
                    return;
                }
            },
            (_, Some(tab)) => (tab, false),
        };
        if created {
            if let Some(entry) = self.workspaces.get_mut(&workspace) {
                entry.active_tab = Some(tab);
            }
            if interactive && let Some(client) = self.clients.get_mut(&id) {
                client.tab = Some(tab);
            }
        }
        if interactive && self.tab(tab).is_some_and(|entry| entry.lease.is_none()) {
            self.take_lease(tab, id);
        }

        let focus = self
            .client_focus(id)
            .filter(|(focus_tab, _)| *focus_tab == tab)
            .map(|(_, leaf)| leaf);
        let record = Pane {
            agent: crate::agents::PaneAgent::default(),
            link: None,
            cols: 80,
            rows: 24,
            state: PaneState::Running,
            screen: None,
            spawn_worktree: self.worktree_at(&cwd),
            launch,
            cwd,
            title: String::new(),
            history,
            workspace,
            env: Vec::new(),
        };
        let pane = match self.reserve_pane(&record) {
            Ok(pane) => pane,
            Err(error) => {
                self.refuse(id, format!("{error:#}"));
                self.drop_empty_tab(tab);
                return;
            }
        };
        let leaf = self.new_leaf(pane);
        let Some(entry) = self.tab_mut(tab) else {
            return;
        };
        match (place, focus) {
            (Place::Split(axis), _) => entry.layout.split(focus, leaf, axis),
            (Place::Stack, Some(focus)) => entry.layout.stack(focus, leaf),
            (Place::Stack | Place::NewTab, _) => entry.layout.split(None, leaf, Axis::Row),
            (Place::Floating, _) => entry.layout.float(leaf),
        }
        let inner = entry
            .layout
            .place(area_cells(entry.area))
            .into_iter()
            .find(|placed| placed.leaf.id == leaf.id)
            .map(|placed| placed.inner)
            .unwrap_or_default();
        let record = Pane {
            agent: crate::agents::PaneAgent::default(),
            cols: inner.width.max(2),
            rows: inner.height.max(1),
            ..record
        };
        match self.start_pane(pane, record, None) {
            Ok(()) => {}
            Err(error) => {
                self.refuse(id, format!("{error:#}"));
                // Nothing to show: take the leaf back out.
                if let Some(entry) = self.tab_mut(tab) {
                    entry.layout.remove(leaf.id);
                }
                self.drop_empty_tab(tab);
                self.broadcast_workspace();
                return;
            }
        }
        self.set_focus(id, tab, leaf.id);
        self.apply_size(tab);
        self.broadcast_workspace();
        self.send_focus_all();
    }

    /// Opens a new tab in a workspace with one pane.
    pub(crate) fn command_tab(
        &mut self,
        workspace: u64,
        name: &str,
        record: Pane,
    ) -> Result<PaneId, String> {
        let tab = self
            .add_tab(workspace, name.to_owned())
            .ok_or_else(|| "could not record a new tab".to_owned())?;
        let pane = self
            .reserve_pane(&record)
            .map_err(|error| format!("{error:#}"))?;
        let leaf = self.new_leaf(pane);
        let Some(entry) = self.tab_mut(tab) else {
            return Err("the new tab is gone".to_owned());
        };
        entry.layout.split(None, leaf, Axis::Row);
        entry.focus = Some(leaf.id);
        let inner = entry
            .layout
            .place(area_cells(entry.area))
            .first()
            .map(|placed| placed.inner)
            .unwrap_or_default();
        let record = Pane {
            agent: crate::agents::PaneAgent::default(),
            cols: inner.width.max(2),
            rows: inner.height.max(1),
            ..record
        };
        if let Err(error) = self.start_pane(pane, record, None) {
            if let Some(entry) = self.tab_mut(tab) {
                entry.layout.remove(leaf.id);
            }
            self.drop_empty_tab(tab);
            return Err(format!("{error:#}"));
        }
        self.apply_size(tab);
        self.broadcast_workspace();
        Ok(pane)
    }

    /// Starts a reserved pane's holder at the record's size, and shows the
    /// pane to every client.
    pub(crate) fn start_pane(
        &mut self,
        pane: PaneId,
        record: Pane,
        after: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let argv = argv(&record.launch).context("a pane needs a command")?;
        let (link, welcome) = holders::start(
            holders::Start {
                binary: &self.holder_binary,
                socket: self.paths.pane_socket(pane),
                pane,
                argv: &argv,
                cwd: &record.cwd,
                env: &record.env,
                cols: record.cols,
                rows: record.rows,
                history: record.history.then(|| self.paths.pane_history(pane)),
                after,
            },
            &self.build,
            &self.events,
        )?;
        let (cols, rows) = holders::screen_size(&welcome.snapshot);
        self.panes.insert(
            pane,
            Pane {
                link: Some(link),
                cols,
                rows,
                screen: Some(welcome.snapshot.clone()),
                ..record
            },
        );
        self.broadcast_frames(FromDaemon::Snapshot {
            pane: pane.raw(),
            seq: 0,
            body: welcome.snapshot,
        });
        self.broadcast_pane(pane);
        Ok(())
    }

    /// Shows a pane to the client: its leaf, or for a pane without one, a
    /// new tab in the pane's workspace.
    pub(crate) fn show_pane(&mut self, client: u64, pane: PaneId) {
        let Some(workspace) = self.panes.get(&pane).map(|entry| entry.workspace) else {
            self.refuse(client, format!("no pane {pane}"));
            return;
        };
        let leaf = match self.leaf_of(pane) {
            Some((_, leaf)) => leaf.id,
            None => {
                // An unnamed tab shows its pane's title.
                let Some(tab) = self.add_tab(workspace, String::new()) else {
                    self.refuse(client, "could not record a new tab".to_owned());
                    return;
                };
                let leaf = self.new_leaf(pane);
                if let Some(entry) = self.tab_mut(tab) {
                    entry.layout.split(None, leaf, Axis::Row);
                    entry.focus = Some(leaf.id);
                }
                self.apply_size(tab);
                leaf.id
            }
        };
        self.show_leaf(client, workspace, leaf);
    }

    /// Removes a leaf, ends its pane, and gives its focus to a neighbour.
    pub(crate) fn close_leaf(&mut self, tab: u64, leaf: LeafId) {
        let Some(entry) = self.tab_mut(tab) else {
            return;
        };
        let Some(removed) = entry.layout.leaf(leaf) else {
            return;
        };
        let next = entry.layout.remove(leaf);
        if entry.focus == Some(leaf) {
            entry.focus = next;
        }
        for client in self.clients.values_mut() {
            if client.focus.get(&tab) == Some(&leaf) {
                match next {
                    Some(next) => client.focus.insert(tab, next),
                    None => client.focus.remove(&tab),
                };
            }
        }
        if let Some(pane) = removed.terminal() {
            self.end_pane(pane);
        }
        self.drop_empty_tab(tab);
        self.apply_size(tab);
        self.broadcast_workspace();
        self.send_focus_all();
    }

    /// Forgets a pane and deletes its history (Invariant 22). A running one
    /// is killed, and its holder is kept until it reports the exit.
    pub(crate) fn end_pane(&mut self, pane: PaneId) {
        // Closing a setup pane skips the rest of the setup.
        self.setup_ended(pane, None);
        let _ = std::fs::remove_dir_all(self.paths.pane_history(pane));
        let entry = self.panes.remove(&pane);
        self.check_gone(pane);
        self.ship_gone(pane);
        if let Some(entry) = entry
            && let Some(link) = entry.link
        {
            if entry.state == PaneState::Running {
                link.send(ToHolder::Kill);
                self.closing.insert(pane, link);
            } else {
                link.send(ToHolder::AckExit);
            }
        }
        self.broadcast(FromDaemon::PaneClosed { pane: pane.raw() });
    }

    pub(crate) fn drop_empty_tab(&mut self, tab: u64) {
        let Some(entry) = self.tab(tab) else {
            return;
        };
        if !entry.layout.is_empty() {
            return;
        }
        let workspace = entry.workspace;
        let siblings = self.workspace_tabs(workspace);
        let index = siblings.iter().position(|id| *id == tab).unwrap_or(0);
        self.tabs.retain(|(id, _)| *id != tab);
        // Clients on the tab move to its neighbour in the workspace.
        let next = siblings
            .get(index + 1)
            .or_else(|| index.checked_sub(1).and_then(|before| siblings.get(before)))
            .copied();
        for client in self.clients.values_mut() {
            client.focus.remove(&tab);
            if client.tab == Some(tab) {
                client.tab = next;
            }
        }
        if let Some(entry) = self.workspaces.get_mut(&workspace)
            && entry.active_tab == Some(tab)
        {
            entry.active_tab = next;
        }
        let orphaned: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, client)| client.interactive && client.tab.is_some())
            .map(|(id, _)| *id)
            .collect();
        for client in orphaned {
            if let Some(tab) = self.client(client).and_then(|client| client.tab)
                && self.tab(tab).is_some_and(|entry| entry.lease.is_none())
            {
                self.take_lease(tab, client);
            }
        }
    }

    pub(crate) fn close_tab(&mut self, tab: u64) {
        let leaves = self
            .tab(tab)
            .map(|entry| entry.layout.leaves())
            .unwrap_or_default();
        for leaf in leaves {
            self.close_leaf(tab, leaf.id);
        }
    }

    /// Starts an ended pane's command in a new holder, in the same leaf and
    /// at the same size.
    pub(crate) fn run_again(&mut self, id: u64, old: PaneId) {
        // A check's pane runs the check again, with new source keys.
        if let Ok(Some((worktree, name))) = self.store.check_of_pane(old) {
            self.run_check(id, worktree, name);
            return;
        }
        let Some(entry) = self.panes.get(&old) else {
            return;
        };
        if argv(&entry.launch).is_none() {
            return;
        }
        let launch = entry.launch.clone();
        self.restart(id, old, launch, crate::agents::PaneAgent::default());
    }

    /// Runs an ended pane's agent kind's resume command, for the agent
    /// session that ran when the pane ended, in the pane's leaf.
    pub(crate) fn resume(&mut self, id: u64, old: PaneId) {
        let Some((kind, reference, argv)) = self.resume_argv(old) else {
            return;
        };
        let agent = crate::agents::PaneAgent::resuming(kind, reference);
        self.restart(id, old, Launch::Command(argv), agent);
    }

    /// Starts `launch` in a new holder that takes an ended pane's place, its
    /// directory, its size and its history.
    fn restart(&mut self, id: u64, old: PaneId, launch: Launch, agent: crate::agents::PaneAgent) {
        let Some(entry) = self.panes.get(&old) else {
            return;
        };
        if entry.state == PaneState::Running {
            return;
        }
        let record = Pane {
            agent,
            link: None,
            cols: entry.cols,
            rows: entry.rows,
            state: PaneState::Running,
            screen: None,
            launch,
            cwd: entry.cwd.clone(),
            title: String::new(),
            history: entry.history,
            workspace: entry.workspace,
            spawn_worktree: entry.spawn_worktree,
            env: entry.env.clone(),
        };
        let Some((tab, leaf)) = self.leaf_of(old) else {
            return;
        };
        let Ok(pane) = self.reserve_pane(&record) else {
            return;
        };
        // The old run's history shows above the new one.
        let after = Some(self.paths.pane_history(old));
        if self.start_pane(pane, record, after).is_err() {
            return;
        }
        if let Some(entry) = self.tab_mut(tab) {
            entry.layout.replace_pane(leaf.id, pane);
        }
        self.setup_moved(old, pane);
        self.end_pane(old);
        self.set_focus(id, tab, leaf.id);
        self.apply_size(tab);
        self.broadcast_workspace();
    }

    /// Focuses a leaf for one client, and shows it: a stack expands it and the
    /// floating layer raises it.
    pub(crate) fn set_focus(&mut self, id: u64, tab: u64, leaf: LeafId) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.focus.insert(tab, leaf);
        }
        let Some(entry) = self.tab_mut(tab) else {
            return;
        };
        entry.focus = Some(leaf);
        let before = entry.layout.clone();
        entry.layout.reveal(leaf);
        if entry.layout != before {
            self.broadcast_workspace();
        }
        self.send_focus(id);
    }

    pub(crate) fn focus_side(&mut self, id: u64, side: Side) {
        let Some((tab, leaf)) = self.client_focus(id) else {
            return;
        };
        let Some(entry) = self.tab(tab) else {
            return;
        };
        if let Some(next) = entry.layout.neighbour(leaf, side, area_cells(entry.area)) {
            self.set_focus(id, tab, next);
        }
    }

    pub(crate) fn focus_leaf(&mut self, id: u64, leaf: LeafId) {
        let Some(tab) = self
            .tabs
            .iter()
            .find(|(_, entry)| entry.layout.contains(leaf))
            .map(|(tab, _)| *tab)
        else {
            return;
        };
        if self.client(id).and_then(|client| client.tab) != Some(tab) {
            self.select_tab(id, tab);
        }
        self.set_focus(id, tab, leaf);
    }

    pub(crate) fn resize_pane(&mut self, id: u64, side: Side) {
        self.edit_focused(id, |layout, leaf| {
            layout.resize(leaf, side, tuning::RESIZE_STEP);
        });
    }

    pub(crate) fn swap_pane(&mut self, id: u64, side: Side) {
        let Some((tab, _)) = self.client_focus(id) else {
            return;
        };
        let Some(area) = self.tab(tab).map(|entry| area_cells(entry.area)) else {
            return;
        };
        self.edit_focused(id, |layout, leaf| layout.swap(leaf, side, area));
    }

    /// Changes the layout around the client's focused leaf, then resizes the
    /// panes and tells every client.
    pub(crate) fn edit_focused(&mut self, id: u64, edit: impl FnOnce(&mut Layout, LeafId)) {
        let Some((tab, leaf)) = self.client_focus(id) else {
            return;
        };
        if let Some(entry) = self.tab_mut(tab) {
            edit(&mut entry.layout, leaf);
        }
        self.apply_size(tab);
        self.broadcast_workspace();
        self.send_focus(id);
    }

    /// Changes the layout of the tab that holds a leaf, then resizes its
    /// panes and tells every client.
    pub(crate) fn edit_leaf(&mut self, leaf: LeafId, edit: impl FnOnce(&mut Layout, Cells)) {
        let Some((tab, entry)) = self
            .tabs
            .iter_mut()
            .find(|(_, entry)| entry.layout.contains(leaf))
        else {
            return;
        };
        let tab = *tab;
        let area = area_cells(entry.area);
        edit(&mut entry.layout, area);
        self.apply_size(tab);
        self.broadcast_workspace();
    }

    /// Shows a tab in one client, in the tab's workspace.
    pub(crate) fn select_tab(&mut self, id: u64, tab: u64) {
        let Some(workspace) = self.tab(tab).map(|entry| entry.workspace) else {
            return;
        };
        if self.client_workspace(id) != workspace {
            self.switch_workspace(id, workspace);
        }
        let interactive = match self.clients.get_mut(&id) {
            Some(client) => {
                client.tab = Some(tab);
                client.interactive
            }
            None => return,
        };
        if let Some(entry) = self.workspaces.get_mut(&workspace) {
            entry.active_tab = Some(tab);
        }
        if interactive && self.tab(tab).is_some_and(|entry| entry.lease.is_none()) {
            self.take_lease(tab, id);
        }
        self.send_focus(id);
    }

    pub(crate) fn cycle_tab(&mut self, id: u64, forward: bool) {
        let tabs = self.workspace_tabs(self.client_workspace(id));
        let count = tabs.len();
        let current = self
            .client(id)
            .and_then(|client| client.tab)
            .and_then(|tab| tabs.iter().position(|entry| *entry == tab));
        let (Some(current), true) = (current, count > 0) else {
            return;
        };
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        self.select_tab(id, tabs[next]);
    }

    pub(crate) fn refuse(&self, id: u64, reason: String) {
        if let Some(client) = self.client(id) {
            self.post(&client.out, FromDaemon::Refused { reason });
        }
    }

    fn send_focus_all(&self) {
        for id in self.clients.keys() {
            self.send_focus(*id);
        }
    }
}

/// The command a pane runs. An adopted pane with no record cannot run again.
pub(crate) fn argv(launch: &Launch) -> Option<Vec<String>> {
    match launch {
        Launch::Shell => Some(vec![
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()),
        ]),
        Launch::Command(argv) if !argv.is_empty() => Some(argv.clone()),
        Launch::Command(_) | Launch::Unknown => None,
    }
}
