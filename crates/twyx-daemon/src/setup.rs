//! Worktree setup: gitignored files copied from the main worktree, shared
//! paths linked from it, and the setup commands run in a pane.
//!
//! `.worktreeinclude` needs no trust: it only copies files. The links and
//! commands of `.twyx/worktree.toml` run only while its exact content is
//! trusted (Invariant 13).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use twyx_core::setup::{self, Setup};
use twyx_protocol::client::{FromDaemon, Launch, PaneState, SetupInfo, SetupReport};

use crate::daemon::{Daemon, Pane};
use crate::git;
use crate::worktrees::{Done, Then};

/// What follows a setup.
#[derive(Debug)]
pub(crate) enum After {
    /// A new worktree: tell the client, or open its recipe.
    Added { client: u64, then: Option<Then> },
    /// A setup the user asked for again.
    Again { client: u64 },
}

/// A setup file as read once: its bytes hashed and parsed together.
struct Read {
    hash: String,
    setup: Result<Setup, String>,
}

fn read(worktree: &Path) -> Option<Read> {
    let path = worktree.join(setup::FILE);
    let bytes = std::fs::read(&path).ok()?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let setup = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{}: not UTF-8", path.display()))
        .and_then(|text| setup::parse(&path, text).map_err(|error| error.to_string()));
    Some(Read { hash, setup })
}

/// Copies with a copy-on-write clone where the file system has one, and
/// never over an existing path.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let clone: &[&str] = if cfg!(target_os = "macos") {
        &["-c"]
    } else {
        &["--reflink=auto"]
    };
    let attempt = |extra: &[&str]| {
        Command::new("cp")
            .args(extra)
            .args(["-R", "-p"])
            .arg(from)
            .arg(to)
            .stdin(Stdio::null())
            .output()
    };
    let output = match attempt(clone) {
        Ok(output) if output.status.success() => return Ok(()),
        _ => attempt(&[]).map_err(|error| format!("cp {}: {error}", from.display()))?,
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "cp {}: {}",
            from.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The copies and links, off the daemon's thread.
fn prepare_files(main: &Path, target: &Path, shared: Option<&[String]>) -> SetupReport {
    let mut report = SetupReport::default();
    let include = main.join(setup::INCLUDE_FILE);
    if include.is_file() {
        match git::ignored_matching(main, &include) {
            Ok(paths) => {
                for path in paths {
                    let to = target.join(&path);
                    if to.symlink_metadata().is_ok() {
                        continue;
                    }
                    match copy(&main.join(&path), &to) {
                        Ok(()) => report.copied.push(path),
                        Err(error) => report.errors.push(error),
                    }
                }
            }
            Err(error) => report.errors.push(format!("{error:#}")),
        }
    }
    for path in shared.unwrap_or_default() {
        let from = main.join(path);
        let to = target.join(path);
        if !from.exists() {
            report
                .errors
                .push(format!("shared {path} is not in the main worktree"));
            continue;
        }
        if to.symlink_metadata().is_ok() {
            report
                .errors
                .push(format!("{path} is already in the new worktree; not linked"));
            continue;
        }
        let linked = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::os::unix::fs::symlink(&from, &to));
        match linked {
            Ok(()) => report.linked.push(path.clone()),
            Err(error) => report.errors.push(format!("link {path}: {error}")),
        }
    }
    report
}

impl Daemon {
    fn trusted_setup(&self, project: u64, read: &Read) -> bool {
        self.store
            .trusted(project, setup::FILE, &read.hash)
            .unwrap_or(false)
    }

    pub(crate) fn setup_file(&self, client: u64, worktree: u64) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let info = read(Path::new(&entry.path)).map(|read| {
            let trusted = self.trusted_setup(entry.project, &read);
            let (shared, commands, error) = match read.setup {
                Ok(setup) => (setup.shared, setup.commands, None),
                Err(error) => (Vec::new(), Vec::new(), Some(error)),
            };
            SetupInfo {
                worktree,
                path: setup::FILE.to_owned(),
                hash: read.hash,
                trusted,
                shared,
                commands,
                error,
            }
        });
        self.reply(client, FromDaemon::SetupFile(info));
    }

    pub(crate) fn trust_setup(&mut self, client: u64, worktree: u64, shown: String) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let project = entry.project;
        match read(Path::new(&entry.path)) {
            Some(read) if read.hash == shown => {
                match self.store.trust(project, setup::FILE, &shown) {
                    Ok(()) => self.reply(client, FromDaemon::SetupTrusted { worktree }),
                    Err(error) => self.refuse(client, format!("{error:#}")),
                }
            }
            Some(_) => self.refuse(
                client,
                format!(
                    "{} changed since it was shown; look at it again",
                    setup::FILE
                ),
            ),
            None => self.refuse(client, format!("the worktree has no {}", setup::FILE)),
        }
    }

    /// Sets a worktree up: the copies and links on a job thread, then the
    /// commands in a pane.
    pub(crate) fn prepare(&mut self, worktree: u64, after: After) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            return;
        };
        let target = PathBuf::from(&entry.path);
        let project = entry.project;
        let Some(main) = self.main_path(project) else {
            return;
        };
        let file = read(&target);
        let untrusted = file
            .as_ref()
            .is_some_and(|read| !self.trusted_setup(project, read));
        let mut errors = Vec::new();
        let setup = match file {
            Some(read) if !untrusted => match read.setup {
                Ok(setup) => Some(setup),
                Err(error) => {
                    errors.push(error);
                    None
                }
            },
            _ => None,
        };
        self.job(move || {
            let mut report = prepare_files(
                &main,
                &target,
                setup.as_ref().map(|setup| setup.shared.as_slice()),
            );
            report.untrusted = untrusted;
            report.errors.extend(errors);
            Done::Prepared {
                worktree,
                report,
                script: setup
                    .filter(|setup| !setup.commands.is_empty())
                    .map(|setup| setup.script()),
                after,
            }
        });
    }

    pub(crate) fn prepared(
        &mut self,
        worktree: u64,
        mut report: SetupReport,
        script: Option<String>,
        after: After,
    ) {
        let workspace = self
            .workspaces
            .iter()
            .find(|(_, workspace)| workspace.worktree == Some(worktree))
            .map(|(id, _)| *id);
        if let (Some(script), Some(workspace), Some(entry)) =
            (script, workspace, self.worktrees.get(&worktree))
        {
            let cwd = entry.path.clone();
            let record = Pane {
                link: None,
                cols: 80,
                rows: 24,
                state: PaneState::Running,
                screen: None,
                launch: Launch::Command(vec!["/bin/sh".to_owned(), "-c".to_owned(), script]),
                spawn_worktree: Some(worktree),
                cwd,
                title: String::new(),
                history: self.config.history,
                workspace,
                env: Vec::new(),
            };
            match self.command_tab(workspace, "setup", record) {
                Ok(pane) => report.pane = Some(pane.raw()),
                Err(error) => report.errors.push(error),
            }
        }
        match after {
            After::Added { client, then } => match (then, workspace) {
                (Some((recipe, switch)), Some(workspace)) => {
                    self.open_recipe(client, workspace, &recipe, switch);
                }
                (_, workspace) => {
                    if let (Some(info), Some(workspace)) = (self.worktree_info(worktree), workspace)
                    {
                        self.reply(
                            client,
                            FromDaemon::WorktreeAdded {
                                worktree: info,
                                workspace,
                                setup: report,
                            },
                        );
                    }
                }
            },
            After::Again { client } => {
                self.reply(client, FromDaemon::SetupDone { worktree, report });
            }
        }
    }
}
