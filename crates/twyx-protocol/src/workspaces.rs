//! The sidebar and the workspace actions both clients share: the overview
//! the daemon keeps current, the workspace mode cursor, and the requests,
//! dialogs and answers of the worktree actions.

use std::collections::HashMap;

use twyx_core::keys::Command;
use twyx_core::layout::LeafId;

use crate::client::{
    ChecksInfo, FromDaemon, InboxItem, Overview, PaneInfo, RemovalPlan, ToDaemon, WorkspaceInfo,
};
use crate::dialog::{Dialog, Purpose};
use crate::lists::CheckKey;
use crate::sidebar::{self, Kind, Row, Target};

/// What a command or an answer asks of the client.
#[derive(Debug, Default, PartialEq)]
pub struct Effect {
    pub requests: Vec<ToDaemon>,
    pub dialog: Option<Dialog>,
    /// Open the workspace switcher.
    pub switcher: bool,
    /// Leave workspace mode.
    pub leave: bool,
    /// The sidebar docked or undocked: the tab area changed.
    pub resized: bool,
    /// Close the open list.
    pub close: bool,
}

#[derive(Debug, Default)]
pub struct Workspaces {
    pub overview: Option<Overview>,
    /// The inbox, newest first, as the daemon last sent it.
    pub inbox: Vec<InboxItem>,
    /// Whether the sidebar stays beside the panes.
    pub docked: bool,
    /// The workspace mode cursor, as a row index.
    pub cursor: usize,
    pub rows: Vec<Row>,
    /// The outcome of the last action, for the status bar.
    pub message: Option<String>,
    /// A worktree this client asked for, to show once it exists.
    switch_to_new: bool,
    /// The worktree this client asked to set up.
    setup_for: Option<u64>,
    /// The checks of the worktree the checks overlay last asked for.
    pub checks: Option<ChecksInfo>,
    /// Whether the client's dialog shows these lines in full; every line
    /// fits when it is not set. A trust prompt that cannot show its whole
    /// file offers no key to trust it.
    pub fits: Option<fn(&[String]) -> bool>,
}

impl Workspaces {
    /// A client's workspaces: its sidebar docked or not at first, and its
    /// dialog's rule for what it shows in full.
    pub fn new(docked: bool, fits: Option<fn(&[String]) -> bool>) -> Self {
        Self {
            docked,
            fits,
            ..Self::default()
        }
    }

    /// Builds the rows again from the overview.
    pub fn refresh(
        &mut self,
        shown: Option<&WorkspaceInfo>,
        focus: Option<LeafId>,
        panes: &HashMap<u64, PaneInfo>,
    ) {
        let Some(overview) = &self.overview else {
            self.rows.clear();
            return;
        };
        self.rows = sidebar::rows(
            overview,
            shown.map(|shown| shown.workspace),
            focus,
            |pane| panes.get(&pane).map(PaneInfo::label).unwrap_or_default(),
        );
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        if self
            .rows
            .get(self.cursor)
            .is_some_and(|row| row.kind == Kind::Heading)
        {
            self.cursor = self.step(self.cursor, true).unwrap_or(self.cursor);
        }
    }

    /// The next row the cursor can stand on, past headings.
    fn step(&self, from: usize, down: bool) -> Option<usize> {
        let choosable = |index: &usize| self.rows[*index].kind != Kind::Heading;
        if down {
            (from + 1..self.rows.len()).find(choosable)
        } else {
            (0..from).rev().find(choosable)
        }
    }

