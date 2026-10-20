//! `twyx check`: list, run and trust the checks of the worktree of the
//! current directory, and read their results.

use anyhow::{Result, bail};
use clap::Subcommand;

use twyx_core::check::{self, Outcome};
use twyx_core::paths::Paths;
use twyx_protocol::client::{CheckInfo, CheckRun, ChecksInfo, FromDaemon, ToDaemon};
use twyx_protocol::connection::Connection;

use crate::projects::{ask, connect, print_json, wait};

#[derive(Debug, Subcommand)]
pub(crate) enum CheckCommand {
    /// List the checks: yours from config.toml, then the repository's.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Run a check and wait for its result. Exits non-zero unless it
    /// passes.
    Run {
        name: String,
        /// Print the run's pane and return at once.
        #[arg(long)]
        no_wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show the last result of each check, or one check's with its output.
    Status {
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show what the repository check file runs, and trust it as it is now.
    Trust {
        /// Trust without asking.
        #[arg(long)]
        yes: bool,
    },
}

fn checks(connection: &mut Connection, worktree: u64) -> Result<ChecksInfo> {
    connection.send(&ToDaemon::Checks { worktree })?;
    wait(connection, |message| match message {
        FromDaemon::Checks(info) => Some(info),
        _ => None,
    })
}

pub(crate) fn check(command: CheckCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = connect(&paths)?;
    let worktree = crate::recipes::here_worktree(&mut connection)?;
    match command {
        CheckCommand::List { json } => {
            let info = checks(&mut connection, worktree)?;
            if json {
                return print_json(&info);
            }
            if let Some(file) = info.file.as_ref().filter(|file| !file.trusted) {
                println!("{} is not trusted: twyx check trust shows it", file.path);
            }
            if let Some(error) = info.file.as_ref().and_then(|file| file.error.as_ref()) {
                println!("{error}");
            }
            if info.checks.is_empty() {
                println!("no checks: add [[check]] to config.toml or {}", check::FILE);
            }
            let width = name_width(&info.checks);
            for entry in &info.checks {
                let from = if entry.repository {
                    check::FILE
                } else {
                    "config.toml"
                };
                let timeout = entry.check.timeout.map_or(String::new(), |timeout| {
                    format!(", timeout {}s", timeout.as_secs())
                });
                println!(
                    "{:width$}  {}  ({from}{timeout})",
                    entry.check.name, entry.check.command
                );
            }
            Ok(())
        }
        CheckCommand::Run {
            name,
            no_wait,
            json,
        } => {
            let info = checks(&mut connection, worktree)?;
            let untrusted = info
                .checks
                .iter()
                .any(|entry| entry.check.name == name && entry.repository)
                && info.file.as_ref().is_some_and(|file| !file.trusted);
            if untrusted && !json {
                trust(&mut connection, &info, false)?;
            }
            connection.send(&ToDaemon::RunCheck {
                worktree,
                name: name.clone(),
            })?;
            let pane = wait(&mut connection, |message| match message {
                FromDaemon::CheckStarted {
                    worktree: started,
                    name: started_name,
                    pane,
                } if started == worktree && started_name == name => Some(pane),
                _ => None,
            })?;
            if no_wait {
                if json {
                    return print_json(&serde_json::json!({ "pane": pane }));
                }
                println!("check {name} runs in pane {pane}");
                return Ok(());
            }
            let run = wait(&mut connection, |message| match message {
                FromDaemon::CheckEnded(run) if run.pane == Some(pane) => Some(run),
                _ => None,
            })?;
            if json {
                print_json(&run)?;
            } else {
                print_run(&run, true);
            }
            if run.outcome != Some(Outcome::Pass) {
                bail!(
                    "check {name}: {}",
                    run.outcome.map_or("no result", Outcome::name)
                );
            }
            Ok(())
        }
        CheckCommand::Status { name, json } => {
            let info = checks(&mut connection, worktree)?;
            let entries: Vec<&CheckInfo> = info
                .checks
                .iter()
                .filter(|entry| name.as_ref().is_none_or(|name| &entry.check.name == name))
                .collect();
            if let Some(name) = &name
                && entries.is_empty()
            {
                bail!("no check named {name}; twyx check list lists them");
            }
            if json {
                return print_json(&entries);
            }
            if let Some(error) = &info.error {
                println!("no source key, so no result shows as stale: {error}");
            }
            let width = name_width(&info.checks);
            for entry in entries {
                match &entry.run {
                    Some(run) if name.is_some() => print_run(run, true),
                    Some(run) => println!("{:width$}  {}", entry.check.name, summary(run)),
                    None => println!("{:width$}  never run", entry.check.name),
                }
            }
            Ok(())
        }
        CheckCommand::Trust { yes } => {
            let info = checks(&mut connection, worktree)?;
            trust(&mut connection, &info, yes)
        }
    }
}

fn name_width(checks: &[CheckInfo]) -> usize {
    checks
        .iter()
        .map(|entry| entry.check.name.chars().count())
        .max()
        .unwrap_or(0)
}

/// The result as it shows now, with the exit code.
fn summary(run: &CheckRun) -> String {
    let Some(outcome) = run.outcome else {
        return match run.pane {
            Some(pane) => format!("running in pane {pane}"),
            None => "running".to_owned(),
        };
    };
    let exit = run
        .exit
        .map_or(String::new(), |code| format!(", exit {code}"));
    format!("{}{exit}", check::label(outcome, run.stale))
}

fn print_run(run: &CheckRun, output: bool) {
    println!("check {}: {}", run.name, summary(run));
    println!("  {}", run.command);
    if output && !run.tail.is_empty() {
        for line in run.tail.lines() {
            println!("  | {line}");
        }
    }
    if run.outcome.is_some() {
        println!("{}", check::EVIDENCE);
    }
}

/// Shows every command of the repository check file and trusts the
/// content shown.
fn trust(connection: &mut Connection, info: &ChecksInfo, yes: bool) -> Result<()> {
    let Some(file) = &info.file else {
        bail!("this worktree has no {}", check::FILE);
    };
    if let Some(error) = &file.error {
        bail!("{error}");
    }
    println!("Checks in {}", file.path);
    println!("  sha256 {}", file.hash);
    println!("  They run, each with /bin/sh -c in the worktree:");
    let repository: Vec<&CheckInfo> = info
        .checks
        .iter()
        .filter(|entry| entry.repository)
        .collect();
    let width = repository
        .iter()
        .map(|entry| entry.check.name.chars().count())
        .max()
        .unwrap_or(0);
    for entry in repository {
        println!("    {:width$}  {}", entry.check.name, entry.check.command);
    }
    println!("Trust covers this file as it is now, not the scripts its commands run.");
    if !yes
        && !ask(
            "Trust it?",
            "not trusted: run twyx check trust on a terminal, or pass --yes",
        )?
    {
        bail!("{} is not trusted, so its checks do not run", file.path);
    }
    connection.send(&ToDaemon::TrustChecks {
        worktree: info.worktree,
        hash: file.hash.clone(),
    })?;
    wait(connection, |message| match message {
        FromDaemon::Checks(info) if info.file.as_ref().is_some_and(|file| file.trusted) => Some(()),
        _ => None,
    })?;
    println!("trusted {}", file.path);
    Ok(())
}
