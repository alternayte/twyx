//! Checks: each run is a pane without a leaf in its worktree's directory,
//! with the source key taken before the start and after the exit
//! (Invariant 11). A repository check file runs only while its exact
//! content is trusted (Invariant 13).

use std::path::Path;

use twyx_core::check::{self, Check, Outcome};
use twyx_core::inbox::Reason;
use twyx_core::pane::PaneId;
use twyx_core::tuning;
use twyx_protocol::client::{
    CheckFile, CheckInfo, CheckRun, ChecksInfo, FromDaemon, Launch, PaneState,
};
use twyx_protocol::holder::ToHolder;

use crate::agents::now_ms;
use crate::daemon::{Daemon, Pane};
use crate::git;
use crate::holders::HolderLink;
use crate::worktrees::Done;

/// A run twyx follows until its result is written.
#[derive(Debug)]
pub(crate) struct Running {
    row: u64,
    run: CheckRun,
    deadline_ms: Option<u64>,
    /// Why twyx stopped it, if it did.
    stopped: Option<Outcome>,
    exited: bool,
    /// The holder, kept after the exit until it sends the output's tail.
    link: Option<HolderLink>,
}

/// The repository check file as read once.
struct File {
    info: CheckFile,
    checks: Vec<Check>,
}

fn read_file(worktree: &Path) -> Option<File> {
    let path = worktree.join(check::FILE);
    let bytes = std::fs::read(&path).ok()?;
    let hash = crate::recipes::hash(&bytes);
    let parsed = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{}: not UTF-8", path.display()))
        .and_then(|text| check::parse(&path, text).map_err(|error| error.to_string()));
    let (checks, error) = match parsed {
        Ok(checks) => (checks, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    Some(File {
        info: CheckFile {
            path: check::FILE.to_owned(),
            hash,
            trusted: false,
            error,
        },
        checks,
    })
}

impl Daemon {
    fn check_file(&self, worktree: u64) -> Option<File> {
        let entry = self.worktrees.get(&worktree)?;
        let mut file = read_file(Path::new(&entry.path))?;
        file.info.trusted = self
            .store
            .trusted(entry.project, check::FILE, &file.info.hash)
            .unwrap_or(false);
        Some(file)
    }

    /// The user's checks, then the repository's; a repository check with a
    /// user check's name is left out.
    fn all_checks(&self, file: Option<&File>) -> Vec<(Check, bool)> {
        let mut checks: Vec<(Check, bool)> = self
            .config
            .checks
            .iter()
            .map(|check| (check.clone(), false))
            .collect();
        for check in file.map(|file| file.checks.as_slice()).unwrap_or_default() {
            if !checks.iter().any(|(seen, _)| seen.name == check.name) {
                checks.push((check.clone(), true));
            }
        }
        checks
    }

    pub(crate) fn checks(&self, client: u64, worktree: u64) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let path = std::path::PathBuf::from(&entry.path);
        let file = self.check_file(worktree);
        let mut runs = self.store.last_checks(worktree).unwrap_or_default();
        for run in &mut runs {
            if let Some(running) = self
                .check_runs
                .values()
                .find(|running| running.run.worktree == worktree && running.run.name == run.name)
            {
                *run = running.run.clone();
            }
        }
        let checks: Vec<CheckInfo> = self
            .all_checks(file.as_ref())
            .into_iter()
            .map(|(check, repository)| CheckInfo {
                run: runs.iter().find(|run| run.name == check.name).cloned(),
                check,
                repository,
            })
            .collect();
        let file = file.map(|file| file.info);
        self.job(move || {
            let (key, error) = match git::checks::source_key(&path) {
                Ok(key) => (Some(key), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            let checks = checks
                .into_iter()
                .map(|mut info| {
                    if let (Some(run), Some(key)) = (&mut info.run, &key) {
                        run.stale = run.outcome.is_some()
                            && run.key_after.as_ref().unwrap_or(&run.key_before) != key;
                    }
                    info
                })
                .collect();
            Done::Checks {
                client,
                info: ChecksInfo {
                    worktree,
                    key,
                    checks,
                    file,
                    error,
                },
            }
        });
    }

    pub(crate) fn trust_checks(&mut self, client: u64, worktree: u64, shown: String) {
        let Some(project) = self.worktrees.get(&worktree).map(|entry| entry.project) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        match self.check_file(worktree) {
            Some(file) if file.info.hash == shown => {
                if let Err(error) = self.store.trust(project, check::FILE, &shown) {
                    self.refuse(client, format!("{error:#}"));
                    return;
                }
                self.checks(client, worktree);
            }
            Some(_) => self.refuse(
                client,
                format!(
                    "{} changed since it was shown; look at it again",
                    check::FILE
                ),
            ),
            None => self.refuse(client, format!("the worktree has no {}", check::FILE)),
        }
    }

    fn check_running(&self, worktree: u64, name: &str) -> Option<PaneId> {
        self.check_runs
            .iter()
            .find(|(_, running)| running.run.worktree == worktree && running.run.name == name)
            .map(|(pane, _)| *pane)
    }

    /// Takes the source key off the daemon's thread, then starts the pane.
    pub(crate) fn run_check(&mut self, client: u64, worktree: u64, name: String) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let path = std::path::PathBuf::from(&entry.path);
        let file = self.check_file(worktree);
        let found = self
            .all_checks(file.as_ref())
            .into_iter()
            .find(|(check, _)| check.name == name);
        let check = match found {
            None => {
                self.refuse(client, format!("no check named {name:?} in this worktree"));
                return;
            }
            Some((_, true)) if !file.as_ref().is_some_and(|file| file.info.trusted) => {
                self.refuse(
                    client,
                    format!(
                        "{} is not trusted; look at it with `twyx check list` and trust it first",
                        check::FILE
                    ),
                );
                return;
            }
            Some((check, _)) => check,
        };
        if self.check_running(worktree, &name).is_some()
            || !self.checks_starting.insert((worktree, name))
        {
            self.refuse(client, format!("check {:?} is already running", check.name));
            return;
        }
        self.job(move || {
            let key = git::checks::source_key(&path).map_err(|error| format!("{error:#}"));
            Done::CheckKeyed {
                client,
                worktree,
                check,
                key,
            }
        });
    }

    pub(crate) fn check_keyed(
        &mut self,
        client: u64,
        worktree: u64,
        check: Check,
        key: Result<String, String>,
    ) {
        self.checks_starting.remove(&(worktree, check.name.clone()));
        let key = match key {
            Ok(key) => key,
            Err(error) => {
                self.refuse(client, format!("could not take the source key: {error}"));
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
        // The last run's pane goes unless the user shows it.
        let last = self
            .store
            .last_checks(worktree)
            .unwrap_or_default()
            .into_iter()
            .find(|run| run.name == check.name)
            .and_then(|run| run.pane)
            .map(PaneId::from_raw);
        if let Some(last) = last
            && self.leaf_of(last).is_none()
            && self.panes.contains_key(&last)
        {
            self.end_pane(last);
        }
        let record = Pane {
            agent: crate::agents::PaneAgent::default(),
            link: None,
            cols: 80,
            rows: 24,
            state: PaneState::Running,
            screen: None,
            launch: Launch::Command(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                check.command.clone(),
            ]),
            spawn_worktree: Some(worktree),
            cwd,
            title: format!("check {}", check.name),
            history: self.config.history,
            workspace,
            env: Vec::new(),
        };
        let pane = match self.reserve_pane(&record) {
            Ok(pane) => pane,
            Err(error) => {
                self.refuse(client, format!("{error:#}"));
                return;
            }
        };
        let started_ms = now_ms();
        let run = CheckRun {
            worktree,
            name: check.name.clone(),
            command: check.command.clone(),
            pane: Some(pane.raw()),
            outcome: None,
            exit: None,
            key_before: key,
            key_after: None,
            tail: String::new(),
            started_ms,
            ended_ms: None,
            stale: false,
        };
        let timeout_ms = check
            .timeout
            .map(|timeout| u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        let row = match self.store.start_check(&run, timeout_ms) {
            Ok(row) => row,
            Err(error) => {
                self.refuse(client, format!("{error:#}"));
                return;
            }
        };
        if let Err(error) = self.start_pane(pane, record, None) {
            let mut run = run;
            run.outcome = Some(Outcome::Cancelled);
            run.tail = format!("{error:#}");
            run.ended_ms = Some(now_ms());
            let _ = self.store.end_check(row, &run);
            self.refuse(client, format!("{error:#}"));
            return;
        }
        self.check_runs.insert(
            pane,
            Running {
                row,
                run,
                deadline_ms: timeout_ms.map(|timeout| started_ms.saturating_add(timeout)),
                stopped: None,
                exited: false,
                link: None,
            },
        );
        self.reply(
            client,
            FromDaemon::CheckStarted {
                worktree,
                name: check.name,
                pane: pane.raw(),
            },
        );
    }

    pub(crate) fn cancel_check(&mut self, client: u64, worktree: u64, name: &str) {
        match self.check_running(worktree, name) {
            Some(pane) => self.stop_check(pane, Outcome::Cancelled),
            None => self.refuse(client, format!("check {name:?} is not running")),
        }
    }

    /// Kills the run's whole process group; the exit then ends the run.
    fn stop_check(&mut self, pane: PaneId, why: Outcome) {
        let Some(running) = self.check_runs.get_mut(&pane) else {
            return;
        };
        if running.exited || running.stopped.is_some() {
            return;
        }
        running.stopped = Some(why);
        if let Some(link) = self.panes.get(&pane).and_then(|entry| entry.link.as_ref()) {
            link.send(if link.has_groups() {
                ToHolder::KillGroup
            } else {
                ToHolder::Kill
            });
        }
    }

    pub(crate) fn check_deadlines(&mut self) {
        let now = now_ms();
        let due: Vec<PaneId> = self
            .check_runs
            .iter()
            .filter(|(_, running)| running.deadline_ms.is_some_and(|deadline| deadline <= now))
            .map(|(pane, _)| *pane)
            .collect();
        for pane in due {
            self.stop_check(pane, Outcome::TimedOut);
        }
    }

    /// A check's pane exited. Returns the holder link back when the pane is
    /// no check run's, for the caller to release.
    pub(crate) fn check_exited(
        &mut self,
        pane: PaneId,
        exit: Option<u32>,
        link: Option<HolderLink>,
    ) -> Option<HolderLink> {
        let Some(running) = self.check_runs.get_mut(&pane) else {
            return link;
        };
        running.exited = true;
        running.run.exit = exit;
        match link {
            Some(link) if link.has_groups() => {
                link.send(ToHolder::Tail {
                    lines: tuning::CHECK_TAIL_LINES,
                });
                running.link = Some(link);
                None
            }
            link => {
                if let Some(link) = link {
                    link.send(ToHolder::AckExit);
                }
                self.key_after(pane);
                None
            }
        }
    }

    pub(crate) fn check_tail(&mut self, pane: PaneId, tail: String) {
        let Some(running) = self.check_runs.get_mut(&pane) else {
            return;
        };
        running.run.tail = tail;
        if let Some(link) = running.link.take() {
            link.send(ToHolder::AckExit);
        }
        self.key_after(pane);
    }

    fn key_after(&mut self, pane: PaneId) {
        let Some(running) = self.check_runs.get(&pane) else {
            return;
        };
        if running.stopped.is_some() {
            self.end_check(pane, None);
            return;
        }
        let Some(path) = self
            .worktrees
            .get(&running.run.worktree)
            .map(|entry| std::path::PathBuf::from(&entry.path))
        else {
            self.end_check(pane, None);
            return;
        };
        self.job(move || Done::CheckEnded {
            pane,
            key: git::checks::source_key(&path).map_err(|error| format!("{error:#}")),
        });
    }

    /// Writes the result and raises its inbox item.
    pub(crate) fn end_check(&mut self, pane: PaneId, key: Option<Result<String, String>>) {
        let Some(mut running) = self.check_runs.remove(&pane) else {
            return;
        };
        let run = &mut running.run;
        let after = match key {
            Some(Ok(key)) => Some(key),
            Some(Err(error)) => {
                run.tail
                    .push_str(&format!("\ntwyx: no source key after the run: {error}"));
                None
            }
            None => None,
        };
        run.outcome = Some(match &after {
            Some(after) => Outcome::of(running.stopped, &run.key_before, after, run.exit),
            None => running.stopped.unwrap_or(Outcome::Inconclusive),
        });
        run.key_after = after;
        run.ended_ms = Some(now_ms());
        if let Err(error) = self.store.end_check(running.row, run) {
            eprintln!("twyx daemon: {error:#}");
        }
        let outcome = run.outcome.map_or("", Outcome::name);
        if let Some(entry) = self.panes.get_mut(&pane) {
            entry.title = format!("check {}: {outcome}", run.name);
            self.broadcast_pane(pane);
            self.raise(pane, Reason::CheckEnded);
        } else {
            running.run.pane = None;
        }
        self.broadcast(FromDaemon::CheckEnded(running.run));
    }

    /// The run's pane went before its exit: closed by the user, or its
    /// holder lost.
    pub(crate) fn check_gone(&mut self, pane: PaneId) {
        let Some(running) = self.check_runs.get_mut(&pane) else {
            return;
        };
        if running.exited {
            return;
        }
        running.stopped.get_or_insert(Outcome::Cancelled);
        running.exited = true;
        self.end_check(pane, None);
    }

    /// Follows the runs a previous daemon left running. A run whose pane
    /// did not survive ends as cancelled.
    pub(crate) fn restore_checks(&mut self) {
        let rows = match self.store.running_checks() {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("twyx daemon: {error:#}");
                return;
            }
        };
        for (row, run, timeout_ms) in rows {
            let pane = run.pane.map(PaneId::from_raw);
            let live = pane.filter(|pane| {
                self.panes
                    .get(pane)
                    .is_some_and(|entry| entry.state == PaneState::Running)
            });
            let deadline_ms = timeout_ms.map(|timeout| run.started_ms.saturating_add(timeout));
            let mut running = Running {
                row,
                run,
                deadline_ms,
                stopped: None,
                exited: false,
                link: None,
            };
            match live {
                Some(pane) => {
                    self.check_runs.insert(pane, running);
                }
                None => {
                    running.run.outcome = Some(Outcome::Cancelled);
                    running.run.tail = "twyx: the run's pane did not survive".to_owned();
                    running.run.ended_ms = Some(now_ms());
                    if let Err(error) = self.store.end_check(row, &running.run) {
                        eprintln!("twyx daemon: {error:#}");
                    }
                }
            }
        }
    }
}
