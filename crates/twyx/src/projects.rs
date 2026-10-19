//! `twyx open`, `twyx ls`, `twyx workspace` and `twyx worktree`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::Serialize;

use twyx_core::paths::Paths;
use twyx_protocol::client::{
    FromDaemon, Overview, ProjectInfo, RemovalPlan, ToDaemon, WorkspaceSummary, WorktreeInfo,
};
use twyx_protocol::connection::{self as client, Connection};

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// List the workspaces.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a workspace in the client that last had input.
    Switch {
        /// A workspace id or name, a worktree name or branch, or a path.
        target: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// List the worktrees of a project, with branch and dirty state.
    List {
        /// A project name or id; the project of the current directory when
        /// left out.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create a worktree and its workspace.
    New {
        /// The branch: checked out when it exists, else made from --from.
        branch: String,
        /// Where a new branch starts; the base branch when left out.
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// A recipe to open in the new worktree's workspace.
        #[arg(long)]
        recipe: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Remove a worktree after showing what it would lose.
    Rm {
        /// A worktree name, branch, path or id.
        worktree: String,
        #[arg(long)]
        project: Option<String>,
        /// Remove without asking.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn connect(paths: &Paths) -> Result<Connection> {
    client::attach(paths, 80, 24, false, None, |_| false)
}

/// Waits for the reply `pick` takes; a refusal is the error.
pub(crate) fn wait<T>(
    connection: &mut Connection,
    mut pick: impl FnMut(FromDaemon) -> Option<T>,
) -> Result<T> {
    loop {
        match connection.receive()? {
            FromDaemon::Refused { reason } => bail!("{reason}"),
            message => {
                if let Some(value) = pick(message) {
                    return Ok(value);
                }
            }
        }
    }
}

pub(crate) fn print_json(value: &impl Serialize) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("write JSON")?
    );
    Ok(())
}

fn absolute(path: &Path) -> Result<String> {
    let path = std::path::absolute(path).with_context(|| format!("find {}", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

pub(crate) fn open_path(
    connection: &mut Connection,
    path: &Path,
    switch: bool,
) -> Result<(u64, Option<u64>)> {
    connection.send(&ToDaemon::Open {
        path: absolute(path)?,
        switch,
    })?;
    wait(connection, |message| match message {
        FromDaemon::Opened { workspace, client } => Some((workspace, client)),
        _ => None,
    })
}

fn overview(connection: &mut Connection, refresh: bool) -> Result<Overview> {
    connection.send(&ToDaemon::Overview { refresh })?;
    wait(connection, |message| match message {
        FromDaemon::Overview(overview) => Some(overview),
        _ => None,
    })
}

#[derive(Debug, Serialize)]
struct Opened<'a> {
    workspace: &'a WorkspaceSummary,
    /// The client that now shows it.
    client: Option<u64>,
}

/// Opens a path's project and shows its workspace in the client that last
/// had input. With no client attached, this terminal attaches to it.
pub fn open(path: &Path, no_switch: bool, json: bool) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let (workspace, client) = open_path(&mut connection, path, !no_switch)?;
    let overview = overview(&mut connection, false)?;
    let summary = overview
        .workspaces
        .iter()
        .find(|summary| summary.workspace == workspace)
        .context("the daemon did not list the workspace it opened")?;
    if json {
        return print_json(&Opened {
            workspace: summary,
            client,
        });
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if client.is_none() && !no_switch && interactive {
        drop(connection);
        return twyx_tui::app::attach(&paths, Some(workspace));
    }
    match client {
        Some(client) => println!("{} is open in client {client}", summary.name),
        None => println!("{} is open: workspace {}", summary.name, summary.workspace),
    }
    Ok(())
}

/// Lists every project with its worktrees, and every workspace.
pub fn ls(json: bool) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let overview = overview(&mut connection, true)?;
    if json {
        return print_json(&overview);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for project in &overview.projects {
        let kind = if project.git { "" } else { "  (directory)" };
        println!("{}{kind}", project.name);
        print_worktrees(&project.worktrees, &overview.workspaces, &home, "  ");
    }
    let loose: Vec<&WorkspaceSummary> = overview
        .workspaces
        .iter()
        .filter(|workspace| workspace.worktree.is_none())
        .collect();
    if !loose.is_empty() {
        println!("workspaces");
        for workspace in loose {
            println!(
                "  {}  {}  {}",
                workspace.name,
                tilde(&workspace.directory, &home),
                counts(workspace)
            );
        }
    }
    Ok(())
}

fn counts(workspace: &WorkspaceSummary) -> String {
    let plural =
        |count: u32, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let mut text = format!(
        "workspace {}, {}, {}",
        workspace.workspace,
        plural(workspace.tabs, "tab"),
        plural(workspace.panes, "pane")
    );
    if workspace.clients > 0 {
        text.push_str(&format!(
            ", shown in {}",
            plural(workspace.clients, "client")
        ));
    }
    text
}

fn tilde(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

/// One line per worktree: name, branch with `*` when dirty, ahead and
/// behind, path and workspace.
fn print_worktrees(
    worktrees: &[WorktreeInfo],
    workspaces: &[WorkspaceSummary],
    home: &str,
    indent: &str,
) {
    let width = worktrees
        .iter()
        .map(|worktree| worktree.name.chars().count())
        .max()
        .unwrap_or(0);
    for worktree in worktrees {
        let mut branch = branch_label(worktree);
        if let Some(git) = &worktree.git {
            if git.ahead > 0 {
                branch.push_str(&format!(" +{}", git.ahead));
            }
            if git.behind > 0 {
                branch.push_str(&format!(" -{}", git.behind));
            }
        }
        let workspace = worktree
            .workspace
            .and_then(|id| workspaces.iter().find(|summary| summary.workspace == id))
            .map(|summary| format!("  {}", counts(summary)))
            .unwrap_or_default();
        println!(
            "{indent}{:width$}  {branch}  {}{workspace}",
            worktree.name,
            tilde(&worktree.path, home),
        );
        if let Some(error) = &worktree.error {
            println!("{indent}  {error}");
        }
    }
}

fn branch_label(worktree: &WorktreeInfo) -> String {
    let git = worktree.git.as_ref();
    let mut label = git
        .map_or(worktree.branch.as_deref(), |git| git.branch.as_deref())
        .unwrap_or("(detached)")
        .to_owned();
    if git.is_some_and(|git| git.changed > 0) {
        label.push('*');
    }
    label
}

pub fn workspace(command: WorkspaceCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    match command {
        WorkspaceCommand::List { json } => {
            let overview = overview(&mut connection, false)?;
            if json {
                return print_json(&overview.workspaces);
            }
            let home = std::env::var("HOME").unwrap_or_default();
            for workspace in &overview.workspaces {
                println!(
                    "{}  {}  {}",
                    workspace.name,
                    tilde(&workspace.directory, &home),
                    counts(workspace)
                );
            }
            Ok(())
        }
        WorkspaceCommand::Switch { target, json } => {
            let overview = overview(&mut connection, false)?;
            let (workspace, client) = match find_workspace(&overview, &target)? {
                Some(workspace) => {
                    connection.send(&ToDaemon::SwitchWorkspace { workspace })?;
                    wait(&mut connection, |message| match message {
                        FromDaemon::Switched { workspace, client } => Some((workspace, client)),
                        _ => None,
                    })?
                }
                None => match find_worktree(&overview, None, &target)? {
                    Some(worktree) => open_path(&mut connection, Path::new(&worktree.path), true)?,
                    None if Path::new(&target).is_dir() => {
                        open_path(&mut connection, Path::new(&target), true)?
                    }
                    None => bail!("no workspace, worktree or directory {target}"),
                },
            };
            if json {
                return print_json(&serde_json::json!({
                    "workspace": workspace,
                    "client": client,
                }));
            }
            match client {
                Some(client) => println!("workspace {workspace} shows in client {client}"),
                None => println!("no client is attached; twyx attaches to workspace {workspace}"),
            }
            Ok(())
        }
    }
}

/// A workspace by id or name.
fn find_workspace(overview: &Overview, target: &str) -> Result<Option<u64>> {
    let found: Vec<&WorkspaceSummary> = overview
        .workspaces
        .iter()
        .filter(|workspace| workspace.workspace.to_string() == target || workspace.name == target)
        .collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.workspace)),
        _ => bail!("{target} names more than one workspace; use its id"),
    }
}

/// A worktree by id, name, `project/name`, branch or path, in one project
/// or in all of them.
fn find_worktree<'a>(
    overview: &'a Overview,
    project: Option<u64>,
    target: &str,
) -> Result<Option<&'a WorktreeInfo>> {
    let path = std::path::absolute(target)
        .ok()
        .and_then(|path| path.canonicalize().ok());
    let found: Vec<&WorktreeInfo> = overview
        .projects
        .iter()
        .filter(|entry| project.is_none_or(|project| entry.project == project))
        .flat_map(|entry| {
            entry
                .worktrees
                .iter()
                .map(move |worktree| (entry, worktree))
        })
        .filter(|(entry, worktree)| {
            worktree.worktree.to_string() == target
                || worktree.name == target
                || format!("{}/{}", entry.name, worktree.name) == target
                || worktree.branch.as_deref() == Some(target)
                || path.as_deref() == Some(Path::new(&worktree.path))
        })
        .map(|(_, worktree)| worktree)
        .collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        _ => bail!("{target} names more than one worktree; use --project or its id"),
    }
}

