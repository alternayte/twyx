//! Every Git call twyx makes, as the Git CLI in a subprocess.
//!
//! Background calls (discovery, listing, status, the removal prompt) keep
//! Invariant 10: no optional locks, so the main index is never written, and
//! no pager, external diff, textconv driver or fsmonitor hook. User actions
//! (adding and removing a worktree) run Git as the user would.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub(crate) mod checks;
pub(crate) mod review;
pub(crate) mod ship;

/// Variables a daemon may inherit that would point Git at another
/// repository or index.
const INHERITED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_EXTERNAL_DIFF",
];

/// A Git command that failed, with the command line and its stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitError {
    pub command: String,
    pub stderr: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed", self.command)?;
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            write!(f, ": {stderr}")?;
        }
        Ok(())
    }
}

impl std::error::Error for GitError {}

fn git(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in INHERITED {
        command.env_remove(name);
    }
    command
}

/// A Git command under Invariant 10.
fn background(dir: &Path, args: &[&str]) -> Command {
    let mut full = vec![
        "--no-pager",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.pager=cat",
        "-c",
        "color.ui=false",
        "-c",
        "log.showSignature=false",
    ];
    full.extend_from_slice(args);
    let mut command = git(dir, &full);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat");
    command
}

fn run(mut command: Command) -> Result<String> {
    let line = command_line(&command);
    let output = command
        .output()
        .with_context(|| format!("run {line}; is Git installed?"))?;
    if !output.status.success() {
        return Err(GitError {
            command: line,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
        .into());
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("{line} printed text that is not UTF-8"))
}

/// The command as the user would type it, without the Invariant 10 options.
fn command_line(command: &Command) -> String {
    let mut words = vec!["git".to_owned()];
    let mut skip = false;
    for arg in command.get_args() {
        let arg = arg.to_string_lossy();
        if skip {
            skip = false;
            continue;
        }
        match arg.as_ref() {
            "-c" => skip = true,
            "--no-pager" => {}
            _ => words.push(arg.into_owned()),
        }
    }
    words.join(" ")
}

/// What a path is to Git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// A path inside a worktree of a repository.
    Repository {
        /// The common Git dir, which names the project.
        common_dir: PathBuf,
        /// The top of the worktree the path is in.
        toplevel: PathBuf,
    },
    /// A directory outside any repository.
    Directory(PathBuf),
}

/// Finds the repository a path belongs to with `git rev-parse`.
pub(crate) fn discover(path: &Path) -> Result<Found> {
    let path = path
        .canonicalize()
        .with_context(|| format!("open {}", path.display()))?;
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }
    let output = background(
        &path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--show-toplevel",
        ],
    )
    .output()
    .context("run git rev-parse; is Git installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if stderr.contains("not a git repository") {
            return Ok(Found::Directory(path));
        }
        return Err(GitError {
            command: "git rev-parse --git-common-dir --show-toplevel".to_owned(),
            stderr,
        }
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let (Some(common_dir), Some(toplevel)) = (lines.next(), lines.next()) else {
        anyhow::bail!("{} is inside a Git dir, not a worktree", path.display());
    };
    Ok(Found::Repository {
        common_dir: canonical(Path::new(common_dir)),
        toplevel: canonical(Path::new(toplevel)),
    })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// One entry of `git worktree list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeEntry {
    pub path: PathBuf,
    /// The Git admin name, and `main` for the main worktree.
    pub admin: String,
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub main: bool,
    /// The directory is gone; Git can prune the record.
    pub prunable: bool,
}

/// Lists the worktrees of the repository `dir` is in, the main one first.
pub(crate) fn worktrees(dir: &Path) -> Result<Vec<WorktreeEntry>> {
    let out = run(background(dir, &["worktree", "list", "--porcelain", "-z"]))?;
    Ok(parse_worktrees(&out))
}

