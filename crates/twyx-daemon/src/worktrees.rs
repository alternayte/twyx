//! Projects and worktrees: open, list, create and remove, and each
//! worktree's branch and dirty state.
//!
//! Git runs on its own threads, never on the one that forwards frames. Each
//! job reports back as an event.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::client::{
    CommitInfo, FromDaemon, GitState, Overview, PaneState, ProjectInfo, RemovalPlan, WorktreeInfo,
};

use crate::daemon::{Daemon, Event};
use crate::git::{self, Found, WorktreeEntry};

#[derive(Debug, Clone)]
pub(crate) struct Project {
    pub git: bool,
    /// The common Git dir, or the directory.
    pub path: String,
    pub name: String,
    pub base_branch: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Worktree {
    pub project: u64,
    pub path: String,
    pub admin: String,
    pub branch: Option<String>,
    pub git: Option<GitState>,
    pub error: Option<String>,
    pub refresh: Refresh,
}

/// One status run at a time per worktree; a request during a run asks for
/// one more after it.
#[derive(Debug, Default)]
pub(crate) struct Refresh {
    running: bool,
    again: bool,
    last: Option<Instant>,
    /// The modification times of HEAD and the index when the last read
    /// started.
    seen: Marks,
}

/// A removal the user confirmed, waiting for its panes to end.
#[derive(Debug)]
pub(crate) struct Removal {
    client: u64,
    worktree: u64,
    force: bool,
    panes: Vec<PaneId>,
    since: Instant,
    started: bool,
}

/// A plan the daemon offered, to be confirmed as it was.
#[derive(Debug)]
pub(crate) struct Plan {
    worktree: u64,
    force: bool,
    panes: Vec<PaneId>,
}

/// An overview a command waits for: the projects to list and the
/// worktrees to read first.
#[derive(Debug)]
pub(crate) struct Waiting {
    pub client: u64,
    projects: BTreeSet<u64>,
    worktrees: BTreeSet<u64>,
}

/// What a Git job found.
#[derive(Debug)]
pub(crate) enum Done {
    Opened {
        client: u64,
        switch: bool,
        found: Result<(Found, Vec<WorktreeEntry>), String>,
    },
    Listed {
        project: u64,
        listed: Result<Vec<WorktreeEntry>, String>,
    },
    Status {
        worktree: u64,
        status: Result<git::Status, String>,
    },
    Added {
        client: u64,
        project: u64,
        path: PathBuf,
        listed: Result<Vec<WorktreeEntry>, String>,
    },
    Planned {
        client: u64,
        worktree: u64,
        findings: Result<Findings, String>,
    },
    Removed {
        client: u64,
        worktree: u64,
        result: Result<(), String>,
    },
}

#[derive(Debug)]
pub(crate) struct Findings {
    dirty: Vec<(String, String)>,
    unpushed: Vec<CommitInfo>,
    unpushed_total: u32,
    unmerged: Vec<CommitInfo>,
    base: Option<String>,
}

fn message(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

impl Daemon {
    fn job(&self, work: impl FnOnce() -> Done + Send + 'static) {
        let events = self.events.clone();
        std::thread::spawn(move || {
            let _ = events.send(Event::Git(work()));
        });
    }

    pub(crate) fn worktree_info(&self, id: u64) -> Option<WorktreeInfo> {
        let worktree = self.worktrees.get(&id)?;
        Some(WorktreeInfo {
            worktree: id,
            project: worktree.project,
            name: worktree.admin.clone(),
            path: worktree.path.clone(),
            branch: worktree.branch.clone(),
            git: worktree.git.clone(),
            error: worktree.error.clone(),
            workspace: self
                .workspaces
                .iter()
                .find(|(_, workspace)| workspace.worktree == Some(id))
                .map(|(id, _)| *id),
        })
    }

    /// The main worktree's directory of a Git project.
    fn main_path(&self, project: u64) -> Option<PathBuf> {
        let project = self.projects.get(&project)?;
        let common = Path::new(&project.path);
        self.worktrees
            .values()
            .filter(|worktree| {
                Path::new(&worktree.path)
                    .join(".git")
                    .canonicalize()
                    .ok()
                    .as_deref()
                    == Some(common)
            })
            .map(|worktree| PathBuf::from(&worktree.path))
            .next()
            .or_else(|| common.parent().map(Path::to_path_buf))
    }

    /// The known worktree a directory is in: the deepest one that holds it.
    pub(crate) fn worktree_at(&self, directory: &str) -> Option<u64> {
        let directory = Path::new(directory)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(directory));
        self.worktrees
            .iter()
            .filter(|(_, worktree)| directory.starts_with(&worktree.path))
            .max_by_key(|(_, worktree)| worktree.path.len())
            .map(|(id, _)| *id)
    }

    pub(crate) fn open(&mut self, client: u64, path: String, switch: bool) {
        self.job(move || {
            let found = git::discover(Path::new(&path)).and_then(|found| {
                let listed = match &found {
                    Found::Repository { toplevel, .. } => git::worktrees(toplevel)?,
                    Found::Directory(_) => Vec::new(),
                };
                Ok((found, listed))
            });
            Done::Opened {
                client,
                switch,
                found: found.map_err(|error| message(&error)),
            }
        });
    }

    pub(crate) fn git_done(&mut self, done: Done) {
        match done {
            Done::Opened {
                client,
                switch,
                found,
            } => match found.and_then(|(found, listed)| {
                self.opened(found, &listed).map_err(|error| message(&error))
            }) {
                Ok(workspace) => {
                    let shown = self.switch_for(client, workspace, switch);
                    self.reply(
                        client,
                        FromDaemon::Opened {
                            workspace,
                            client: shown,
                        },
                    );
                }
                Err(reason) => self.refuse(client, reason),
            },
            Done::Listed { project, listed } => {
                match listed {
                    Ok(listed) => {
                        if let Err(error) = self.sync(project, &listed) {
                            eprintln!("twyx daemon: {error:#}");
                        }
                    }
                    Err(reason) => {
                        for worktree in self.worktrees.values_mut() {
                            if worktree.project == project {
                                worktree.error = Some(reason.clone());
                            }
                        }
                    }
                }
                let worktrees: Vec<u64> = self
                    .worktrees
                    .iter()
                    .filter(|(_, worktree)| worktree.project == project)
                    .map(|(id, _)| *id)
                    .collect();
                for waiting in &mut self.waiting {
                    if waiting.projects.remove(&project) {
                        waiting.worktrees.extend(&worktrees);
                    }
                }
                for worktree in worktrees {
                    self.request_status(worktree);
                }
                self.answer_overviews();
            }
            Done::Status { worktree, status } => self.status_done(worktree, status),
            Done::Added {
                client,
                project,
                path,
                listed,
            } => {
                let added = listed
                    .and_then(|listed| self.sync(project, &listed).map_err(|e| message(&e)))
                    .and_then(|()| {
                        let path = path.canonicalize().unwrap_or(path);
                        let id = self
                            .worktrees
                            .iter()
                            .find(|(_, worktree)| Path::new(&worktree.path) == path)
                            .map(|(id, _)| *id)
                            .ok_or_else(|| {
                                format!("Git did not list the new worktree {}", path.display())
                            })?;
                        let workspace = self
                            .worktree_workspace(id)
                            .map_err(|error| message(&error))?;
                        Ok((id, workspace))
                    });
                match added {
                    Ok((id, workspace)) => {
                        self.request_status(id);
                        if let Some(worktree) = self.worktree_info(id) {
                            self.reply(
                                client,
                                FromDaemon::WorktreeAdded {
                                    worktree,
                                    workspace,
                                },
                            );
                        }
                    }
                    Err(reason) => self.refuse(client, reason),
                }
            }
            Done::Planned {
                client,
                worktree,
                findings,
            } => self.planned(client, worktree, findings),
            Done::Removed {
                client,
                worktree,
                result,
            } => {
                self.removals.retain(|removal| removal.worktree != worktree);
                match result {
                    Ok(()) => {
                        self.forget_worktree(worktree);
                        self.reply(client, FromDaemon::WorktreeRemoved { worktree });
                    }
                    Err(reason) => self.refuse(client, reason),
                }
            }
        }
    }

    /// Records what an open found and returns the workspace to show.
    fn opened(&mut self, found: Found, listed: &[WorktreeEntry]) -> anyhow::Result<u64> {
        match found {
            Found::Directory(directory) => {
                let directory = directory.to_string_lossy().into_owned();
                if self
                    .workspaces
                    .get(&self.default_workspace)
                    .is_some_and(|workspace| workspace.directory == directory)
                {
                    return Ok(self.default_workspace);
                }
                let name = base_name(&directory);
                let project = self.store.project(false, &directory, &name)?;
                self.projects.insert(
                    project,
                    Project {
                        git: false,
                        path: directory.clone(),
                        name: name.clone(),
                        base_branch: None,
                    },
                );
                self.ensure_workspace(&directory, &name, Some(project), None)
            }
            Found::Repository {
                common_dir,
                toplevel,
            } => {
                let path = common_dir.to_string_lossy().into_owned();
                let name = listed
                    .iter()
                    .find(|entry| entry.main)
                    .map(|entry| base_name(&entry.path.to_string_lossy()))
                    .unwrap_or_else(|| base_name(&path));
                let project = self.store.project(true, &path, &name)?;
                let base_branch = self
                    .projects
                    .get(&project)
                    .and_then(|project| project.base_branch.clone());
                self.projects.insert(
                    project,
                    Project {
                        git: true,
                        path,
                        name,
                        base_branch,
                    },
                );
                self.sync(project, listed)?;
                let worktree = self
                    .worktrees
                    .iter()
                    .find(|(_, worktree)| Path::new(&worktree.path) == toplevel)
                    .map(|(id, _)| *id)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Git did not list the worktree at {}", toplevel.display())
                    })?;
                self.worktree_workspace(worktree)
            }
        }
    }

    /// The workspace of a worktree, made when it has none.
    fn worktree_workspace(&mut self, worktree: u64) -> anyhow::Result<u64> {
        let Some(entry) = self.worktrees.get(&worktree) else {
            anyhow::bail!("no worktree {worktree}");
        };
        let project = entry.project;
        let name = format!(
            "{}/{}",
            self.projects
                .get(&project)
                .map(|project| project.name.as_str())
                .unwrap_or_default(),
            entry.admin
        );
        let directory = entry.path.clone();
        self.ensure_workspace(&directory, &name, Some(project), Some(worktree))
    }

    /// Makes the worktrees of a project match what Git listed. A worktree
    /// Git no longer lists is marked removed; its workspace stays.
    fn sync(&mut self, project: u64, listed: &[WorktreeEntry]) -> anyhow::Result<()> {
        let mut live = BTreeSet::new();
        for entry in listed.iter().filter(|entry| !entry.prunable) {
            let path = entry.path.to_string_lossy().into_owned();
            let id = self
                .store
                .worktree(project, &path, &entry.admin, entry.branch.as_deref())?;
            live.insert(id);
            let worktree = self.worktrees.entry(id).or_insert_with(|| Worktree {
                project,
                path: path.clone(),
                admin: entry.admin.clone(),
                branch: entry.branch.clone(),
                git: None,
                error: None,
                refresh: Refresh::default(),
            });
            worktree.project = project;
            worktree.admin = entry.admin.clone();
            worktree.branch = entry.branch.clone();
            let workspace = self
                .workspaces
                .iter()
                .find(|(_, workspace)| workspace.directory == path)
                .map(|(id, _)| *id);
            // A worktree made again at a known directory gets its workspace back.
            if let Some(workspace) = workspace
                && self.workspaces.get(&workspace).and_then(|w| w.worktree) != Some(id)
            {
                self.worktree_workspace(id)?;
            }
        }
        let gone: Vec<u64> = self
            .worktrees
            .iter()
            .filter(|(id, worktree)| worktree.project == project && !live.contains(id))
            .map(|(id, _)| *id)
            .collect();
        for worktree in gone {
            self.store.remove_worktree(worktree)?;
            self.worktrees.remove(&worktree);
            for workspace in self.workspaces.values_mut() {
                if workspace.worktree == Some(worktree) {
                    workspace.worktree = None;
                }
            }
            self.broadcast_workspace();
        }
        Ok(())
    }

    pub(crate) fn overview(&mut self, client: u64, refresh: bool) {
        if !refresh {
            self.reply(client, FromDaemon::Overview(self.overview_now()));
            return;
        }
        let projects: Vec<(u64, PathBuf)> = self
            .projects
            .keys()
            .filter_map(|id| Some((*id, self.main_path(*id)?)))
            .filter(|(id, _)| self.projects.get(id).is_some_and(|project| project.git))
            .collect();
        self.waiting.push(Waiting {
            client,
            projects: projects.iter().map(|(id, _)| *id).collect(),
            worktrees: BTreeSet::new(),
        });
        for (project, main) in projects {
            self.job(move || Done::Listed {
                project,
                listed: git::worktrees(&main).map_err(|error| message(&error)),
            });
        }
        self.answer_overviews();
    }

    fn overview_now(&self) -> Overview {
        Overview {
            projects: self
                .projects
                .iter()
                .map(|(id, project)| ProjectInfo {
                    project: *id,
                    name: project.name.clone(),
                    path: project.path.clone(),
                    git: project.git,
                    worktrees: self
                        .worktrees
                        .iter()
                        .filter(|(_, worktree)| worktree.project == *id)
                        .filter_map(|(worktree, _)| self.worktree_info(*worktree))
                        .collect(),
                })
                .collect(),
            workspaces: self.workspace_summaries(),
        }
    }

    fn answer_overviews(&mut self) {
        let (ready, waiting): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|waiting| waiting.projects.is_empty() && waiting.worktrees.is_empty());
        self.waiting = waiting;
        for waiting in ready {
            self.reply(waiting.client, FromDaemon::Overview(self.overview_now()));
        }
    }

    /// Reads a worktree's status, or asks for one more read after the
    /// running one.
    pub(crate) fn request_status(&mut self, id: u64) {
        let Some(worktree) = self.worktrees.get_mut(&id) else {
            return;
        };
        if worktree.refresh.running {
            worktree.refresh.again = true;
            return;
        }
        worktree.refresh.running = true;
        worktree.refresh.last = Some(Instant::now());
        let path = PathBuf::from(&worktree.path);
        worktree.refresh.seen = marks(&path);
        self.job(move || Done::Status {
            worktree: id,
            status: git::status(&path).map_err(|error| message(&error)),
        });
    }

    fn status_done(&mut self, id: u64, status: Result<git::Status, String>) {
        let Some(worktree) = self.worktrees.get_mut(&id) else {
            return;
        };
        worktree.refresh.running = false;
        let before = (
            worktree.git.clone(),
            worktree.error.clone(),
            worktree.branch.clone(),
        );
        match status {
            Ok(status) => {
                worktree.git = Some(GitState {
                    branch: status.branch.clone(),
                    upstream: status.upstream.clone(),
                    ahead: status.ahead,
                    behind: status.behind,
                    changed: status.changes.len() as u32,
                });
                worktree.error = None;
                worktree.branch = status.branch;
            }
            Err(reason) => worktree.error = Some(reason),
        }
        let again = std::mem::take(&mut worktree.refresh.again);
        let branch = worktree.branch.clone();
        let changed = before != (worktree.git.clone(), worktree.error.clone(), branch.clone());
        if before.2 != branch
            && let Err(error) = self.store.set_branch(id, branch.as_deref())
        {
            eprintln!("twyx daemon: {error:#}");
        }
        if changed {
            self.broadcast_worktree(id);
        }
        if again {
            self.request_status(id);
            return;
        }
        for waiting in &mut self.waiting {
            waiting.worktrees.remove(&id);
        }
        self.answer_overviews();
    }

    /// Looks at the worktrees clients show: a changed HEAD or index, or a
    /// status older than the interval, reads the status again. Also ends
    /// removals whose panes would not end.
    pub(crate) fn tick(&mut self) {
        let shown: BTreeSet<u64> = self
            .clients
            .values()
            .filter(|client| client.interactive)
            .filter_map(|client| self.workspaces.get(&client.workspace)?.worktree)
            .collect();
        let mut stale = Vec::new();
        for id in shown {
            let Some(worktree) = self.worktrees.get_mut(&id) else {
                continue;
            };
            let moved = marks(Path::new(&worktree.path)) != worktree.refresh.seen;
            let old = worktree
                .refresh
                .last
                .is_none_or(|last| last.elapsed() >= tuning::GIT_STATUS_INTERVAL);
            if moved || old {
                stale.push(id);
            }
        }
        for id in stale {
            self.request_status(id);
        }
        self.advance_removals();
    }

    pub(crate) fn new_worktree(
        &mut self,
        client: u64,
        project: u64,
        branch: String,
        start: Option<String>,
    ) {
        let Some(entry) = self.projects.get(&project).filter(|project| project.git) else {
            self.refuse(client, format!("project {project} is not a Git repository"));
            return;
        };
        let base_branch = entry.base_branch.clone();
        let Some(main) = self.main_path(project) else {
            self.refuse(client, format!("project {project} has no main worktree"));
            return;
        };
        let path = match worktree_path(self.config.worktree_location.as_deref(), &main, &branch) {
            Ok(path) => path,
            Err(reason) => {
                self.refuse(client, reason);
                return;
            }
        };
        self.job(move || {
            let added = (|| {
                git::check_branch_name(&main, &branch)?;
                let start = match start {
                    Some(start) => start,
                    None => git::base_branch(&main, base_branch.as_deref())
                        .unwrap_or_else(|| "HEAD".to_owned()),
                };
                git::add_worktree(&main, &path, &branch, &start)?;
                git::worktrees(&main)
            })();
            Done::Added {
                client,
                project,
                path,
                listed: added.map_err(|error| message(&error)),
            }
        });
    }

    pub(crate) fn plan_removal(&mut self, client: u64, worktree: u64) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        if self.main_path(entry.project).as_deref() == Some(Path::new(&entry.path)) {
            self.refuse(client, "the main worktree cannot be removed".to_owned());
            return;
        }
        let path = PathBuf::from(&entry.path);
        let base_branch = self
            .projects
            .get(&entry.project)
            .and_then(|project| project.base_branch.clone());
        self.job(move || {
            let findings = (|| {
                if !path.is_dir() {
                    return Ok(Findings {
                        dirty: Vec::new(),
                        unpushed: Vec::new(),
                        unpushed_total: 0,
                        unmerged: Vec::new(),
                        base: None,
                    });
                }
                let status = git::status(&path)?;
                let commits = |commits: Vec<git::Commit>| {
                    commits
                        .into_iter()
                        .map(|commit| CommitInfo {
                            oid: commit.oid,
                            subject: commit.subject,
                        })
                        .collect()
                };
                let (unpushed, unpushed_total) = git::unpushed(&path, tuning::PROMPT_COMMITS)?;
                let base = git::base_branch(&path, base_branch.as_deref());
                let unmerged = match &base {
                    Some(base) => commits(git::unmerged(&path, base)?),
                    None => Vec::new(),
                };
                anyhow::Ok(Findings {
                    dirty: status
                        .changes
                        .into_iter()
                        .map(|change| (change.code, change.path))
                        .collect(),
                    unpushed: commits(unpushed),
                    unpushed_total,
                    unmerged,
                    base,
                })
            })();
            Done::Planned {
                client,
                worktree,
                findings: findings.map_err(|error| message(&error)),
            }
        });
    }

    /// The live panes a removal kills: those spawned in the worktree and
    /// those in its workspace.
    fn removal_panes(&self, worktree: u64) -> Vec<PaneId> {
        let workspace = self
            .workspaces
            .iter()
            .find(|(_, workspace)| workspace.worktree == Some(worktree))
            .map(|(id, _)| *id);
        self.panes
            .iter()
            .filter(|(_, pane)| pane.state == PaneState::Running && pane.link.is_some())
            .filter(|(_, pane)| {
                pane.spawn_worktree == Some(worktree) || Some(pane.workspace) == workspace
            })
            .map(|(id, _)| *id)
            .collect()
    }

    fn planned(&mut self, client: u64, worktree: u64, findings: Result<Findings, String>) {
        let findings = match findings {
            Ok(findings) => findings,
            Err(reason) => {
                self.refuse(client, reason);
                return;
            }
        };
        let Some(info) = self.worktree_info(worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let panes = self.removal_panes(worktree);
        self.plans.retain(|_, plan| plan.worktree != worktree);
        self.next_plan += 1;
        let id = self.next_plan;
        self.plans.insert(
            id,
            Plan {
                worktree,
                force: !findings.dirty.is_empty(),
                panes: panes.clone(),
            },
        );
        let panes = panes
            .iter()
            .filter_map(|pane| Some(self.pane_info(*pane, self.panes.get(pane)?)))
            .collect();
        self.reply(
            client,
            FromDaemon::RemovalPlan(RemovalPlan {
                plan: id,
                worktree: info,
                dirty: findings.dirty,
                unpushed: findings.unpushed,
                unpushed_total: findings.unpushed_total,
                unmerged: findings.unmerged,
                base: findings.base,
                panes,
            }),
        );
    }

    /// Removes a worktree as its plan listed it. A live pane the plan did
    /// not list stops the removal (Invariant 20); the listed ones are
    /// killed, and Git runs once they have ended.
    pub(crate) fn remove_worktree(&mut self, client: u64, plan: u64) {
        let Some(plan) = self.plans.remove(&plan) else {
            self.refuse(
                client,
                "that removal plan is unknown or used; ask again".to_owned(),
            );
            return;
        };
        if !self.worktrees.contains_key(&plan.worktree) {
            self.refuse(client, format!("no worktree {}", plan.worktree));
            return;
        }
        let panes = self.removal_panes(plan.worktree);
        if let Some(pane) = panes.iter().find(|pane| !plan.panes.contains(pane)) {
            self.refuse(
                client,
                format!("pane {pane} started in the worktree after the prompt; ask again"),
            );
            return;
        }
        let workspace = self
            .workspaces
            .iter()
            .find(|(_, workspace)| workspace.worktree == Some(plan.worktree))
            .map(|(id, _)| *id);
        if let Some(workspace) = workspace {
            self.evacuate(workspace);
            for tab in self.workspace_tabs(workspace) {
                self.close_tab(tab);
            }
        }
        for pane in &plan.panes {
            match self.leaf_of(*pane) {
                Some((tab, leaf)) => self.close_leaf(tab, leaf.id),
                None => self.end_pane(*pane),
            }
        }
        self.removals.push(Removal {
            client,
            worktree: plan.worktree,
            force: plan.force,
            panes: plan.panes,
            since: Instant::now(),
            started: false,
        });
        self.advance_removals();
    }

    /// Starts Git for each removal whose panes have all ended, and gives up
    /// on one whose panes would not end.
    pub(crate) fn advance_removals(&mut self) {
        let mut start = Vec::new();
        let mut stuck = Vec::new();
        for removal in &mut self.removals {
            if removal.started {
                continue;
            }
            let alive: Vec<PaneId> = removal
                .panes
                .iter()
                .filter(|pane| self.closing.contains_key(pane) || self.panes.contains_key(pane))
                .copied()
                .collect();
            if alive.is_empty() {
                removal.started = true;
                start.push((removal.client, removal.worktree, removal.force));
            } else if removal.since.elapsed() >= tuning::REMOVAL_KILL_WAIT {
                stuck.push((removal.client, removal.worktree, alive));
            }
        }
        for (client, worktree, alive) in stuck {
            self.removals.retain(|removal| removal.worktree != worktree);
            let alive: Vec<String> = alive.iter().map(ToString::to_string).collect();
            self.refuse(
                client,
                format!("panes {} did not end; the worktree stays", alive.join(", ")),
            );
        }
        for (client, worktree, force) in start {
            let Some(entry) = self.worktrees.get(&worktree) else {
                continue;
            };
            let path = PathBuf::from(&entry.path);
            let Some(main) = self.main_path(entry.project) else {
                continue;
            };
            self.job(move || Done::Removed {
                client,
                worktree,
                result: git::remove_worktree(&main, &path, force).map_err(|e| message(&e)),
            });
        }
    }

    /// Forgets a removed worktree and deletes its empty workspace.
    fn forget_worktree(&mut self, worktree: u64) {
        if let Err(error) = self.store.remove_worktree(worktree) {
            eprintln!("twyx daemon: {error:#}");
        }
        self.worktrees.remove(&worktree);
        let workspaces: Vec<u64> = self
            .workspaces
            .iter()
            .filter(|(_, workspace)| workspace.worktree == Some(worktree))
            .map(|(id, _)| *id)
            .collect();
        for workspace in workspaces {
            if !self.workspace_tabs(workspace).is_empty() {
                if let Some(entry) = self.workspaces.get_mut(&workspace) {
                    entry.worktree = None;
                }
                continue;
            }
            self.evacuate(workspace);
            self.workspaces.remove(&workspace);
            if let Err(error) = self.store.delete_workspace(workspace) {
                eprintln!("twyx daemon: {error:#}");
            }
        }
        self.broadcast_workspace();
    }

    pub(crate) fn reply(&self, client: u64, message: FromDaemon) {
        if let Some(client) = self.client(client) {
            self.post(&client.out, message);
        }
    }

    /// Restores the projects and worktrees the last daemon wrote.
    pub(crate) fn restore_projects(
        &mut self,
        projects: Vec<crate::store::SavedProject>,
        worktrees: Vec<crate::store::SavedWorktree>,
    ) {
        for project in projects {
            self.projects.insert(
                project.id,
                Project {
                    git: project.git,
                    path: project.path,
                    name: project.name,
                    base_branch: project.base_branch,
                },
            );
        }
        for worktree in worktrees {
            self.worktrees.insert(
                worktree.id,
                Worktree {
                    project: worktree.project,
                    path: worktree.path,
                    admin: worktree.admin,
                    branch: worktree.branch,
                    git: None,
                    error: None,
                    refresh: Refresh::default(),
                },
            );
        }
    }
}

