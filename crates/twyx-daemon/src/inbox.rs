//! The inbox: one item per pane that needs the user, kept until the user
//! dismisses it. An item is read once its pane comes into view in a focused
//! client; "mark unread" holds until the pane comes into view again.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use twyx_core::config::Notice;
use twyx_core::inbox::{Reason, mark, plain};
use twyx_core::pane::PaneId;
use twyx_core::theme::AgentState as Mark;
use twyx_core::tuning;
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
        let notice = self.config.notifications.for_reason(reason);
        if notice == Notice::Off {
            return;
        }
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
        // Invariant 23: a pane in view in a focused client is seen, and
        // never notifies.
        if self.in_view().contains(&pane) {
            self.set_read(id, true);
        } else if matches!(notice, Notice::Notify | Notice::Sound) {
            self.send_notice(pane, reason, notice == Notice::Sound);
        }
        self.inbox_changed = true;
    }

    /// Sends a notification to one client: the focused one with the latest
    /// input, else any with the latest input. Too soon after the last one,
    /// for the pane or at all, the item stays in the inbox only.
    fn send_notice(&mut self, pane: PaneId, reason: Reason, sound: bool) {
        let now = Instant::now();
        if self
            .notified
            .get(&pane)
            .is_some_and(|last| now.duration_since(*last) < tuning::NOTIFY_PANE_GAP)
            || self
                .last_notice
                .is_some_and(|last| now.duration_since(last) < tuning::NOTIFY_GAP)
        {
            return;
        }
        let Some(client) = self
            .clients
            .values()
            .filter(|client| client.interactive)
            .max_by_key(|client| (client.focused, client.used))
        else {
            return;
        };
        let Some(entry) = self.panes.get(&pane) else {
            return;
        };
        let agent = entry
            .agent
            .session
            .as_ref()
            .or(entry.agent.ended.as_ref())
            .map(|session| session.kind.clone());
        let what = match reason {
            Reason::NeedsApproval => "needs approval",
            Reason::TurnFinished => "is done",
            Reason::Failed => "failed",
            Reason::CheckEnded => "",
        };
        let place = self
            .workspaces
            .get(&entry.workspace)
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default();
        let who = agent.unwrap_or_else(|| format!("pane {pane}"));
        let body = match reason {
            // A check's pane title carries its name and result.
            Reason::CheckEnded => format!("{} · {place}", entry.title),
            _ => format!("{who} {what} · {place}"),
        };
        let body = plain(&body, tuning::NOTIFY_TEXT_MAX);
        self.post(
            &client.out,
            FromDaemon::Notify {
                title: "twyx".to_owned(),
                body,
                sound,
            },
        );
        self.notified.insert(pane, now);
        self.last_notice = Some(now);
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