    /// Puts the cursor on the client's own workspace.
    pub fn focus_current(&mut self) {
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| row.current && row.kind != Kind::Pane)
            .or_else(|| self.rows.iter().position(|row| row.current))
        {
            self.cursor = index;
        }
    }

    /// The request that shows a row's workspace, worktree or pane.
    pub fn open(&self, index: usize) -> Option<ToDaemon> {
        Some(match self.rows.get(index)?.target.clone()? {
            Target::Workspace(workspace) => ToDaemon::SwitchWorkspace { workspace },
            Target::Open(path) => ToDaemon::Open { path, switch: true },
            Target::Pane(leaf) => ToDaemon::FocusLeaf { leaf },
        })
    }

    /// The workspaces in sidebar order, for next and previous.
    fn order(&self) -> Vec<u64> {
        let mut order = Vec::new();
        for row in &self.rows {
            if let Some(Target::Workspace(workspace)) = row.target
                && row.kind != Kind::Pane
                && !order.contains(&workspace)
            {
                order.push(workspace);
            }
        }
        order
    }

    /// The project and worktree a worktree action applies to: the cursor's
    /// row in workspace mode, else the shown workspace.
    fn subject(&self, in_mode: bool, shown: Option<&WorkspaceInfo>) -> (Option<u64>, Option<u64>) {
        if in_mode && let Some(row) = self.rows.get(self.cursor) {
            return (row.project, row.worktree);
        }
        let worktree = shown.and_then(|shown| shown.worktree.as_ref());
        (
            worktree.map(|worktree| worktree.project).or_else(|| {
                let id = shown?.workspace;
                self.overview
                    .as_ref()?
                    .workspaces
                    .iter()
                    .find(|summary| summary.workspace == id)?
                    .project
            }),
            worktree.map(|worktree| worktree.worktree),
        )
    }

    fn git_project(&self, project: u64) -> bool {
        self.overview.as_ref().is_some_and(|overview| {
            overview
                .projects
                .iter()
                .any(|entry| entry.project == project && entry.git)
        })
    }

    /// Runs one workspace command.
    pub fn command(
        &mut self,
        command: Command,
        in_mode: bool,
        shown: Option<&WorkspaceInfo>,
    ) -> Effect {
        let mut effect = Effect::default();
        match command {
            Command::ToggleSidebar => {
                self.docked = !self.docked;
                effect.resized = true;
            }
            Command::Switcher => {
                effect.switcher = true;
                effect.leave = true;
            }
            Command::SidebarUp | Command::SidebarDown => {
                let down = command == Command::SidebarDown;
                self.cursor = self.step(self.cursor, down).unwrap_or(self.cursor);
            }
            Command::SidebarOpen => {
                effect.requests.extend(self.open(self.cursor));
                effect.leave = true;
            }
            Command::NextWorkspace | Command::PreviousWorkspace => {
                let order = self.order();
                let at = shown
                    .and_then(|shown| order.iter().position(|id| *id == shown.workspace))
                    .unwrap_or(0);
                let next = if command == Command::NextWorkspace {
                    (at + 1) % order.len().max(1)
                } else {
                    (at + order.len().max(1) - 1) % order.len().max(1)
                };
                if let Some(workspace) = order.get(next) {
                    effect.requests.push(ToDaemon::SwitchWorkspace {
                        workspace: *workspace,
                    });
                }
            }
            Command::NewWorktree => match self.subject(in_mode, shown) {
                (Some(project), _) if self.git_project(project) => {
                    effect.dialog = Some(Dialog::Prompt {
                        title: "new worktree: branch".to_owned(),
                        text: String::new(),
                        purpose: Purpose::NewWorktree { project },
                    });
                }
                _ => self.message = Some("not in a Git repository".to_owned()),
            },
            Command::RemoveWorktree => match self.subject(in_mode, shown) {
                (_, Some(worktree)) => {
                    effect.requests.push(ToDaemon::PlanRemoval { worktree });
                }
                _ => self.message = Some("no worktree here".to_owned()),
            },
            Command::SetupWorktree => match self.subject(in_mode, shown) {
                (_, Some(worktree)) => {
                    self.setup_for = Some(worktree);
                    effect.requests.push(ToDaemon::Setup { worktree });
                }
                _ => self.message = Some("no worktree here".to_owned()),
            },
            _ => {}
        }
        effect
    }

    /// Takes a prompt's text.
    pub fn prompted(&mut self, purpose: Purpose, text: String) -> Effect {
        let mut effect = Effect::default();
        if let Purpose::Note {
            worktree,
            path,
            lines,
        } = &purpose
        {
            effect.requests.push(ToDaemon::AddNote {
                worktree: *worktree,
                path: path.clone(),
                lines: *lines,
                body: text.clone(),
            });
        }
        if let Purpose::RenameTab { tab } = purpose {
            effect.requests.push(ToDaemon::RenameTab {
                tab,
                name: text.clone(),
            });
        }
        if purpose == Purpose::CopySearch {
            effect
                .requests
                .push(ToDaemon::Copy(crate::holder::CopyAction::Search(
                    text.clone(),
                )));
        }
        if let Purpose::Commit { worktree } = purpose {
            effect.requests.push(ToDaemon::Commit {
                worktree,
                message: text.clone(),
            });
        }
        if let Purpose::NewWorktree { project } = purpose {
            self.switch_to_new = true;
            self.message = Some(format!("creating {text}"));
            effect.requests.push(ToDaemon::NewWorktree {
                project,
                branch: text,
                start: None,
            });
        }
        effect
    }

    /// Takes a confirmation's key.
    pub fn confirmed(&mut self, purpose: Purpose, key: char) -> Effect {
        let mut effect = Effect::default();
        match purpose {
            Purpose::RemoveWorktree { plan } => {
                effect.requests.push(ToDaemon::RemoveWorktree {
                    plan,
                    delete_branch: key == 'd',
                });
            }
            Purpose::TrustSetup { worktree, hash } => {
                effect
                    .requests
                    .push(ToDaemon::TrustSetup { worktree, hash });
            }
            Purpose::Resume { this, all } => {
                effect.requests.push(ToDaemon::Resume {
                    panes: if key == 'a' { all } else { vec![this] },
                });
            }
            Purpose::SendNotes { worktree, panes } => {
                if let Some(pane) = key
                    .to_digit(10)
                    .and_then(|digit| panes.get((digit as usize).checked_sub(1)?))
                {
                    effect.requests.push(ToDaemon::SendNotes {
                        worktree,
                        pane: *pane,
                    });
                }
            }
            Purpose::Push { worktree } => effect.requests.push(ToDaemon::Push { worktree }),
            Purpose::TrustChecks { worktree, hash } => {
                effect
                    .requests
                    .push(ToDaemon::TrustChecks { worktree, hash });
            }
            Purpose::NewWorktree { .. }
            | Purpose::Commit { .. }
            | Purpose::PullRequest { .. }
            | Purpose::Integrations
            | Purpose::RenameTab { .. }
            | Purpose::CopySearch
            | Purpose::NewTask { .. }
            | Purpose::Note { .. } => {}
        }
        effect
    }

    /// Takes an answer from the daemon.
    pub fn answer(&mut self, message: &FromDaemon) -> Effect {
        let mut effect = Effect::default();
        match message {
            FromDaemon::Overview(overview) => self.overview = Some(overview.clone()),
            FromDaemon::Inbox(items) => self.inbox = items.clone(),
            FromDaemon::Delivered { result, .. } => {
                self.message = Some(match result {
                    Ok(()) => "sent to the agent".to_owned(),
                    Err(reason) => format!("on the clipboard: {reason}"),
                });
            }
            FromDaemon::TaskStarted { branch, note, .. } => {
                self.message = Some(match note {
                    Some(note) => format!("started {branch}; {note}"),
                    None => format!("started {branch}"),
                });
            }
            FromDaemon::WorktreeAdded {
                worktree,
                workspace,
                setup,
            } => {
                let mut text = format!("created {}", worktree.name);
                if setup.untrusted {
                    text.push_str("; its setup file is not trusted (ctrl-g w u)");
                }
                if let Some(error) = setup.errors.first() {
                    text = format!("{text}; setup: {error}");
                }
                self.message = Some(text);
                if std::mem::take(&mut self.switch_to_new) {
                    effect.requests.push(ToDaemon::SwitchWorkspace {
                        workspace: *workspace,
                    });
                }
            }
            FromDaemon::RemovalPlan(plan) => effect.dialog = Some(removal(plan)),
            FromDaemon::WorktreeRemoved { branch, .. } => {
                self.message = Some(match branch {
                    Some(Ok(branch)) => format!("removed the worktree and the branch {branch}"),
                    Some(Err(error)) => format!("removed the worktree; {error}"),
                    None => "removed the worktree".to_owned(),
                });
            }
            FromDaemon::SetupFile(info) => {
                match (info, self.setup_for) {
                    (Some(info), _) if !info.trusted => {
                        let mut lines = vec![format!("{} in the worktree", info.path)];
                        lines.push(format!("sha256 {}", info.hash));
                        if let Some(error) = &info.error {
                            lines.push(error.clone());
                        }
                        lines.extend(
                            info.shared
                                .iter()
                                .map(|path| format!("links {path} from the main worktree")),
                        );
                        lines.extend(
                            info.commands
                                .iter()
                                .map(|command| format!("runs {command}")),
                        );
                        lines.push("Trust covers this file as it is now, not the scripts its commands run.".to_owned());
                        effect.dialog = Some(Dialog::Confirm {
                            title: "trust the setup file".to_owned(),
                            keys: self.trust_keys(&lines, "trust and set up"),
                            lines: self.shown_lines(lines, "twyx worktree setup"),
                            purpose: Purpose::TrustSetup {
                                worktree: info.worktree,
                                hash: info.hash.clone(),
                            },
                        });
                    }
                    (_, Some(worktree)) => effect.requests.push(ToDaemon::RunSetup { worktree }),
                    (_, None) => {}
                }
            }
            FromDaemon::SetupTrusted { worktree } => {
                effect.requests.push(ToDaemon::RunSetup {
                    worktree: *worktree,
                });
            }
            FromDaemon::SetupDone { report, .. } => {
                self.setup_for = None;
                let mut parts = Vec::new();
                if !report.copied.is_empty() {
                    parts.push(format!("copied {}", report.copied.len()));
                }
                if !report.linked.is_empty() {
                    parts.push(format!("linked {}", report.linked.len()));
                }
                if report.pane.is_some() {
                    parts.push("setup runs in the setup tab".to_owned());
                }
                parts.extend(report.errors.iter().cloned());
                self.message = Some(if parts.is_empty() {
                    "nothing to set up".to_owned()
                } else {
                    parts.join("; ")
                });
            }
            FromDaemon::Shipped(report) => self.message = Some(shipped(report)),
            FromDaemon::Checks(info) => self.checks = Some(info.clone()),
            FromDaemon::CheckStarted { worktree, name, .. } => {
                self.message = Some(format!("check {name} runs; ctrl-g e and o show it"));
                effect.requests.push(ToDaemon::Checks {
                    worktree: *worktree,
                });
            }
            FromDaemon::CheckEnded(run) => {
                if let Some(outcome) = run.outcome {
                    let mut text = format!("check {}: {}", run.name, outcome.name());
                    if outcome == twyx_core::check::Outcome::Pass {
                        text = format!("{text}. {}", twyx_core::check::EVIDENCE);
                    }
                    self.message = Some(text);
                }
                if self
                    .checks
                    .as_ref()
                    .is_some_and(|info| info.worktree == run.worktree)
                {
                    effect.requests.push(ToDaemon::Checks {
                        worktree: run.worktree,
                    });
                }
            }
            FromDaemon::Refused { reason } => {
                self.switch_to_new = false;
                self.message = Some(reason.clone());
            }
            _ => {}
        }
        effect
    }

    /// The new task form for the shown workspace's repository.
    pub fn new_task(
        &mut self,
        shown: Option<&WorkspaceInfo>,
        default_agent: Option<&str>,
    ) -> Effect {
        let mut effect = Effect::default();
        match shown {
            Some(shown) if shown.worktree.is_some() => {
                effect.dialog = Some(Dialog::Form {
                    title: "new task".to_owned(),
                    fields: vec![
                        ("name".to_owned(), String::new()),
                        ("from".to_owned(), String::new()),
                        (
                            "agents".to_owned(),
                            default_agent.unwrap_or("shell").to_owned(),
                        ),
                        ("count".to_owned(), "1".to_owned()),
                        ("prompt".to_owned(), String::new()),
                    ],
                    at: 4,
                    purpose: Purpose::NewTask {
                        workspace: shown.workspace,
                    },
                });
            }
            _ => self.message = Some("not in a Git repository".to_owned()),
        }
        effect
    }

    /// Takes a form's values.
    pub fn submitted(&mut self, purpose: Purpose, values: Vec<String>) -> Effect {
        let mut effect = Effect::default();
        let field = |at: usize| values.get(at).cloned().unwrap_or_default();
        if let Purpose::PullRequest { worktree } = purpose {
            effect.requests.push(ToDaemon::OpenPullRequest {
                worktree,
                title: field(0),
                body: field(1),
            });
        }
        if let Purpose::NewTask { workspace } = purpose {
            let named: Vec<String> = field(2).split(',').map(str::to_owned).collect();
            let count = field(3).parse().unwrap_or(1);
            let agents = twyx_core::task::per_worktree(&named, count, None);
            self.message = Some(format!(
                "starting {} task worktree{}",
                agents.len(),
                if agents.len() == 1 { "" } else { "s" }
            ));
            effect.requests.push(ToDaemon::NewTask {
                workspace,
                name: field(0),
                start: Some(field(1)).filter(|start| !start.is_empty()),
                agents,
                prompt: Some(field(4)).filter(|prompt| !prompt.is_empty()),
            });
        }
        effect
    }

    /// A trust prompt's keys: `y` only when the dialog shows every line.
    fn trust_keys(&self, lines: &[String], word: &str) -> Vec<(char, String)> {
        if self.fits.is_none_or(|fits| fits(lines)) {
            vec![('y', word.to_owned())]
        } else {
            Vec::new()
        }
    }

    /// A trust prompt's lines, or where to read them in full when the dialog
    /// cannot show them all.
    fn shown_lines(&self, lines: Vec<String>, shell: &str) -> Vec<String> {
        if self.fits.is_none_or(|fits| fits(&lines)) {
            return lines;
        }
        let mut short: Vec<String> = lines.into_iter().take(2).collect();
        short.push(format!(
            "The file is too long to show here. Run {shell} in a shell to read all of it and trust it."
        ));
        short
    }

    /// Does what a key in the checks list of `worktree` asks.
    pub fn check(&mut self, worktree: u64, key: CheckKey) -> Effect {
        let mut effect = Effect::default();
        let run = |name: &str| {
            self.checks
                .as_ref()
                .filter(|info| info.worktree == worktree)
                .and_then(|info| info.checks.iter().find(|entry| entry.check.name == name))
                .and_then(|entry| entry.run.as_ref())
        };
        match key {
            CheckKey::Stay => {}
            CheckKey::Close => effect.close = true,
            CheckKey::Run(name) => effect.requests.push(ToDaemon::RunCheck { worktree, name }),
            CheckKey::Cancel(name) => {
                effect
                    .requests
                    .push(ToDaemon::CancelCheck { worktree, name });
            }
            CheckKey::Show(name) => match run(&name).and_then(|run| run.pane) {
                Some(pane) => {
                    effect.close = true;
                    effect.requests.push(ToDaemon::ShowPane { pane });
                }
                None => self.message = Some(format!("check {name} has no output to show")),
            },
            CheckKey::Trust => match self
                .checks
                .as_ref()
                .and_then(|checks| self.trust_checks(checks))
            {
                Some(prompt) => effect.dialog = Some(prompt),
                None => self.message = Some("no untrusted repository checks".to_owned()),
            },
        }
        effect
    }

    /// The prompt for a tab's new name, with its name now.
    pub fn rename_tab(shown: Option<&WorkspaceInfo>, tab: Option<u64>) -> Option<Dialog> {
        let tab = tab?;
        let name = shown?
            .tabs
            .iter()
            .find(|each| each.tab == tab)
            .map(|each| each.name.clone())
            .unwrap_or_default();
        Some(Dialog::Prompt {
            title: "rename tab".to_owned(),
            text: name,
            purpose: Purpose::RenameTab { tab },
        })
    }

    /// The prompt for a copy mode search.
    pub fn copy_search() -> Dialog {
        Dialog::Prompt {
            title: "search".to_owned(),
            text: String::new(),
            purpose: Purpose::CopySearch,
        }
    }

    /// The trust prompt for a worktree's repository check file, when it
    /// needs one.
    pub fn trust_checks(&self, info: &ChecksInfo) -> Option<Dialog> {
        let file = info.file.as_ref().filter(|file| !file.trusted)?;
        let mut lines = vec![format!("{} in the worktree", file.path)];
        lines.push(format!("sha256 {}", file.hash));
        if let Some(error) = &file.error {
            lines.push(error.clone());
        }
        lines.extend(
            info.checks
                .iter()
                .filter(|entry| entry.repository)
                .map(|entry| format!("{} runs {}", entry.check.name, entry.check.command)),
        );
        lines.push(
            "Trust covers this file as it is now, not the scripts its commands run.".to_owned(),
        );
        Some(Dialog::Confirm {
            title: "trust the repository's checks".to_owned(),
            keys: self.trust_keys(&lines, "trust"),
            lines: self.shown_lines(lines, "twyx check trust"),
            purpose: Purpose::TrustChecks {
                worktree: info.worktree,
                hash: file.hash.clone(),
            },
        })
    }
}

