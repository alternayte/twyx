//! `twyx recipe`: list, run and trust recipes.

use anyhow::{Context, Result, bail};
use clap::Subcommand;

use serde::Deserialize;
use twyx_core::paths::Paths;
use twyx_protocol::api;
use twyx_protocol::client::{RecipeInfo, RecipeSource};

use crate::projects::{Connection, ask, connect, open_path, print_json};

#[derive(Debug, Subcommand)]
pub enum RecipeCommand {
    /// List the recipes of the current directory's workspace: built in,
    /// yours, and the repository's.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Open a recipe's tabs in the current directory's workspace, and show
    /// them in the client that last had input.
    Run {
        name: String,
        /// Open it in a new worktree on this branch.
        #[arg(long)]
        branch: Option<String>,
        /// Switch no client.
        #[arg(long)]
        no_switch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show what a repository recipe runs, and trust it as it is now.
    Trust {
        name: String,
        /// Trust without asking.
        #[arg(long)]
        yes: bool,
    },
}

/// The workspace of the current directory, which this opens.
pub(crate) fn here(connection: &mut Connection) -> Result<u64> {
    let here = std::env::current_dir().context("current directory")?;
    Ok(open_path(connection, &here, false)?.0)
}

/// The Git worktree of the current directory, which this opens.
pub(crate) fn here_worktree(connection: &mut Connection) -> Result<u64> {
    let workspace = here(connection)?;
    crate::projects::overview(connection, false)?
        .workspaces
        .iter()
        .find(|summary| summary.workspace == workspace)
        .and_then(|summary| summary.worktree)
        .context("this directory is not in a Git worktree")
}

fn recipes(connection: &mut Connection, workspace: u64) -> Result<Vec<RecipeInfo>> {
    connection.call("recipe.list", &api::WorkspaceRef { workspace })
}

pub fn recipe(command: RecipeCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    match command {
        RecipeCommand::List { json } => {
            let workspace = here(&mut connection)?;
            let recipes = recipes(&mut connection, workspace)?;
            if json {
                return print_json(&recipes);
            }
            for recipe in &recipes {
                let source = match &recipe.source {
                    RecipeSource::BuiltIn => "built in".to_owned(),
                    RecipeSource::User { path } => path.clone(),
                    RecipeSource::Repository { path, trusted, .. } => {
                        format!("{path}{}", if *trusted { "" } else { ", not trusted" })
                    }
                };
                let shadowed = if recipe.shadowed {
                    ", hidden by the one above"
                } else {
                    ""
                };
                say!("{}  ({source}{shadowed})", recipe.name);
                match &recipe.error {
                    Some(error) => say!("  {error}"),
                    None if !recipe.description.is_empty() => say!("  {}", recipe.description),
                    None => {}
                }
            }
            Ok(())
        }
        RecipeCommand::Run {
            name,
            branch,
            no_switch,
            json,
        } => run(&mut connection, &name, branch, !no_switch, json),
        RecipeCommand::Trust { name, yes } => {
            let workspace = here(&mut connection)?;
            trust(&mut connection, workspace, &name, yes)
        }
    }
}

/// Runs a recipe in the current directory's workspace, asking for trust on
/// the terminal when a repository recipe needs it.
pub fn run(
    connection: &mut Connection,
    name: &str,
    branch: Option<String>,
    switch: bool,
    json: bool,
) -> Result<()> {
    let workspace = here(connection)?;
    let recipe = recipes(connection, workspace)?
        .into_iter()
        .find(|recipe| recipe.name == name && !recipe.shadowed)
        .with_context(|| format!("no recipe {name}; twyx recipe list lists them"))?;
    if let RecipeSource::Repository { trusted: false, .. } = recipe.source
        && !json
    {
        trust(connection, workspace, name, false)?;
    }
    #[derive(Deserialize)]
    struct Opened {
        workspace: u64,
        tabs: Vec<u64>,
        client: Option<u64>,
    }
    let Opened {
        workspace,
        tabs,
        client,
    } = connection.call(
        "recipe.run",
        &api::RunRecipe {
            workspace,
            name: name.to_owned(),
            branch,
            switch,
        },
    )?;
    if json {
        return print_json(&serde_json::json!({
            "workspace": workspace,
            "tabs": tabs,
            "client": client,
        }));
    }
    let plural = if tabs.len() == 1 { "" } else { "s" };
    match client {
        Some(client) => say!(
            "{name} opened {} tab{plural} in workspace {workspace}, shown in client {client}",
            tabs.len()
        ),
        None => say!(
            "{name} opened {} tab{plural} in workspace {workspace}",
            tabs.len()
        ),
    }
    Ok(())
}

/// Shows every command, directory and variable of a repository recipe and
/// trusts the content shown.
fn trust(connection: &mut Connection, workspace: u64, name: &str, yes: bool) -> Result<()> {
    let recipe = recipes(connection, workspace)?
        .into_iter()
        .find(|recipe| recipe.name == name && !recipe.shadowed)
        .with_context(|| format!("no recipe {name}; twyx recipe list lists them"))?;
    let RecipeSource::Repository { path, hash, .. } = &recipe.source else {
        bail!("{name} is not a repository recipe; only those need trust");
    };
    if let Some(error) = &recipe.error {
        bail!("{error}");
    }
    say!("Recipe {name} in {path}");
    say!("  sha256 {hash}");
    if recipe.new_worktree {
        say!("  It opens in a new worktree.");
    }
    say!("  It runs:");
    let width = recipe
        .panes
        .iter()
        .map(|pane| pane.name.chars().count())
        .max()
        .unwrap_or(0);
    for pane in &recipe.panes {
        let what = match (&pane.command, pane.agent) {
            (Some(command), _) => command.clone(),
            (None, true) => "the default agent, else your shell".to_owned(),
            (None, false) => "your shell".to_owned(),
        };
        say!("    {:width$}  {what}", pane.name);
        if let Some(dir) = &pane.dir {
            say!("    {:width$}  in {dir}", "");
        }
        for (key, value) in &pane.env {
            say!("    {:width$}  with {key}={value}", "");
        }
    }
    say!("Trust covers this file as it is now, not the scripts its commands run.");
    if !yes
        && !ask(
            "Trust it?",
            "not trusted: run twyx recipe trust on a terminal, or pass --yes",
        )?
    {
        bail!("{name} is not trusted, so it did not run");
    }
    let () = connection.call(
        "recipe.trust",
        &api::TrustRecipe {
            workspace,
            name: name.to_owned(),
            hash: hash.clone(),
        },
    )?;
    say!("trusted {name}");
    Ok(())
}
