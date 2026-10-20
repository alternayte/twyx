//! `twyx task new`: a worktree per agent from one start ref, then each agent
//! with the prompt.

use anyhow::Result;
use clap::Subcommand;
use twyx_core::paths::Paths;
use twyx_protocol::api;

use crate::projects::{connect, print_json};

#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// Start a task in the repository of the current directory.
    New {
        /// The task name; the branch is its slug. Made from the prompt when
        /// left out.
        #[arg(long)]
        name: Option<String>,
        /// The start ref: a branch, a commit or a remote branch. The base
        /// branch when left out.
        #[arg(long = "from")]
        start: Option<String>,
        /// The agent kind, or `shell`. Give it once per worktree to run
        /// different agents; the last one fills the rest of `--count`.
        #[arg(long = "agent")]
        agents: Vec<String>,
        /// How many worktrees, each with the same prompt.
        #[arg(long, default_value_t = 1)]
        count: usize,
        /// The first message for the agent.
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn task(command: TaskCommand) -> Result<()> {
    let TaskCommand::New {
        name,
        start,
        agents,
        count,
        prompt,
        json,
    } = command;
    let paths = Paths::discover()?;
    let default = twyx_core::config::load(&paths.config.join("config.toml"))?.default_agent;
    let agents = twyx_core::task::per_worktree(&agents, count, default);
    let mut connection = connect(&paths)?;
    let workspace = crate::recipes::here(&mut connection)?;
    let added: Vec<serde_json::Value> = connection.call(
        "task.new",
        &api::NewTask {
            workspace,
            name: name.unwrap_or_default(),
            start,
            agents,
            prompt,
        },
    )?;
    if json {
        return print_json(&added);
    }
    for added in added {
        let crate::projects::Added {
            worktree, setup, ..
        } = serde_json::from_value(added)?;
        let path = worktree.path;
        println!("created {path}");
        if setup.untrusted {
            println!("  its setup file is not trusted, so setup did not run: twyx worktree setup");
        } else if setup.pane.is_some() {
            println!("  the agent starts when setup ends with exit 0");
        }
        for error in &setup.errors {
            println!("  setup: {error}");
        }
    }
    Ok(())
}
