//! Workspaces: the tabs for one worktree or plain directory, which client
//! shows which, and switching between them. No process stops on a switch.

use twyx_protocol::client::{FromDaemon, PaneRef, TabInfo, WorkspaceInfo, WorkspaceSummary};

use crate::daemon::Daemon;

#[derive(Debug, Clone)]
pub(crate) struct Workspace {
    pub directory: String,
    pub name: String,
    pub project: Option<u64>,
    /// `None` for a plain directory, for the default workspace, and after
    /// its worktree is gone.
    pub worktree: Option<u64>,
    /// The tab a client shows when it comes to this workspace.
    pub active_tab: Option<u64>,
}

impl Daemon {
    /// The workspace a client shows; the default one for a client that is
    /// gone.
    pub(crate) fn client_workspace(&self, id: u64) -> u64 {
        self.client(id)
            .map_or(self.default_workspace, |client| client.workspace)
    }

    /// The tabs of one workspace, in order.
    pub(crate) fn workspace_tabs(&self, workspace: u64) -> Vec<u64> {
        self.tabs
            .iter()
            .filter(|(_, tab)| tab.workspace == workspace)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The tab a client shows when it comes to a workspace: the last one
    /// shown there, else the first.
    pub(crate) fn landing_tab(&self, workspace: u64) -> Option<u64> {
        self.workspaces
            .get(&workspace)
            .and_then(|entry| entry.active_tab)
            .filter(|tab| self.tab(*tab).is_some_and(|tab| tab.workspace == workspace))
            .or_else(|| self.workspace_tabs(workspace).first().copied())
    }

    /// Shows a workspace in one client, and makes it the one a new client
    /// shows. An empty workspace shows nothing until the client asks for a
    /// pane.
    pub(crate) fn switch_workspace(&mut self, id: u64, workspace: u64) {
        if !self.workspaces.contains_key(&workspace) {
            return;
        }
        let tab = self.landing_tab(workspace);
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        client.workspace = workspace;
        client.tab = tab;
        let interactive = client.interactive;
        if interactive {
            self.active_workspace = workspace;
        }
        if interactive
            && let Some(tab) = tab
            && self.tab(tab).is_some_and(|entry| entry.lease.is_none())
        {
            self.take_lease(tab, id);
        }
        if let Some(client) = self.clients.get(&id) {
            self.post(
                &client.out,
                FromDaemon::Workspace(self.workspace_info(workspace)),
            );
        }
        self.send_focus(id);
        if let Some(worktree) = self
            .workspaces
            .get(&workspace)
            .and_then(|entry| entry.worktree)
        {
            self.request_status(worktree);
        }
    }

    /// The interactive client that last sent input, else the one that
    /// attached last.
    pub(crate) fn last_used_client(&self) -> Option<u64> {
        self.clients
            .iter()
            .filter(|(_, client)| client.interactive)
            .max_by_key(|(_, client)| client.used)
            .map(|(id, _)| *id)
    }

    /// Switches the client that asked when it is interactive; for a command,
    /// the last-used client when `switch` is set.
    /// Shows a leaf in a workspace to the client, or for a command client
    /// to the interactive client used last.
    pub(crate) fn show_leaf(&mut self, id: u64, workspace: u64, leaf: twyx_core::layout::LeafId) {
        let target = if self.client(id).is_some_and(|client| client.interactive) {
            id
        } else {
            self.last_used_client().unwrap_or(id)
        };
        if self.client_workspace(target) != workspace {
            self.switch_workspace(target, workspace);
        }
        self.focus_leaf(target, leaf);
        self.broadcast_workspace();
    }

    pub(crate) fn switch_for(&mut self, id: u64, workspace: u64, switch: bool) -> Option<u64> {
        let target = if self.client(id).is_some_and(|client| client.interactive) {
            Some(id)
        } else if switch {
            self.last_used_client()
        } else {
            None
        };
        if let Some(target) = target {
            self.switch_workspace(target, workspace);
        }
        target
    }

    /// Moves every client off a workspace, to the default one.
    pub(crate) fn evacuate(&mut self, workspace: u64) {
        let on: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, client)| client.workspace == workspace)
            .map(|(id, _)| *id)
            .collect();
        for id in on {
            self.switch_workspace(id, self.default_workspace);
        }
        if self.active_workspace == workspace {
            self.active_workspace = self.default_workspace;
        }
    }

    pub(crate) fn workspace_info(&self, workspace: u64) -> WorkspaceInfo {
        let entry = self.workspaces.get(&workspace);
        WorkspaceInfo {
            workspace,
            name: entry.map(|entry| entry.name.clone()).unwrap_or_default(),
            directory: entry
                .map(|entry| entry.directory.clone())
                .unwrap_or_default(),
            worktree: entry
                .and_then(|entry| entry.worktree)
                .and_then(|worktree| self.worktree_info(worktree)),
            tabs: self
                .tabs
                .iter()
                .filter(|(_, tab)| tab.workspace == workspace)
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

    /// Tells each client about the workspace it shows.
    pub(crate) fn broadcast_workspace(&self) {
        for client in self.clients.values() {
            self.post(
                &client.out,
                FromDaemon::Workspace(self.workspace_info(client.workspace)),
            );
        }
    }

    /// Tells the clients that show a worktree's workspace about it.
    pub(crate) fn broadcast_worktree(&self, worktree: u64) {
        for client in self.clients.values() {
            if self
                .workspaces
                .get(&client.workspace)
                .is_some_and(|entry| entry.worktree == Some(worktree))
            {
                self.post(
                    &client.out,
                    FromDaemon::Workspace(self.workspace_info(client.workspace)),
                );
            }
        }
    }

    pub(crate) fn workspace_summaries(&self) -> Vec<WorkspaceSummary> {
        self.workspaces
            .iter()
            .map(|(id, entry)| {
                let tabs = self.workspace_tabs(*id);
                WorkspaceSummary {
                    workspace: *id,
                    name: entry.name.clone(),
                    directory: entry.directory.clone(),
                    project: entry.project,
                    worktree: entry.worktree,
                    tabs: tabs.len() as u32,
                    panes: tabs
                        .iter()
                        .filter_map(|tab| Some((*tab, self.tab(*tab)?)))
                        .flat_map(|(tab, entry)| {
                            entry.layout.leaves().into_iter().filter_map(move |leaf| {
                                let pane = leaf.terminal()?;
                                Some(PaneRef {
                                    pane: pane.raw(),
                                    tab,
                                    leaf: leaf.id,
                                    mark: self.mark(pane),
                                    agent: self
                                        .panes
                                        .get(&pane)
                                        .and_then(|entry| entry.agent.session.as_ref())
                                        .map(|session| session.kind.clone()),
                                })
                            })
                        })
                        .collect(),
                    clients: self
                        .clients
                        .values()
                        .filter(|client| client.interactive && client.workspace == *id)
                        .count() as u32,
                }
            })
            .collect()
    }

    /// The workspace of a directory, made when there is none.
    pub(crate) fn ensure_workspace(
        &mut self,
        directory: &str,
        name: &str,
        project: Option<u64>,
        worktree: Option<u64>,
    ) -> anyhow::Result<u64> {
        let id = self.store.workspace(directory, name, project, worktree)?;
        let entry = self.workspaces.entry(id).or_insert_with(|| Workspace {
            directory: directory.to_owned(),
            name: name.to_owned(),
            project,
            worktree,
            active_tab: None,
        });
        entry.name = name.to_owned();
        entry.project = project;
        entry.worktree = worktree;
        Ok(id)
    }
}
