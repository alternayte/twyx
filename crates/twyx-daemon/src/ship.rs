//! Commit, push and pull request: each runs in a pane in a tab of its
//! own, so the user watches the hooks and answers any prompt. The pane's
//! exit and last lines go back to the client that asked. twyx runs none of
//! them without that client's request.

use std::path::{Path, PathBuf};

use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::client::{FromDaemon, Launch, PaneState, ShipReport};
use twyx_protocol::holder::ToHolder;

use crate::daemon::{Daemon, Pane};
use crate::git;
use crate::holders::HolderLink;
use crate::worktrees::Done;

/// A step running in a pane.
#[derive(Debug)]
pub(crate) struct Ship {
    client: u64,
    worktree: u64,
    command: String,
    exit: Option<u32>,
    link: Option<HolderLink>,
}

/// The command as the user would type it.
fn line(argv: &[String]) -> String {
    argv.iter()
        .map(|word| {
            if word.is_empty()
                || word.contains(|ch: char| ch.is_whitespace() || "'\"$`\\".contains(ch))
            {
                format!("'{}'", word.replace('\'', "'\\''"))
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Finds `program` on `PATH`.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

fn pull_request_argv(
    worktree: &Path,
    title: &str,
    body: &str,
    base: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    if !on_path("gh") {
        anyhow::bail!(
            "opening a pull request needs the GitHub CLI: install gh (https://cli.github.com), then run gh auth login"
        );
    }
    let signed_in = std::process::Command::new("gh")
        .args(["auth", "status"])
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !signed_in {
        anyhow::bail!("gh is not signed in: run gh auth login in a pane, then try again");
    }
    let branch = git::ship::branch(worktree)?;
    let mut argv: Vec<String> = [
        "gh", "pr", "create", "--head", &branch, "--title", title, "--body", body,
    ]
    .map(str::to_owned)
    .to_vec();
    if let Some(base) = base {
        argv.extend(["--base".to_owned(), base.to_owned()]);
    }
    Ok(argv)
}

impl Daemon {
    fn ship_path(&self, client: u64, worktree: u64) -> Option<PathBuf> {
        match self.worktrees.get(&worktree) {
            Some(entry) => Some(PathBuf::from(&entry.path)),
            None => {
                self.refuse(client, format!("no worktree {worktree}"));
                None
            }
        }
    }

    pub(crate) fn commit(&mut self, client: u64, worktree: u64, message: String) {
        if message.trim().is_empty() {
            self.refuse(client, "a commit needs a message".to_owned());
            return;
        }
        let argv = ["git", "commit", "--message", &message]
            .map(str::to_owned)
            .to_vec();
        self.ship_ready(client, worktree, Ok(argv));
    }

    pub(crate) fn push(&mut self, client: u64, worktree: u64) {
        let Some(path) = self.ship_path(client, worktree) else {
            return;
        };
        self.job(move || Done::ShipReady {
            client,
            worktree,
            argv: git::ship::push_argv(&path).map_err(|error| format!("{error:#}")),
        });
    }

    pub(crate) fn open_pull_request(
        &mut self,
        client: u64,
        worktree: u64,
        title: String,
        body: String,
    ) {
        if title.trim().is_empty() {
            self.refuse(client, "a pull request needs a title".to_owned());
            return;
        }
        let Some(path) = self.ship_path(client, worktree) else {
            return;
        };
        let base = self
            .worktrees
            .get(&worktree)
            .and_then(|entry| self.projects.get(&entry.project))
            .and_then(|project| project.base_branch.clone());
        self.job(move || Done::ShipReady {
            client,
            worktree,
            argv: pull_request_argv(&path, &title, &body, base.as_deref())
                .map_err(|error| format!("{error:#}")),
        });
    }

    /// Starts a step's command in a new tab of the worktree's workspace.
    pub(crate) fn ship_ready(
        &mut self,
        client: u64,
        worktree: u64,
        argv: Result<Vec<String>, String>,
    ) {
        let argv = match argv {
            Ok(argv) => argv,
            Err(error) => {
                self.reply(
                    client,
                    FromDaemon::Shipped(ShipReport {
                        worktree,
                        command: String::new(),
                        output: String::new(),
                        result: Some(Err(error)),
                        pane: None,
                    }),
                );
                return;
            }
        };
        let workspace = match self.worktree_workspace(worktree) {
            Ok(workspace) => workspace,
            Err(error) => {
                self.refuse(client, format!("{error:#}"));
                return;
            }
        };
        let Some(cwd) = self
            .worktrees
            .get(&worktree)
            .map(|entry| entry.path.clone())
        else {
            return;
        };
        let command = line(&argv);
        let name = argv.get(1).cloned().unwrap_or_default();
        let record = Pane {
            agent: crate::agents::PaneAgent::default(),
            link: None,
            cols: 80,
            rows: 24,
            state: PaneState::Running,
            screen: None,
            launch: Launch::Command(argv),
            spawn_worktree: Some(worktree),
            cwd,
            title: command.clone(),
            history: self.config.history,
            workspace,
            env: Vec::new(),
        };
        let tab_name = match name.as_str() {
            "pr" => "pull request".to_owned(),
            other => other.to_owned(),
        };
        match self.command_tab(workspace, &tab_name, record) {
            Ok(pane) => {
                self.ships.insert(
                    pane,
                    Ship {
                        client,
                        worktree,
                        command: command.clone(),
                        exit: None,
                        link: None,
                    },
                );
                self.reply(
                    client,
                    FromDaemon::Shipped(ShipReport {
                        worktree,
                        command,
                        output: String::new(),
                        result: None,
                        pane: Some(pane.raw()),
                    }),
                );
            }
            Err(error) => self.refuse(client, error),
        }
    }

    /// A step's pane exited: ask for its last lines, then report. Returns
    /// the link back when the pane is no step's.
    pub(crate) fn ship_exited(
        &mut self,
        pane: PaneId,
        exit: Option<u32>,
        link: Option<HolderLink>,
    ) -> Option<HolderLink> {
        let Some(ship) = self.ships.get_mut(&pane) else {
            return link;
        };
        ship.exit = exit;
        match link {
            Some(link) if link.has_groups() => {
                link.send(ToHolder::Tail {
                    lines: tuning::CHECK_TAIL_LINES,
                });
                ship.link = Some(link);
            }
            link => {
                if let Some(link) = link {
                    link.send(ToHolder::AckExit);
                }
                self.ship_tail(pane, String::new());
            }
        }
        None
    }

    /// Whether a holder's `Tail` is a step's.
    pub(crate) fn ships(&self, pane: PaneId) -> bool {
        self.ships.contains_key(&pane)
    }

    pub(crate) fn ship_tail(&mut self, pane: PaneId, tail: String) {
        let Some(ship) = self.ships.remove(&pane) else {
            return;
        };
        if let Some(link) = ship.link {
            link.send(ToHolder::AckExit);
        }
        let result = match ship.exit {
            Some(0) => Ok(()),
            Some(code) => Err(format!("exited {code}")),
            None => Err("ended by a signal".to_owned()),
        };
        self.reply(
            ship.client,
            FromDaemon::Shipped(ShipReport {
                worktree: ship.worktree,
                command: ship.command,
                output: tail,
                result: Some(result),
                pane: Some(pane.raw()),
            }),
        );
    }

    /// A step's pane closed before it exited.
    pub(crate) fn ship_gone(&mut self, pane: PaneId) {
        if self
            .ships
            .get(&pane)
            .is_some_and(|ship| ship.exit.is_none())
        {
            self.ship_tail(pane, String::new());
        }
    }
}
