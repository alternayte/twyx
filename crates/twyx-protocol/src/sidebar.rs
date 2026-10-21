//! The rows both clients' sidebars show, from the daemon's overview, in two
//! sections: spaces (projects, their worktrees and workspaces, and the plain
//! panes of each) and agents (one row per agent session). Every pane shows
//! once.

use serde::{Deserialize, Serialize};
use twyx_core::inbox::most_urgent;
use twyx_core::layout::LeafId;
use twyx_core::theme::AgentState as Mark;

use crate::client::{Overview, WorkspaceSummary, WorktreeInfo};

/// What choosing a row does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// Show this workspace.
    Workspace(u64),
    /// Open this path: a worktree with no workspace yet.
    Open(String),
    /// Focus this pane, in its workspace.
    Pane(LeafId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A section's name, or the blank line before it. Nothing to choose.
    Heading,
    Project,
    Worktree,
    Workspace,
    Pane,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub kind: Kind,
    /// How far the row is indented: 0 for a project or a loose workspace.
    pub depth: u8,
    pub label: String,
    /// The branch of a worktree, `detached` when HEAD is; the worktree or
    /// workspace of an agent session.
    pub detail: String,
    pub dirty: bool,
    /// The row of the workspace the client shows, or of its focused pane.
    pub current: bool,
    pub target: Option<Target>,
    /// The worktree a row stands for, for worktree actions.
    pub worktree: Option<u64>,
    /// The project a row belongs to.
    pub project: Option<u64>,
    /// A pane's status mark, or the most urgent mark of a project's,
    /// worktree's or workspace's panes.
    pub mark: Option<Mark>,
}

/// The rows, in order. `shown` is the client's workspace and `focused` its
/// focused leaf; `title` names a pane.
pub fn rows(
    overview: &Overview,
    shown: Option<u64>,
    focused: Option<LeafId>,
    title: impl Fn(u64) -> String,
) -> Vec<Row> {
    let mut rows = vec![heading("spaces")];
    let workspace = |id: u64| {
        overview
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace == id)
    };
    let panes =
        |rows: &mut Vec<Row>, summary: &WorkspaceSummary, depth: u8, project: Option<u64>| {
            for pane in summary.panes.iter().filter(|pane| pane.agent.is_none()) {
                rows.push(Row {
                    kind: Kind::Pane,
                    depth,
                    label: title(pane.pane),
                    detail: String::new(),
                    dirty: false,
                    current: shown == Some(summary.workspace) && focused == Some(pane.leaf),
                    target: Some(Target::Pane(pane.leaf)),
                    worktree: summary.worktree,
                    project,
                    mark: pane.mark,
                });
            }
        };
    for project in &overview.projects {
        let own = project
            .worktrees
            .iter()
            .find_map(|worktree| worktree.workspace)
            .or_else(|| {
                overview
                    .workspaces
                    .iter()
                    .find(|summary| {
                        summary.project == Some(project.project) && summary.worktree.is_none()
                    })
                    .map(|summary| summary.workspace)
            });
        if !project.git {
            // A plain directory is its own workspace.
            let summary = overview
                .workspaces
                .iter()
                .find(|summary| summary.project == Some(project.project));
            rows.push(Row {
                kind: Kind::Project,
                depth: 0,
                label: project.name.clone(),
                detail: String::new(),
                dirty: false,
                current: summary.is_some_and(|summary| shown == Some(summary.workspace)),
                target: summary.map(|summary| Target::Workspace(summary.workspace)),
                worktree: None,
                project: Some(project.project),
                mark: summary.and_then(rollup),
            });
            if let Some(summary) = summary {
                panes(&mut rows, summary, 1, Some(project.project));
            }
            continue;
        }
        let main = project
            .worktrees
            .iter()
            .find(|worktree| worktree.name == "main");
        rows.push(Row {
            kind: Kind::Project,
            depth: 0,
            label: project.name.clone(),
            detail: String::new(),
            dirty: false,
            current: false,
            target: main.map(target).or_else(|| own.map(Target::Workspace)),
            worktree: None,
            project: Some(project.project),
            mark: most_urgent(
                project
                    .worktrees
                    .iter()
                    .filter_map(|worktree| worktree.workspace.and_then(workspace))
                    .filter_map(rollup),
            ),
        });
        for worktree in &project.worktrees {
            rows.push(Row {
                kind: Kind::Worktree,
                depth: 1,
                label: worktree.name.clone(),
                detail: branch(worktree),
                dirty: worktree.git.as_ref().is_some_and(|git| git.changed > 0),
                current: worktree.workspace.is_some() && worktree.workspace == shown,
                target: Some(target(worktree)),
                worktree: Some(worktree.worktree),
                project: Some(project.project),
                mark: worktree.workspace.and_then(workspace).and_then(rollup),
            });
            if let Some(summary) = worktree.workspace.and_then(workspace) {
                panes(&mut rows, summary, 2, Some(project.project));
            }
        }
    }
    // Workspaces of no project, and those whose worktree is gone.
    for summary in overview.workspaces.iter().filter(|summary| {
        summary.worktree.is_none()
            && !overview
                .projects
                .iter()
                .any(|project| !project.git && summary.project == Some(project.project))
    }) {
        rows.push(Row {
            kind: Kind::Workspace,
            depth: 0,
            label: summary.name.clone(),
            detail: String::new(),
            dirty: false,
            current: shown == Some(summary.workspace),
            target: Some(Target::Workspace(summary.workspace)),
            worktree: None,
            project: summary.project,
            mark: rollup(summary),
        });
        panes(&mut rows, summary, 1, summary.project);
    }
    let agents = agents(overview, &rows, shown, focused);
    if !agents.is_empty() {
        rows.push(heading(""));
        rows.push(heading("agents"));
        rows.extend(agents);
    }
    rows
}

