//! `twyx review`: the review state of the worktree of the current
//! directory.

use anyhow::{Result, bail};
use clap::Subcommand;
use twyx_core::paths::Paths;
use twyx_protocol::client::{FromDaemon, Reviewed, ToDaemon};

use crate::projects::{connect, wait};

#[derive(Debug, Subcommand)]
pub(crate) enum ReviewCommand {
    /// Show the base and each changed file's review state.
    Status,
}

pub(crate) fn review(command: ReviewCommand) -> Result<()> {
    let ReviewCommand::Status = command;
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let worktree = crate::recipes::here_worktree(&mut connection)?;
    connection.send(&ToDaemon::Review { worktree })?;
    let review = wait(&mut connection, |message| match message {
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
        println!("  {}  {state}{hunks}", file.path);
    }
    Ok(())
}