/// A shipping step for the status bar: its command and how it went, with a
/// pull request's URL.
fn shipped(report: &crate::client::ShipReport) -> String {
    let last = report
        .output
        .lines()
        .rev()
        .find(|line| line.trim().starts_with("https://"))
        .or_else(|| {
            report
                .output
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
        })
        .map(str::trim)
        .unwrap_or_default();
    match &report.result {
        None => format!("{} runs in its tab", report.command),
        Some(Ok(())) if last.is_empty() => format!("{}: done", report.command),
        Some(Ok(())) => format!("{}: {last}", report.command),
        Some(Err(error)) if report.command.is_empty() => error.clone(),
        Some(Err(error)) if last.is_empty() => format!("{}: {error}", report.command),
        Some(Err(error)) => format!("{}: {error}: {last}", report.command),
    }
}

/// The removal prompt: what the worktree would lose, and the keys.
fn removal(plan: &RemovalPlan) -> Dialog {
    let mut lines = vec![format!("{} at {}", plan.worktree.name, plan.worktree.path)];
    for (code, path) in plan.dirty.iter().take(8) {
        lines.push(format!("discards {code} {path}"));
    }
    if plan.dirty.len() > 8 {
        lines.push(format!("discards {} more changes", plan.dirty.len() - 8));
    }
    match plan.unpushed_total {
        0 => {}
        1 => lines.push("1 commit is on no remote".to_owned()),
        count => lines.push(format!("{count} commits are on no remote")),
    }
    let base = plan
        .base
        .as_deref()
        .map(|base| base.trim_start_matches("refs/heads/"))
        .unwrap_or("the base branch");
    for commit in &plan.unmerged {
        lines.push(format!(
            "not in {base}: {} {}",
            commit.oid.get(..12).unwrap_or(&commit.oid),
            commit.subject
        ));
    }
    for pane in &plan.panes {
        lines.push(format!("kills pane {} {}", pane.pane, pane.label()));
    }
    if lines.len() == 1 {
        lines.push(
            "It has no changes, no unpushed or unmerged commits and no live panes.".to_owned(),
        );
    }
    let mut keys = vec![('y', "remove".to_owned())];
    if let Some(branch) = &plan.branch {
        let how = if plan.unmerged.is_empty() { "-d" } else { "-D" };
        keys.push(('d', format!("remove and delete {branch} ({how})")));
    }
    Dialog::Confirm {
        title: "remove worktree".to_owned(),
        lines,
        keys,
        purpose: Purpose::RemoveWorktree { plan: plan.plan },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::client::{GitState, PaneRef, ProjectInfo, WorkspaceSummary, WorktreeInfo};

    fn workspaces() -> Workspaces {
        let worktree = |id: u64, name: &str, workspace: Option<u64>| WorktreeInfo {
            worktree: id,
            project: 1,
            name: name.to_owned(),
            path: format!("/src/{name}"),
            branch: Some(name.to_owned()),
            git: Some(GitState {
                branch: Some(name.to_owned()),
                upstream: None,
                ahead: 0,
                behind: 0,
                changed: 0,
            }),
            error: None,
            workspace,
        };
        let summary = |id: u64, worktree: Option<u64>, project: Option<u64>| WorkspaceSummary {
            workspace: id,
            name: format!("w{id}"),
            directory: String::new(),
            project,
            worktree,
            tabs: 1,
            panes: vec![PaneRef {
                pane: id,
                tab: id,
                leaf: LeafId(id),
                mark: None,
                agent: None,
            }],
            clients: 0,
        };
        let mut workspaces = Workspaces {
            overview: Some(Overview {
                projects: vec![ProjectInfo {
                    project: 1,
                    name: "app".to_owned(),
                    path: "/src/app/.git".to_owned(),
                    git: true,
                    worktrees: vec![worktree(10, "main", Some(2)), worktree(11, "fix", None)],
                }],
                workspaces: vec![summary(1, None, None), summary(2, Some(10), Some(1))],
            }),
            ..Workspaces::default()
        };
        workspaces.refresh(None, None, &HashMap::new());
        workspaces
    }

    #[test]
    fn the_cursor_opens_worktrees_workspaces_and_panes() {
        let mut workspaces = workspaces();
        // spaces, app, main, its pane, fix, default, its pane
        assert_eq!(workspaces.rows.len(), 7);
        assert_eq!(
            workspaces.open(2),
            Some(ToDaemon::SwitchWorkspace { workspace: 2 })
        );
        assert_eq!(
            workspaces.open(4),
            Some(ToDaemon::Open {
                path: "/src/fix".to_owned(),
                switch: true
            })
        );
        assert_eq!(
            workspaces.open(6),
            Some(ToDaemon::FocusLeaf { leaf: LeafId(1) })
        );
        workspaces.cursor = 5;
        let effect = workspaces.command(Command::SidebarDown, true, None);
        assert_eq!(workspaces.cursor, 6);
        assert!(effect.requests.is_empty());
        workspaces.command(Command::SidebarDown, true, None);
        assert_eq!(workspaces.cursor, 6, "the cursor stops at the last row");
        workspaces.cursor = 1;
        workspaces.command(Command::SidebarUp, true, None);
        assert_eq!(workspaces.cursor, 1, "the cursor never stands on a heading");
    }

    #[test]
    fn a_new_worktree_asks_for_a_branch_then_shows_it() {
        let mut workspaces = workspaces();
        workspaces.cursor = 4;
        let effect = workspaces.command(Command::NewWorktree, true, None);
        let Some(Dialog::Prompt { purpose, .. }) = effect.dialog else {
            panic!("no prompt");
        };
        let effect = workspaces.prompted(purpose, "topic".to_owned());
        assert_eq!(
            effect.requests,
            vec![ToDaemon::NewWorktree {
                project: 1,
                branch: "topic".to_owned(),
                start: None
            }]
        );
        let info = workspaces.overview.as_ref().unwrap().projects[0].worktrees[1].clone();
        let effect = workspaces.answer(&FromDaemon::WorktreeAdded {
            worktree: info,
            workspace: 9,
            setup: Default::default(),
        });
        assert_eq!(
            effect.requests,
            vec![ToDaemon::SwitchWorkspace { workspace: 9 }]
        );
        // A plain directory's workspace has no worktree to make.
        workspaces.cursor = 5;
        let effect = workspaces.command(Command::NewWorktree, true, None);
        assert!(effect.dialog.is_none());
    }

    #[test]
    fn removal_asks_with_the_plan_and_deletes_the_branch_only_on_d() {
        let mut workspaces = workspaces();
        workspaces.cursor = 4;
        let effect = workspaces.command(Command::RemoveWorktree, true, None);
        assert_eq!(
            effect.requests,
            vec![ToDaemon::PlanRemoval { worktree: 11 }]
        );
        let plan = RemovalPlan {
            plan: 4,
            worktree: workspaces.overview.as_ref().unwrap().projects[0].worktrees[1].clone(),
            dirty: Vec::new(),
            unpushed: Vec::new(),
            unpushed_total: 0,
            unmerged: Vec::new(),
            base: None,
            branch: Some("fix".to_owned()),
            panes: Vec::new(),
        };
        let effect = workspaces.answer(&FromDaemon::RemovalPlan(plan));
        let Some(Dialog::Confirm { keys, purpose, .. }) = effect.dialog else {
            panic!("no confirmation");
        };
        assert_eq!(keys.len(), 2);
        assert_eq!(
            workspaces.confirmed(purpose.clone(), 'y').requests,
            vec![ToDaemon::RemoveWorktree {
                plan: 4,
                delete_branch: false
            }]
        );
        assert_eq!(
            workspaces.confirmed(purpose, 'd').requests,
            vec![ToDaemon::RemoveWorktree {
                plan: 4,
                delete_branch: true
            }]
        );
    }
}