fn heading(label: &str) -> Row {
    Row {
        kind: Kind::Heading,
        depth: 0,
        label: label.to_owned(),
        detail: String::new(),
        dirty: false,
        current: false,
        target: None,
        worktree: None,
        project: None,
        mark: None,
    }
}

/// One row per agent session, in the order of the spaces rows: the agent's
/// kind, and the worktree or workspace it runs in.
fn agents(
    overview: &Overview,
    spaces: &[Row],
    shown: Option<u64>,
    focused: Option<LeafId>,
) -> Vec<Row> {
    let mut order: Vec<u64> = Vec::new();
    for row in spaces {
        if let Some(Target::Workspace(workspace)) = row.target
            && !order.contains(&workspace)
        {
            order.push(workspace);
        }
    }
    let rank = |summary: &WorkspaceSummary| {
        order
            .iter()
            .position(|id| *id == summary.workspace)
            .unwrap_or(order.len())
    };
    let mut summaries: Vec<&WorkspaceSummary> = overview.workspaces.iter().collect();
    summaries.sort_by_key(|summary| rank(summary));
    let mut rows = Vec::new();
    for summary in summaries {
        let place = summary
            .worktree
            .and_then(|id| {
                overview
                    .projects
                    .iter()
                    .flat_map(|project| &project.worktrees)
                    .find(|worktree| worktree.worktree == id)
            })
            .map_or_else(|| summary.name.clone(), |worktree| worktree.name.clone());
        for pane in &summary.panes {
            let Some(agent) = &pane.agent else { continue };
            rows.push(Row {
                kind: Kind::Pane,
                depth: 0,
                label: agent.clone(),
                detail: place.clone(),
                dirty: false,
                current: shown == Some(summary.workspace) && focused == Some(pane.leaf),
                target: Some(Target::Pane(pane.leaf)),
                worktree: summary.worktree,
                project: summary.project,
                mark: pane.mark,
            });
        }
    }
    rows
}

/// The most urgent mark of a workspace's panes.
fn rollup(summary: &WorkspaceSummary) -> Option<Mark> {
    most_urgent(summary.panes.iter().filter_map(|pane| pane.mark))
}

fn target(worktree: &WorktreeInfo) -> Target {
    match worktree.workspace {
        Some(workspace) => Target::Workspace(workspace),
        None => Target::Open(worktree.path.clone()),
    }
}

