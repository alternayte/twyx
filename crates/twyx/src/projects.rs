//! `twyx open`, `twyx ls`, `twyx workspace` and `twyx worktree`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::json;

use twyx_core::paths::Paths;
use twyx_protocol::api;
use twyx_protocol::client::{
    Overview, ProjectInfo, RemovalPlan, SetupInfo, SetupReport, WorkspaceSummary, WorktreeInfo,
};
use twyx_protocol::connection as client;

/// An API v1 connection to the daemon.
pub(crate) type Connection = api::Client;

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
        /// Delete the worktree's branch too: with git branch -d, or -D when
        /// the prompt listed its unmerged commits.
        #[arg(long)]
        delete_branch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show a worktree's .twyx/worktree.toml, trust it as it is now, and set
    /// the worktree up: copies, links and the setup commands in a pane.
    Setup {
        /// A worktree name, branch, path or id.
        worktree: String,
        #[arg(long)]
        project: Option<String>,
        /// Trust without asking.
        #[arg(long)]
        yes: bool,
    },
}

pub(crate) fn connect(paths: &Paths) -> Result<Connection> {
    client::attach_api(paths)
}

/// Where `open` and `workspace.switch` show a workspace.
#[derive(Debug, Deserialize)]
struct Shown {
    workspace: u64,
    client: Option<u64>,
}

/// What `worktree.new` answers.
#[derive(Debug, Deserialize)]
pub(crate) struct Added {
    pub worktree: WorktreeInfo,
    pub workspace: u64,
    pub setup: SetupReport,
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
    let shown: Shown = connection.call(
        "open",
        &api::Open {
            path: absolute(path)?,
            switch,
        },
    )?;
    Ok((shown.workspace, shown.client))
}

pub(crate) fn overview(connection: &mut Connection, refresh: bool) -> Result<Overview> {
    connection.call("overview", &api::OverviewParams { refresh })
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
        plural(workspace.panes.len() as u32, "pane")
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
                    let shown: Shown =
                        connection.call("workspace.switch", &api::WorkspaceRef { workspace })?;
                    (shown.workspace, shown.client)
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
                return print_json(&json!({
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
            let Added {
                worktree,
                workspace,
                setup,
            } = connection.call(
                "worktree.new",
                &api::NewWorktree {
                    project: project.project,
                    branch,
                    start: from,
                },
            )?;
            if json {
                return print_json(&json!({
                    "worktree": worktree,
                    "workspace": workspace,
                    "setup": setup,
                }));
            }
            println!(
                "created {} at {}: workspace {workspace}",
                worktree.name, worktree.path
            );
            print_setup(&setup);
            if setup.untrusted && std::io::stdin().is_terminal() {
                set_up(&mut connection, worktree.worktree, false)?;
            }
            Ok(())
        }
        WorktreeCommand::Setup {
            worktree,
            project,
            yes,
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
            set_up(&mut connection, target, yes)
        }
        WorktreeCommand::Rm {
            worktree,
            project,
            yes,
            delete_branch,
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
            let plan: RemovalPlan = connection.call(
                "worktree.plan_removal",
                &api::WorktreeRef { worktree: target },
            )?;
            if delete_branch && plan.branch.is_none() {
                bail!("the worktree's HEAD is detached: no branch to delete");
            }
            if !json {
                print_plan(&plan);
                if let (true, Some(branch)) = (delete_branch, &plan.branch) {
                    if plan.unmerged.is_empty() {
                        println!("  The branch {branch} is deleted with git branch -d.");
                    } else {
                        println!(
                            "  The branch {branch} is deleted with git branch -D, losing the {} commits not in the base above.",
                            plan.unmerged.len()
                        );
                    }
                }
            }
            let confirmed = yes
                || (!json
                    && ask(
                        "Remove it?",
                        "not removed: pass --yes to remove without a terminal to ask on",
                    )?);
            if !confirmed {
                if json {
                    return print_json(&json!({ "plan": plan, "removed": false }));
                }
                println!("kept {}", plan.worktree.name);
                return Ok(());
            }
            #[derive(Deserialize)]
            struct Removed {
                branch: Option<Result<String, String>>,
            }
            let Removed { branch } = connection.call(
                "worktree.remove",
                &api::RemoveWorktree {
                    plan: plan.plan,
                    delete_branch,
                },
            )?;
            if json {
                return print_json(&json!({
                    "plan": plan,
                    "removed": true,
                    "branch": branch,
                }));
            }
            println!("removed {}", plan.worktree.name);
            match branch {
                Some(Ok(branch)) => println!("deleted the branch {branch}"),
                Some(Err(error)) => println!("{error}"),
                None => {}
            }
            Ok(())
        }
    }
}

fn print_setup(report: &SetupReport) {
    if !report.copied.is_empty() {
        println!(
            "copied from the main worktree: {}",
            report.copied.join(", ")
        );
    }
    if !report.linked.is_empty() {
        println!(
            "linked from the main worktree: {}",
            report.linked.join(", ")
        );
    }
    if let Some(pane) = report.pane {
        println!("the setup commands run in pane {pane}");
    }
    if report.untrusted {
        println!(".twyx/worktree.toml is not trusted, so its links and commands did not run");
    }
    for error in &report.errors {
        println!("setup: {error}");
    }
}

/// Shows a worktree's setup file, trusts it on a yes, and sets the
/// worktree up.
fn set_up(connection: &mut Connection, worktree: u64, yes: bool) -> Result<()> {
    let info: Option<SetupInfo> =
        connection.call("worktree.setup_file", &api::WorktreeRef { worktree })?;
    if let Some(info) = info.filter(|info| !info.trusted) {
        if let Some(error) = &info.error {
            bail!("{error}");
        }
        println!("{} in the worktree", info.path);
        println!("  sha256 {}", info.hash);
        for path in &info.shared {
            println!("  links {path} from the main worktree");
        }
        for command in &info.commands {
            println!("  runs {command}");
        }
        println!("Trust covers this file as it is now, not the scripts its commands run.");
        if !yes
            && !ask(
                "Trust it?",
                "not trusted: run twyx worktree setup on a terminal, or pass --yes",
            )?
        {
            println!("not trusted; nothing of it ran");
            return Ok(());
        }
        let () = connection.call(
            "worktree.trust_setup",
            &api::TrustFile {
                worktree,
                hash: info.hash.clone(),
            },
        )?;
    }
    let report: SetupReport =
        connection.call("worktree.run_setup", &api::WorktreeRef { worktree })?;
    print_setup(&report);
    Ok(())
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
