//! `twyx review`: the review of the worktree of the current directory, and
//! shipping it: stage, commit, push and open a pull request.

use anyhow::{Result, bail};
use clap::Subcommand;
use twyx_core::paths::Paths;
use twyx_protocol::client::{FromDaemon, IndexState, Reviewed, ShipReport, ToDaemon};
use twyx_protocol::connection::Connection;

use crate::projects::{connect, wait};

#[derive(Debug, Subcommand)]
pub(crate) enum ReviewCommand {
    /// Show the review in the client that last had input.
    Open,
    /// Show the base and each changed file's review state.
    Status,
    /// Stage files on the main index; every change without paths.
    Stage {
        paths: Vec<String>,
        /// Stage the files whose changes are all reviewed.
        #[arg(long, conflicts_with = "paths")]
        reviewed: bool,
    },
    /// Unstage files; every change without paths.
    Unstage { paths: Vec<String> },
    /// Commit the staged changes. The hooks run in a pane you can watch.
    Commit {
        #[arg(short, long)]
        message: String,
    },
    /// Push the branch, and set its upstream on the first push.
    Push,
    /// Open a pull request for the branch with gh.
    Pr {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
    },
}

pub(crate) fn review(command: ReviewCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let worktree = crate::recipes::here_worktree(&mut connection)?;
    match command {
        ReviewCommand::Open => {
            connection.send(&ToDaemon::OpenReview {
                worktree: Some(worktree),
            })?;
            println!("review of worktree {worktree} shown");
            Ok(())
        }
        ReviewCommand::Status => status(&mut connection, worktree),
        ReviewCommand::Stage { paths, reviewed } => {
            if reviewed {
                connection.send(&ToDaemon::StageReviewed { worktree })?;
                return report(&mut connection);
            }
            stage(&mut connection, worktree, paths, false)
        }
        ReviewCommand::Unstage { paths } => stage(&mut connection, worktree, paths, true),
        ReviewCommand::Commit { message } => {
            connection.send(&ToDaemon::Commit { worktree, message })?;
            report(&mut connection)
        }
        ReviewCommand::Push => {
            connection.send(&ToDaemon::Push { worktree })?;
            report(&mut connection)
        }
        ReviewCommand::Pr { title, body } => {
            connection.send(&ToDaemon::OpenPullRequest {
                worktree,
                title,
                body,
            })?;
            report(&mut connection)
        }
    }
}

fn stage(
    connection: &mut Connection,
    worktree: u64,
    paths: Vec<String>,
    unstage: bool,
) -> Result<()> {
    let paths: Vec<Option<String>> = if paths.is_empty() {
        vec![None]
    } else {
        paths.into_iter().map(Some).collect()
    };
    for path in paths {
        connection.send(&ToDaemon::Stage {
            worktree,
            path,
            hunks: None,
            unstage,
        })?;
        report(connection)?;
    }
    Ok(())
}

/// Prints each report of one step until it ends; fails when the step did.
fn report(connection: &mut Connection) -> Result<()> {
    loop {
        let report: ShipReport = wait(connection, |message| match message {
            FromDaemon::Shipped(report) => Some(report),
            _ => None,
        })?;
        if let (None, Some(pane)) = (&report.result, report.pane) {
            println!("$ {}", report.command);
            println!("runs in pane {pane}");
            continue;
        }
        if report.pane.is_none() && !report.command.is_empty() {
            println!("$ {}", report.command);
        }
        if !report.output.is_empty() {
            println!("{}", report.output);
        }
        return match report.result {
            Some(Err(error)) => bail!("{error}"),
            _ => Ok(()),
        };
    }
}

fn status(connection: &mut Connection, worktree: u64) -> Result<()> {
    connection.send(&ToDaemon::Review { worktree })?;
    let review = wait(connection, |message| match message {
        FromDaemon::Review(review) => Some(review),
        _ => None,
    })?;
    if let Some(error) = review.error {
        bail!("{error}");
    }
    let base: String = review.base.chars().take(12).collect();
    println!("base {base} (merge-base with {})", review.base_ref);
    for file in &review.files {
        let state = match file.reviewed {
            Reviewed::No => "unreviewed",
            Reviewed::Partly => "partly reviewed",
            Reviewed::Yes => "reviewed",
            Reviewed::TooLarge => "too large to review",
        };
        let hunks = file
            .diff
            .as_ref()
            .map_or(String::new(), |diff| match diff.hunks.len() {
                1 => ", 1 hunk left".to_owned(),
                count => format!(", {count} hunks left"),
            });
        let index = match file.index {
            IndexState::Committed => "committed",
            IndexState::Unstaged => "unstaged",
            IndexState::Staged => "staged",
            IndexState::Partly => "partly staged",
        };
        println!("  {}  {state}{hunks}; {index}", file.path);
    }
    Ok(())
}