/// A project by id or name, else the project of the current directory,
/// which this opens.
fn find_project(
    connection: &mut Connection,
    overview: &mut Overview,
    wanted: Option<&str>,
) -> Result<ProjectInfo> {
    if let Some(wanted) = wanted {
        let found: Vec<&ProjectInfo> = overview
            .projects
            .iter()
            .filter(|project| project.project.to_string() == wanted || project.name == wanted)
            .collect();
        return match found.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no project {wanted}; twyx ls lists them"),
            _ => bail!("{wanted} names more than one project; use its id"),
        };
    }
    let here = std::env::current_dir().context("current directory")?;
    let (workspace, _) = open_path(connection, &here, false)?;
    *overview = self::overview(connection, true)?;
    let project = overview
        .workspaces
        .iter()
        .find(|summary| summary.workspace == workspace)
        .and_then(|summary| summary.project);
    overview
        .projects
        .iter()
        .find(|entry| Some(entry.project) == project && entry.git)
        .cloned()
        .with_context(|| format!("{} is not in a Git repository", here.display()))
}

pub fn worktree(command: WorktreeCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let mut overview = overview(&mut connection, true)?;
    match command {
        WorktreeCommand::List { project, json } => {
            let project = find_project(&mut connection, &mut overview, project.as_deref())?;
            if json {
                return print_json(&project.worktrees);
            }
            let home = std::env::var("HOME").unwrap_or_default();
            print_worktrees(&project.worktrees, &overview.workspaces, &home, "");
            Ok(())
        }
        WorktreeCommand::New {
            branch,
            from,
            project: _,
            recipe: Some(recipe),
            json,
        } => {
            if from.is_some() {
                bail!("--from does not go with --recipe; the recipe's [worktree] from applies");
            }
            crate::recipes::run(&mut connection, &recipe, Some(branch), true, json)
        }
        WorktreeCommand::New {
            branch,
            from,
            project,
            recipe: None,
            json,
        } => {
            let project = find_project(&mut connection, &mut overview, project.as_deref())?;
            connection.send(&ToDaemon::NewWorktree {
                project: project.project,
                branch,
                start: from,
            })?;
            let (worktree, workspace) = wait(&mut connection, |message| match message {
                FromDaemon::WorktreeAdded {
                    worktree,
                    workspace,
                } => Some((worktree, workspace)),
                _ => None,
            })?;
            if json {
                return print_json(&serde_json::json!({
                    "worktree": worktree,
                    "workspace": workspace,
                }));
            }
            println!(
                "created {} at {}: workspace {workspace}",
                worktree.name, worktree.path
            );
            Ok(())
        }
        WorktreeCommand::Rm {
            worktree,
            project,
            yes,
            json,
        } => {
            let project = match project {
                Some(project) => {
                    Some(find_project(&mut connection, &mut overview, Some(&project))?.project)
                }
                None => None,
            };
            let target = find_worktree(&overview, project, &worktree)?
                .with_context(|| format!("no worktree {worktree}; twyx ls lists them"))?
                .worktree;
            connection.send(&ToDaemon::PlanRemoval { worktree: target })?;
            let plan = wait(&mut connection, |message| match message {
                FromDaemon::RemovalPlan(plan) => Some(plan),
                _ => None,
            })?;
            if !json {
                print_plan(&plan);
            }
            let confirmed = yes
                || (!json
                    && ask(
                        "Remove it?",
                        "not removed: pass --yes to remove without a terminal to ask on",
                    )?);
            if !confirmed {
                if json {
                    return print_json(&serde_json::json!({ "plan": plan, "removed": false }));
                }
                println!("kept {}", plan.worktree.name);
                return Ok(());
            }
            connection.send(&ToDaemon::RemoveWorktree { plan: plan.plan })?;
            wait(&mut connection, |message| match message {
                FromDaemon::WorktreeRemoved { worktree } if worktree == target => Some(()),
                _ => None,
            })?;
            if json {
                return print_json(&serde_json::json!({ "plan": plan, "removed": true }));
            }
            println!("removed {}", plan.worktree.name);
            Ok(())
        }
    }
}