fn parse_worktrees(out: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    // Records are separated by an empty field. The first is the main
    // worktree, and a bare main repository has no worktree to list.
    for (index, record) in out.split("\0\0").enumerate() {
        let mut entry = WorktreeEntry {
            path: PathBuf::new(),
            admin: String::new(),
            branch: None,
            main: index == 0,
            prunable: false,
        };
        let mut bare = false;
        for field in record.split('\0') {
            let (key, value) = field.split_once(' ').unwrap_or((field, ""));
            match key {
                "worktree" => entry.path = PathBuf::from(value),
                "branch" => {
                    entry.branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_owned(),
                    );
                }
                "prunable" => entry.prunable = true,
                "bare" => bare = true,
                _ => {}
            }
        }
        if bare || entry.path.as_os_str().is_empty() {
            continue;
        }
        entry.admin = if entry.main {
            "main".to_owned()
        } else {
            admin_name(&entry.path)
        };
        entries.push(entry);
    }
    entries
}

/// A linked worktree's admin name: the last part of the Git dir its `.git`
/// file points at, else the directory's own name.
fn admin_name(path: &Path) -> String {
    std::fs::read_to_string(path.join(".git"))
        .ok()
        .and_then(|text| {
            let gitdir = text.trim().strip_prefix("gitdir:")?.trim().to_owned();
            Path::new(&gitdir)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// One changed path from `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    /// The two-letter status, `??` for an untracked file.
    pub code: String,
    pub path: String,
}

/// A worktree's branch and dirty state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Status {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: Vec<Change>,
}

/// `git status --porcelain=v2 --branch` in a worktree.
pub(crate) fn status(worktree: &Path) -> Result<Status> {
    let out = run(background(
        worktree,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ],
    ))?;
    Ok(parse_status(&out))
}

