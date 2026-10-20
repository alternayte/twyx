//! The inbox: one item per pane that needs the user, kept until the user
//! dismisses it. An item is read once its pane comes into view in a focused
//! client; "mark unread" holds until the pane comes into view again.

use std::collections::{BTreeMap, BTreeSet};

use twyx_core::inbox::{Reason, mark};
use twyx_core::pane::PaneId;
use twyx_core::theme::AgentState as Mark;
use twyx_protocol::client::{FromDaemon, InboxItem, PaneState};

use crate::agents::now_ms;
use crate::daemon::Daemon;
use crate::store::SavedItem;

#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub pane: PaneId,
    pub reason: Reason,
    pub created_ms: u64,
    pub read: bool,
}

impl From<SavedItem> for Item {
    fn from(saved: SavedItem) -> Self {
        Self {
            pane: saved.pane,
            reason: saved.reason,
            created_ms: saved.created_ms,
            read: saved.read,
        }
    }
}

impl Daemon {
    /// A pane needs the user: its item comes back unread with this reason,
    /// or a new one appears. A pane in view in a focused client is seen.
    pub(crate) fn raise(&mut self, pane: PaneId, reason: Reason) {
        let existing = self
            .inbox
            .iter()
            .find(|(_, item)| item.pane == pane)
            .map(|(id, _)| *id);
        let session = self
            .panes
            .get(&pane)
            .and_then(|entry| entry.agent.session.as_ref())
            .and_then(|session| session.to_saved())
            .map(|saved| saved.row);
        let created_ms = now_ms();
        let id = match self
            .store
            .raise_item(existing, pane, session, reason, created_ms)
        {
            Ok(id) => id,
            Err(error) => {
                eprintln!("twyx daemon: {error:#}");
                return;
            }
        };
        self.inbox.insert(
            id,
            Item {
                pane,
                reason,
                created_ms,
                read: false,
            },
        );
        if self.in_view().contains(&pane) {
            self.set_read(id, true);
        }
        self.inbox_changed = true;
    }

    pub(crate) fn set_read(&mut self, id: u64, read: bool) {
        let Some(item) = self.inbox.get_mut(&id) else {
            return;
        };
        if item.read == read {
            return;
        }
        item.read = read;
        if let Err(error) = self.store.read_item(id, read) {
            eprintln!("twyx daemon: {error:#}");
        }
        self.inbox_changed = true;
    }

    pub(crate) fn dismiss(&mut self, id: u64) {
        if self.inbox.remove(&id).is_none() {
            return;
        }
        if let Err(error) = self.store.dismiss_item(id) {
            eprintln!("twyx daemon: {error:#}");
        }
        self.inbox_changed = true;
    }

    /// The panes in view in each focused interactive client.
    fn views(&self) -> BTreeMap<u64, BTreeSet<PaneId>> {
        self.clients
            .iter()
            .filter(|(_, client)| client.interactive && client.focused)
            .map(|(id, client)| {
                let shown = client
                    .tab
                    .and_then(|tab| self.tab(tab))
                    .map(|tab| tab.layout.shown())
                    .unwrap_or_default();
                (*id, shown.into_iter().collect())
            })
            .collect()
    }

    pub(crate) fn in_view(&self) -> BTreeSet<PaneId> {
        self.views().into_values().flatten().collect()
    }

    /// Reads the items of panes that came into view since the last call,
    /// and sends the inbox to watching clients when it changed.
    pub(crate) fn settle_inbox(&mut self) {
        let views = self.views();
        let mut arrived = Vec::new();
        for (client, shown) in &views {
            let before = self.clients.get(client).map(|client| &client.seen);
            arrived.extend(
                shown
                    .iter()
                    .filter(|pane| before.is_none_or(|seen| !seen.contains(pane))),
            );
        }
        for client in self.clients.values_mut() {
            client.seen = BTreeSet::new();
        }
        for (client, shown) in views {
            if let Some(entry) = self.clients.get_mut(&client) {
                entry.seen = shown;
            }
        }
        let unread: Vec<u64> = self
            .inbox
            .iter()
            .filter(|(_, item)| !item.read && arrived.contains(&item.pane))
            .map(|(id, _)| *id)
            .collect();
        for id in unread {
            self.set_read(id, true);
        }
        if std::mem::take(&mut self.inbox_changed) {
            let items = self.inbox_items();
            for client in self.clients.values().filter(|client| client.watching) {
                self.post(&client.out, FromDaemon::Inbox(items.clone()));
            }
        }
    }

    /// A pane's status mark.
    pub(crate) fn mark(&self, pane: PaneId) -> Option<Mark> {
        let entry = self.panes.get(&pane)?;
        let failed = match &entry.state {
            PaneState::Exited { code } => *code != 0,
            PaneState::Killed { .. } => true,
            PaneState::Running | PaneState::Lost => false,
        };
        let unread = self
            .inbox
            .values()
            .any(|item| item.pane == pane && !item.read);
        let agent = entry
            .agent
            .session
            .as_ref()
            .map(|session| session.info().state);
        mark(failed, agent, unread)
    }

    pub(crate) fn inbox_items(&self) -> Vec<InboxItem> {
        let mut items: Vec<InboxItem> = self
            .inbox
            .iter()
            .filter_map(|(id, item)| {
                let entry = self.panes.get(&item.pane)?;
                let workspace = self.workspaces.get(&entry.workspace);
                let worktree = entry
                    .spawn_worktree
                    .and_then(|worktree| self.worktrees.get(&worktree));
                let project = worktree
                    .map(|worktree| worktree.project)
                    .or_else(|| workspace.and_then(|workspace| workspace.project))
                    .and_then(|project| self.projects.get(&project));
                Some(InboxItem {
                    item: *id,
                    pane: item.pane.raw(),
                    reason: item.reason,
                    agent: entry.agent.session.as_ref().map(|session| {
                        let info = session.info();
                        (info.state, info.source)
                    }),
                    mark: self.mark(item.pane),
                    created_ms: item.created_ms,
                    read: item.read,
                    workspace: entry.workspace,
                    project: project.map(|project| project.name.clone()),
                    worktree: worktree.map(|worktree| worktree.admin.clone()),
                    title: if entry.title.is_empty() {
                        entry.cwd.clone()
                    } else {
                        entry.title.clone()
                    },
                })
            })
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.created_ms));
        items
    }
}