fn branch(worktree: &WorktreeInfo) -> String {
    worktree
        .git
        .as_ref()
        .map_or(worktree.branch.as_deref(), |git| git.branch.as_deref())
        .unwrap_or("detached")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{GitState, PaneRef, ProjectInfo};

    fn worktree(id: u64, name: &str, workspace: Option<u64>, changed: u32) -> WorktreeInfo {
        WorktreeInfo {
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
                changed,
            }),
            error: None,
            workspace,
        }
    }

    fn summary(
        id: u64,
        name: &str,
        project: Option<u64>,
        worktree: Option<u64>,
        panes: &[u64],
    ) -> WorkspaceSummary {
        WorkspaceSummary {
            workspace: id,
            name: name.to_owned(),
            directory: format!("/{name}"),
            project,
            worktree,
            tabs: 1,
            panes: panes
                .iter()
                .map(|pane| PaneRef {
                    pane: *pane,
                    tab: 1,
                    leaf: LeafId(*pane),
                    mark: match pane {
                        5 => Some(Mark::Working),
                        6 => Some(Mark::NeedsApproval),
                        _ => None,
                    },
                    agent: (*pane == 6).then(|| "claude".to_owned()),
                })
                .collect(),
            clients: 0,
        }
    }

    #[test]
    fn spaces_list_worktrees_with_their_plain_panes_and_agents_list_each_session_once() {
        let overview = Overview {
            projects: vec![
                ProjectInfo {
                    project: 1,
                    name: "twyx".to_owned(),
                    path: "/src/twyx/.git".to_owned(),
                    git: true,
                    worktrees: vec![
                        worktree(10, "main", Some(2), 1),
                        worktree(11, "fix", None, 0),
                    ],
                },
                ProjectInfo {
                    project: 2,
                    name: "notes".to_owned(),
                    path: "/notes".to_owned(),
                    git: false,
                    worktrees: Vec::new(),
                },
            ],
            workspaces: vec![
                summary(1, "default", None, None, &[7]),
                summary(2, "twyx/main", Some(1), Some(10), &[5, 6]),
                summary(3, "notes", Some(2), None, &[]),
            ],
        };
        let rows = rows(&overview, Some(2), Some(LeafId(6)), |pane| {
            format!("p{pane}")
        });
        let lines: Vec<(Kind, u8, &str, &str, bool, bool)> = rows
            .iter()
            .map(|row| {
                (
                    row.kind,
                    row.depth,
                    row.label.as_str(),
                    row.detail.as_str(),
                    row.dirty,
                    row.current,
                )
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                (Kind::Heading, 0, "spaces", "", false, false),
                (Kind::Project, 0, "twyx", "", false, false),
                (Kind::Worktree, 1, "main", "main", true, true),
                (Kind::Pane, 2, "p5", "", false, false),
                (Kind::Worktree, 1, "fix", "fix", false, false),
                (Kind::Project, 0, "notes", "", false, false),
                (Kind::Workspace, 0, "default", "", false, false),
                (Kind::Pane, 1, "p7", "", false, false),
                (Kind::Heading, 0, "", "", false, false),
                (Kind::Heading, 0, "agents", "", false, false),
                (Kind::Pane, 0, "claude", "main", false, true),
            ]
        );
        assert_eq!(rows[4].target, Some(Target::Open("/src/fix".to_owned())));
        assert_eq!(rows[1].target, Some(Target::Workspace(2)));
        assert_eq!(rows[5].target, Some(Target::Workspace(3)));
        assert_eq!(rows[10].target, Some(Target::Pane(LeafId(6))));
        let marks: Vec<Option<Mark>> = rows.iter().map(|row| row.mark).collect();
        assert_eq!(
            marks,
            vec![
                None,
                Some(Mark::NeedsApproval),
                Some(Mark::NeedsApproval),
                Some(Mark::Working),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(Mark::NeedsApproval),
            ],
            "each row rolls up the most urgent mark of its panes"
        );
    }
}