fn parse_status(out: &str) -> Status {
    let mut status = Status::default();
    let mut fields = out.split('\0');
    while let Some(field) = fields.next() {
        if let Some(header) = field.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.head" if value != "(detached)" => status.branch = Some(value.to_owned()),
                "branch.upstream" => status.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    for count in value.split(' ') {
                        if let Some(ahead) = count.strip_prefix('+') {
                            status.ahead = ahead.parse().unwrap_or(0);
                        } else if let Some(behind) = count.strip_prefix('-') {
                            status.behind = behind.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let mut parts = field.splitn(2, ' ');
        let (Some(kind), Some(rest)) = (parts.next(), parts.next()) else {
            continue;
        };
        // Ordinary entries have 8 fields before the path, renames 9 and then
        // the original path as its own field, unmerged entries 10.
        let (code, path) = match kind {
            "1" => (&rest[..2.min(rest.len())], rest.splitn(8, ' ').nth(7)),
            "2" => {
                let entry = (&rest[..2.min(rest.len())], rest.splitn(9, ' ').nth(8));
                fields.next();
                entry
            }
            "u" => (&rest[..2.min(rest.len())], rest.splitn(10, ' ').nth(9)),
            "?" => ("??", Some(rest)),
            _ => continue,
        };
        if let Some(path) = path {
            status.changes.push(Change {
                code: code.to_owned(),
                path: path.to_owned(),
            });
        }
    }
    status
}

/// One commit, for a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Commit {
    pub oid: String,
    pub subject: String,
}

/// The newest `limit` commits of a worktree's HEAD that no remote-tracking
/// branch has, and how many there are in all. Without a remote that is the
/// whole history, so the list is cut.
pub(crate) fn unpushed(worktree: &Path, limit: usize) -> Result<(Vec<Commit>, u32)> {
    let total = run(background(
        worktree,
        &["rev-list", "--count", "HEAD", "--not", "--remotes", "--"],
    ))?;
    let total = total.trim().parse().unwrap_or(0);
    let limit = format!("--max-count={limit}");
    Ok((
        log(worktree, &[&limit, "HEAD", "--not", "--remotes"])?,
        total,
    ))
}

/// The commits of `head` that the base does not have.
pub(crate) fn unmerged(dir: &Path, base: &str, head: &str) -> Result<Vec<Commit>> {
    log(dir, &[&format!("{base}..{head}")])
}

fn log(dir: &Path, revisions: &[&str]) -> Result<Vec<Commit>> {
    let mut args = vec!["log", "--format=%H %s"];
    args.extend_from_slice(revisions);
    args.push("--");
    let out = run(background(dir, &args))?;
    Ok(out
        .lines()
        .map(|line| {
            let (oid, subject) = line.split_once(' ').unwrap_or((line, ""));
            Commit {
                oid: oid.to_owned(),
                subject: subject.to_owned(),
            }
        })
        .collect())
}

/// The full ref of the base branch: the override, else the remote's default
/// branch, else `main`, else `master`. `None` when none exists.
pub(crate) fn base_branch(dir: &Path, wanted: Option<&str>) -> Option<String> {
    let exists = |reference: &str| {
        background(dir, &["rev-parse", "--verify", "--quiet", reference])
            .output()
            .is_ok_and(|output| output.status.success())
    };
    if let Some(wanted) = wanted {
        return [
            format!("refs/heads/{wanted}"),
            format!("refs/remotes/{wanted}"),
            format!("refs/remotes/origin/{wanted}"),
        ]
        .into_iter()
        .find(|reference| exists(reference));
    }
    if let Ok(remote) = run(background(
        dir,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )) {
        let remote = remote.trim();
        if !remote.is_empty() && exists(remote) {
            return Some(remote.to_owned());
        }
    }
    ["refs/heads/main", "refs/heads/master"]
        .into_iter()
        .find(|reference| exists(reference))
        .map(str::to_owned)
}

/// Whether a local branch exists.
pub(crate) fn has_branch(dir: &Path, branch: &str) -> bool {
    background(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .output()
    .is_ok_and(|output| output.status.success())
}

/// Refuses a name Git would not take as a branch, such as one that starts
/// with a dash.
pub(crate) fn check_branch_name(dir: &Path, branch: &str) -> Result<()> {
    run(background(dir, &["check-ref-format", "--branch", branch])).map(|_| ())
}

/// Adds a worktree at `path`. An existing branch is checked out; a new one
/// starts at `start`.
pub(crate) fn add_worktree(dir: &Path, path: &Path, branch: &str, start: &str) -> Result<()> {
    let path = path.to_string_lossy();
    if has_branch(dir, branch) {
        run(git(dir, &["worktree", "add", &path, branch]))?;
    } else {
        run(git(dir, &["worktree", "add", "-b", branch, &path, start]))?;
    }
    Ok(())
}

/// The paths in a worktree that match the gitignore-style patterns of
/// `patterns` and that Git ignores, whole directories as one entry.
pub(crate) fn ignored_matching(worktree: &Path, patterns: &Path) -> Result<Vec<String>> {
    let exclude = format!("--exclude-from={}", patterns.display());
    let candidates = run(background(
        worktree,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--directory",
            &exclude,
        ],
    ))?;
    let candidates: Vec<&str> = candidates
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = background(worktree, &["check-ignore", "-z", "--stdin"]);
    command.stdin(Stdio::piped());
    let line = command_line(&command);
    let mut child = command
        .spawn()
        .with_context(|| format!("run {line}; is Git installed?"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        for path in &candidates {
            // A collapsed directory ends in a slash, which check-ignore reads
            // as the directory itself.
            stdin
                .write_all(path.trim_end_matches('/').as_bytes())
                .and_then(|()| stdin.write_all(b"\0"))
                .with_context(|| format!("write to {line}"))?;
        }
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("run {line}"))?;
    // Exit 1 means none of them is ignored.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(GitError {
            command: line,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Deletes a local branch: `-d`, or `-D` when `force`.
pub(crate) fn delete_branch(dir: &Path, branch: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    run(git(dir, &["branch", flag, "--", branch])).map(|_| ())
}

/// Removes a worktree. `force` discards its changes and untracked files.
pub(crate) fn remove_worktree(dir: &Path, path: &Path, force: bool) -> Result<()> {
    let path = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    run(git(dir, &args)).map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A throwaway repository, made and driven with the real Git CLI.
    struct Repo {
        root: PathBuf,
    }

    impl Repo {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("twyx-git-{}-{name}", std::process::id()))
                .canonicalize_or_create();
            let repo = Self { root };
            repo.git(
                &["init", "--quiet", "--initial-branch=main", "repo"],
                &repo.root,
            );
            repo.git(&["config", "user.email", "t@example.com"], &repo.path());
            repo.git(&["config", "user.name", "t"], &repo.path());
            repo.git(&["config", "commit.gpgsign", "false"], &repo.path());
            std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
            repo.git(&["add", "a.txt"], &repo.path());
            repo.git(&["commit", "--quiet", "-m", "first"], &repo.path());
            repo
        }

        fn path(&self) -> PathBuf {
            self.root.join("repo")
        }

        fn git(&self, args: &[&str], dir: &Path) -> String {
            let output = Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    trait CreateDir {
        fn canonicalize_or_create(self) -> PathBuf;
    }

    impl CreateDir for PathBuf {
        fn canonicalize_or_create(self) -> PathBuf {
            let _ = std::fs::remove_dir_all(&self);
            std::fs::create_dir_all(&self).unwrap();
            self.canonicalize().unwrap()
        }
    }

    #[test]
    fn invariant_10_background_git_never_writes_the_main_index_or_runs_a_pager_or_fsmonitor_hook() {
        let repo = Repo::new("invariant-10");
        let marker = repo.root.join("ran");
        let hook = repo.root.join("hook.sh");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\necho \"$0 $*\" >> {}\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let hook = hook.to_string_lossy().into_owned();
        repo.git(&["config", "core.fsmonitor", &hook], &repo.path());
        repo.git(&["config", "core.pager", &hook], &repo.path());

        // A touched file with the same content leaves the index stat data
        // stale, which a plain `git status` would refresh and write.
        let index = repo.path().join(".git/index");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(repo.path().join("a.txt"), "one\n").unwrap();
        let before = std::fs::read(&index).unwrap();

        let found = status(&repo.path()).unwrap();
        assert!(found.changes.is_empty(), "{found:?}");
        unpushed(&repo.path(), 10).unwrap();
        worktrees(&repo.path()).unwrap();
        discover(&repo.path()).unwrap();

        assert_eq!(
            std::fs::read(&index).unwrap(),
            before,
            "the index was written"
        );
        assert!(
            !marker.exists(),
            "a hook ran: {:?}",
            std::fs::read_to_string(&marker)
        );

        // The same status without twyx's rules writes the index, so the test
        // above can fail.
        repo.git(
            &["-c", "core.fsmonitor=false", "status", "--porcelain"],
            &repo.path(),
        );
        assert_ne!(
            std::fs::read(&index).unwrap(),
            before,
            "the control did not write"
        );
    }

    #[test]
    fn discover_finds_the_common_dir_from_a_linked_worktree_and_a_plain_directory() {
        let repo = Repo::new("discover");
        let linked = repo.root.join("linked");
        repo.git(
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                &linked.to_string_lossy(),
            ],
            &repo.path(),
        );
        std::fs::create_dir_all(linked.join("sub")).unwrap();
        assert_eq!(
            discover(&linked.join("sub")).unwrap(),
            Found::Repository {
                common_dir: repo.path().join(".git"),
                toplevel: linked.clone(),
            }
        );
        let plain = repo.root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(discover(&plain).unwrap(), Found::Directory(plain));

        let listed = worktrees(&linked).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            (
                listed[0].admin.as_str(),
                listed[0].main,
                listed[0].branch.as_deref()
            ),
            ("main", true, Some("main"))
        );
        assert_eq!(
            (
                listed[1].admin.as_str(),
                listed[1].path.clone(),
                listed[1].branch.as_deref()
            ),
            ("linked", linked, Some("feature"))
        );
    }

    #[test]
    fn status_reads_branch_and_every_kind_of_change() {
        let repo = Repo::new("status");
        std::fs::write(repo.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(repo.path().join("with space.txt"), "new\n").unwrap();
        std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
        repo.git(&["add", "b.txt"], &repo.path());
        repo.git(&["mv", "a.txt", "c.txt"], &repo.path());
        let found = status(&repo.path()).unwrap();
        assert_eq!(found.branch.as_deref(), Some("main"));
        let mut changes: Vec<(String, String)> = found
            .changes
            .into_iter()
            .map(|change| (change.code, change.path))
            .collect();
        changes.sort();
        assert_eq!(
            changes,
            vec![
                ("??".to_owned(), "with space.txt".to_owned()),
                ("A.".to_owned(), "b.txt".to_owned()),
                ("RM".to_owned(), "c.txt".to_owned()),
            ]
        );
    }

    #[test]
    fn worktree_add_and_remove_report_unpushed_and_unmerged_commits() {
        let repo = Repo::new("lifecycle");
        let base = base_branch(&repo.path(), None).unwrap();
        assert_eq!(base, "refs/heads/main");
        let path = repo.root.join("repo.worktrees/fix");
        add_worktree(&repo.path(), &path, "fix", &base).unwrap();
        std::fs::write(path.join("fix.txt"), "fix\n").unwrap();
        repo.git(&["add", "fix.txt"], &path);
        repo.git(&["commit", "--quiet", "-m", "fix it"], &path);
        let unmerged = unmerged(&path, &base, "HEAD").unwrap();
        assert_eq!(unmerged.len(), 1);
        assert_eq!(unmerged[0].subject, "fix it");
        let (shown, total) = unpushed(&path, 1).unwrap();
        assert_eq!((shown[0].subject.as_str(), total), ("fix it", 2));

        std::fs::write(path.join("fix.txt"), "dirty\n").unwrap();
        let refused = remove_worktree(&repo.path(), &path, false).unwrap_err();
        let refused = refused.downcast::<GitError>().unwrap();
        assert!(
            refused.command.starts_with("git worktree remove "),
            "{refused}"
        );
        remove_worktree(&repo.path(), &path, true).unwrap();
        assert!(!path.exists());
        assert!(check_branch_name(&repo.path(), "-x").is_err());
    }

    #[test]
    fn include_patterns_pick_only_ignored_paths() {
        let repo = Repo::new("include");
        std::fs::write(
            repo.path().join(".gitignore"),
            "node_modules/\n.env\nbuild/\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.path().join("node_modules/a")).unwrap();
        std::fs::write(repo.path().join("node_modules/a/i.js"), "x").unwrap();
        std::fs::write(repo.path().join(".env"), "K=V").unwrap();
        std::fs::create_dir_all(repo.path().join("build")).unwrap();
        std::fs::write(repo.path().join("build/out"), "x").unwrap();
        std::fs::write(repo.path().join("notes.txt"), "untracked, not ignored").unwrap();
        let patterns = repo.root.join("include");
        std::fs::write(&patterns, "# deps\nnode_modules/\n.env\nnotes.txt\n").unwrap();
        let mut found = ignored_matching(&repo.path(), &patterns).unwrap();
        found.sort();
        assert_eq!(found, vec![".env", "node_modules"]);
        let none = repo.root.join("none");
        std::fs::write(&none, "missing\n").unwrap();
        assert!(ignored_matching(&repo.path(), &none).unwrap().is_empty());
    }

    #[test]
    fn worktree_list_skips_a_bare_main_and_marks_prunable() {
        let out = "worktree /r\0bare\0\0worktree /r/a\0HEAD 1\0detached\0prunable gitdir file points to non-existent location\0\0";
        let listed = parse_worktrees(out);
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].main);
        assert!(listed[0].prunable);
        assert_eq!(listed[0].branch, None);
    }
}