fn print_plan(plan: &RemovalPlan) {
    let worktree = &plan.worktree;
    println!(
        "Remove worktree {} ({}) at {}",
        worktree.name,
        branch_label(worktree),
        worktree.path
    );
    if plan.dirty.is_empty()
        && plan.unpushed.is_empty()
        && plan.unmerged.is_empty()
        && plan.panes.is_empty()
    {
        println!("  It has no changes, no unpushed or unmerged commits and no live panes.");
        return;
    }
    if !plan.dirty.is_empty() {
        println!("  Changes that are discarded:");
        for (code, path) in &plan.dirty {
            println!("    {code} {path}");
        }
    }
    if !plan.unpushed.is_empty() {
        println!("  Commits on no remote:");
        for commit in &plan.unpushed {
            println!("    {} {}", commit.oid, commit.subject);
        }
        let more = (plan.unpushed_total as usize).saturating_sub(plan.unpushed.len());
        if more > 0 {
            println!("    and {more} older");
        }
    }
    if !plan.unmerged.is_empty() {
        let base = plan
            .base
            .as_deref()
            .map(|base| base.trim_start_matches("refs/heads/"))
            .unwrap_or("the base branch");
        println!("  Commits not in {base}:");
        for commit in &plan.unmerged {
            println!("    {} {}", commit.oid, commit.subject);
        }
    }
    if !plan.panes.is_empty() {
        println!("  Live panes that are killed:");
        for pane in &plan.panes {
            let title = if pane.title.is_empty() {
                &pane.cwd
            } else {
                &pane.title
            };
            println!("    pane {}  {title}", pane.pane);
        }
    }
}

/// Asks on the terminal. With no terminal to ask on, `without` is the
/// error.
pub(crate) fn ask(question: &str, without: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("{without}");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush().context("write the question")?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("read the answer")?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}
