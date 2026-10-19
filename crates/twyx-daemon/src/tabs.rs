//! Tab and layout commands: new, close and run-again panes, focus, resize,
//! swap, zoom, float, and the tabs themselves.

use twyx_core::layout::{Axis, Layout, LeafId, Side};
use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::client::{FromDaemon, Launch, PaneState, Place};
use twyx_protocol::holder::ToHolder;

use crate::daemon::{Daemon, Pane, area_cells};
use crate::holders;

impl Daemon {
    pub(crate) fn new_pane(&mut self, id: u64, launch: Launch, cwd: String, place: Place) {
        let Some(argv) = argv(&launch) else {
            self.refuse(id, "a pane needs a command".to_owned());
            return;
        };
        let interactive = self.client(id).is_some_and(|client| client.interactive);
        let existing = self
            .client(id)
            .and_then(|client| client.tab)
            .or(self.active_tab)
            .filter(|tab| self.tab(*tab).is_some());
        let (tab, created) = match (place, existing) {
            (Place::NewTab, _) | (_, None) => match self.add_tab(String::new()) {
                Some(tab) => (tab, true),
                None => {
                    self.refuse(id, "could not record a new tab".to_owned());
                    return;
                }
            },
            (_, Some(tab)) => (tab, false),
        };
        if created {
            self.active_tab = Some(tab);
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
        let pane = match self.reserve_pane(&launch, &cwd, 80, 24) {
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
        let (cols, rows) = (inner.width.max(2), inner.height.max(1));

        let started = holders::start(
            holders::Start {
                binary: &self.holder_binary,
                socket: self.paths.pane_socket(pane),
                pane,
                argv: &argv,
                cwd: &cwd,
                cols,
                rows,
            },
            &self.build,
            &self.events,
        );
        let (link, welcome) = match started {
            Ok(started) => started,
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
        };
        let (cols, rows) = holders::screen_size(&welcome.snapshot);
        self.panes.insert(
            pane,
            Pane {
                link: Some(link),
                cols,
                rows,
                state: PaneState::Running,
                screen: Some(welcome.snapshot.clone()),
                launch,
                cwd,
                title: String::new(),
            },
        );
        self.broadcast_frames(FromDaemon::Snapshot {
            pane: pane.raw(),
            seq: 0,
            body: welcome.snapshot,
        });
        self.broadcast_pane(pane);
        self.set_focus(id, tab, leaf.id);
        self.apply_size(tab);
        self.broadcast_workspace();
        self.send_focus_all();
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
        self.end_pane(removed.pane);
        self.drop_empty_tab(tab);
        self.apply_size(tab);
        self.broadcast_workspace();
        self.send_focus_all();
    }

    /// Forgets a pane. A running one is killed, and its holder is kept until
    /// it reports the exit.
    fn end_pane(&mut self, pane: PaneId) {
        if let Some(entry) = self.panes.remove(&pane)
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

    fn drop_empty_tab(&mut self, tab: u64) {
        let Some(index) = self.tabs.iter().position(|(id, _)| *id == tab) else {
            return;
        };
        if !self.tabs[index].1.layout.is_empty() {
            return;
        }
        self.tabs.remove(index);
        // Clients on the tab move to its neighbour.
        let next = self
            .tabs
            .get(index)
            .or_else(|| {
                index
                    .checked_sub(1)
                    .and_then(|before| self.tabs.get(before))
            })
            .map(|(id, _)| *id);
        for client in self.clients.values_mut() {
            client.focus.remove(&tab);
            if client.tab == Some(tab) {
                client.tab = next;
            }
        }
        if self.active_tab == Some(tab) {
            self.active_tab = next;
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
        let Some(entry) = self.panes.get(&old) else {
            return;
        };
        if entry.state == PaneState::Running {
            return;
        }
        let Some(argv) = argv(&entry.launch) else {
            return;
        };
        let (launch, cwd, cols, rows) = (
            entry.launch.clone(),
            entry.cwd.clone(),
            entry.cols,
            entry.rows,
        );
        let Some((tab, leaf)) = self.leaf_of(old) else {
            return;
        };
        let Ok(pane) = self.reserve_pane(&launch, &cwd, cols, rows) else {
            return;
        };
        let Ok((link, welcome)) = holders::start(
            holders::Start {
                binary: &self.holder_binary,
                socket: self.paths.pane_socket(pane),
                pane,
                argv: &argv,
                cwd: &cwd,
                cols,
                rows,
            },
            &self.build,
            &self.events,
        ) else {
            return;
        };
        if let Some(entry) = self.tab_mut(tab) {
            entry.layout.replace_pane(leaf.id, pane);
        }
        self.end_pane(old);
        self.panes.insert(
            pane,
            Pane {
                link: Some(link),
                cols,
                rows,
                state: PaneState::Running,
                screen: Some(welcome.snapshot.clone()),
                launch,
                cwd,
                title: String::new(),
            },
        );
        self.broadcast_frames(FromDaemon::Snapshot {
            pane: pane.raw(),
            seq: 0,
            body: welcome.snapshot,
        });
        self.broadcast_pane(pane);
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

    pub(crate) fn select_tab(&mut self, id: u64, tab: u64) {
        if self.tab(tab).is_none() {
            return;
        }
        let interactive = match self.clients.get_mut(&id) {
            Some(client) => {
                client.tab = Some(tab);
                client.interactive
            }
            None => return,
        };
        self.active_tab = Some(tab);
        if interactive && self.tab(tab).is_some_and(|entry| entry.lease.is_none()) {
            self.take_lease(tab, id);
        }
        self.send_focus(id);
    }

    fn refuse(&self, id: u64, reason: String) {
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
fn argv(launch: &Launch) -> Option<Vec<String>> {
    match launch {
        Launch::Shell => Some(vec![
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()),
        ]),
        Launch::Command(argv) if !argv.is_empty() => Some(argv.clone()),
        Launch::Command(_) | Launch::Unknown => None,
    }
}