/// Where a new worktree goes: the config's location with `{parent}`,
/// `{repo}` and `{branch}` filled in, else
/// `<repo parent>/<repo name>.worktrees/<branch>`.
fn worktree_path(location: Option<&str>, main: &Path, branch: &str) -> Result<PathBuf, String> {
    let repo = main
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = main
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    let template = location.unwrap_or("{parent}/{repo}.worktrees/{branch}");
    let filled = template
        .replace("{parent}", &parent)
        .replace("{repo}", &repo)
        .replace("{branch}", branch);
    let filled = match filled.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => Path::new(&home).join(rest),
            None => return Err("the worktree location starts with ~ and HOME is not set".into()),
        },
        None => PathBuf::from(filled),
    };
    if !filled.is_absolute() {
        return Err(format!(
            "the worktree location {} is not an absolute path",
            filled.display()
        ));
    }
    if filled.exists() {
        return Err(format!("{} already exists", filled.display()));
    }
    Ok(filled)
}

/// The modification times of a worktree's HEAD and index.
type Marks = (Option<SystemTime>, Option<SystemTime>);

fn marks(worktree: &Path) -> Marks {
    let gitdir = git_dir(worktree);
    let modified = |name: &str| {
        gitdir
            .as_ref()
            .and_then(|dir| std::fs::metadata(dir.join(name)).ok())
            .and_then(|metadata| metadata.modified().ok())
    };
    (modified("HEAD"), modified("index"))
}

/// A worktree's own Git dir: `.git` itself, or where its `.git` file points.
fn git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    Some(if target.is_absolute() {
        target
    } else {
        worktree.join(target)
    })
}

fn base_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

/// Plans by id, for the daemon.
pub(crate) type Plans = BTreeMap<u64, Plan>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn worktrees_go_next_to_the_repository_unless_config_says_otherwise() {
        let main = Path::new("/src/twyx");
        assert_eq!(
            worktree_path(None, main, "fix/login").unwrap(),
            PathBuf::from("/src/twyx.worktrees/fix/login")
        );
        assert_eq!(
            worktree_path(Some("/wt/{repo}-{branch}"), main, "x").unwrap(),
            PathBuf::from("/wt/twyx-x")
        );
        assert!(worktree_path(Some("rel/{branch}"), main, "x").is_err());
    }
}
