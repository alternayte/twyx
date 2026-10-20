//! `twyx review`: the review of the worktree of the current directory, and
//! shipping it: stage, commit, push and open a pull request.

use anyhow::{Result, bail};
use clap::Subcommand;
use twyx_core::paths::Paths;
use twyx_protocol::api;
use twyx_protocol::client::{IndexState, ReviewInfo, Reviewed, ShipReport};

use crate::projects::{Connection, connect, print_json};

#[derive(Debug, Subcommand)]
pub(crate) enum ReviewCommand {
    /// Show the review in the client that last had input.
    Open {
        #[arg(long)]
        json: bool,
    },
    /// Show the base and each changed file's review state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stage files on the main index; every change without paths.
    Stage {
        paths: Vec<String>,
        /// Stage the files whose changes are all reviewed.
        #[arg(long, conflicts_with = "paths")]
        reviewed: bool,
        #[arg(long)]
        json: bool,
    },
    /// Unstage files; every change without paths.
    Unstage {
        paths: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Commit the staged changes. The hooks run in a pane you can watch.
    Commit {
        #[arg(short, long)]
        message: String,
        #[arg(long)]
        json: bool,
    },
    /// Push the branch, and set its upstream on the first push.
    Push {
        #[arg(long)]
        json: bool,
    },
    /// Open a pull request for the branch with gh.
    Pr {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn review(command: ReviewCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let worktree = crate::recipes::here_worktree(&mut connection)?;
    match command {
        ReviewCommand::Open { json } => {
            let () = connection.call("review.open", &api::WorktreeRef { worktree })?;
            if json {
                return print_json(&serde_json::json!({ "worktree": worktree }));
            }
            say!("review of worktree {worktree} shown");
            Ok(())
        }
        ReviewCommand::Status { json } => status(&mut connection, worktree, json),
        ReviewCommand::Stage {
            paths,
            reviewed,
            json,
        } => {
            if reviewed {
                let shipped =
                    connection.call("review.stage_reviewed", &api::WorktreeRef { worktree })?;
                return report(&shipped, json);
            }
            stage(&mut connection, worktree, paths, false, json)
        }
        ReviewCommand::Unstage { paths, json } => {
            stage(&mut connection, worktree, paths, true, json)
        }
        ReviewCommand::Commit { message, json } => {
            let shipped = connection.call(
                "review.commit",
                &api::Commit {
                    worktree,
                    message,
                    wait: true,
                },
            )?;
            report(&shipped, json)
        }
        ReviewCommand::Push { json } => {
            let shipped = connection.call(
                "review.push",
                &api::Push {
                    worktree,
                    wait: true,
                },
            )?;
            report(&shipped, json)
        }
        ReviewCommand::Pr { title, body, json } => {
            let shipped = connection.call(
                "review.pull_request",
                &api::PullRequest {
                    worktree,
                    title,
                    body,
                    wait: true,
                },
            )?;
            report(&shipped, json)
        }
    }
}

fn stage(
    connection: &mut Connection,
    worktree: u64,
    paths: Vec<String>,
    unstage: bool,
    json: bool,
) -> Result<()> {
    let paths: Vec<Option<String>> = if paths.is_empty() {
        vec![None]
    } else {
        paths.into_iter().map(Some).collect()
    };
    let mut reports: Vec<ShipReport> = Vec::new();
    for path in paths {
        reports.push(connection.call(
            "review.stage",
            &api::Stage {
                worktree,
                path,
                hunks: None,
                unstage,
            },
        )?);
    }
    if json {
        return print_json(&reports);
    }
    for shipped in &reports {
        report(shipped, false)?;
    }
    Ok(())
}

/// Prints what a step ran and printed; fails when the step did.
fn report(report: &ShipReport, json: bool) -> Result<()> {
    if json {
        print_json(report)?;
    } else {
        if !report.command.is_empty() {
            say!("$ {}", report.command);
        }
        if let Some(pane) = report.pane {
            say!("ran in pane {pane}");
        }
        if !report.output.is_empty() {
            say!("{}", report.output);
        }
    }
    match &report.result {
        Some(Err(error)) => bail!("{error}"),
        _ => Ok(()),
    }
}

fn status(connection: &mut Connection, worktree: u64, json: bool) -> Result<()> {
    let review: ReviewInfo = connection.call("review.status", &api::WorktreeRef { worktree })?;
    if json {
        return print_json(&review);
    }
    if let Some(error) = review.error {
        bail!("{error}");
    }
    let base: String = review.base.chars().take(12).collect();
    say!("base {base} (merge-base with {})", review.base_ref);
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
        say!("  {}  {state}{hunks}; {index}", file.path);
    }
    Ok(())
}
