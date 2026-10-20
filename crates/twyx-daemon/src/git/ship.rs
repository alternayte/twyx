//! Shipping from the review: staging on the main index, and the commands
//! that commit and push. Staging is the user's own action on the main
//! index, so it runs as a plain Git command, not under Invariant 10.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use twyx_core::diff::{self, FileDiff, Hunk};

use super::{GitError, background, command_line, git, run};

/// The paths with staged changes, and those with unstaged changes or new
/// and not ignored.
pub(crate) fn index_paths(worktree: &Path) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let list = |args: &[&str]| -> Result<BTreeSet<String>> {
        Ok(run(background(worktree, args))?
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    };
    let staged = list(&["diff", "--cached", "--name-only", "-z", "--no-renames"])?;
    let mut unstaged = list(&["diff", "--name-only", "-z", "--no-renames"])?;
    unstaged.extend(list(&["ls-files", "--others", "--exclude-standard", "-z"])?);
    Ok((staged, unstaged))
}

/// What a step ran and printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub command: String,
    pub output: String,
}

/// Runs a user's Git command and keeps its line and everything it printed.
fn shown(mut command: std::process::Command, input: Option<&str>) -> Result<Shown> {
    let line = command_line(&command);
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("run {line}; is Git installed?"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input.as_bytes())
            .with_context(|| format!("write the patch to {line}"))?;
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("run {line}"))?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(GitError {
            command: line,
            stderr: printed,
        }
        .into());
    }
    Ok(Shown {
        command: line,
        output: printed.trim_end().to_owned(),
    })
}

fn zero_diff(worktree: &Path, cached: bool, path: &str) -> Result<Option<FileDiff>> {
    let mut args = vec![
        "diff",
        "-U0",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
    ];
    if cached {
        args.push("--cached");
    }
    args.extend(["--", path]);
    Ok(diff::parse(&run(background(worktree, &args))?)
        .into_iter()
        .find(|file| file.path == path))
}

fn apply(worktree: &Path, patch: &str, reverse: bool) -> Result<Shown> {
    let mut args = vec!["apply", "--cached", "--unidiff-zero"];
    if reverse {
        args.push("-R");
    }
    args.push("-");
    let command = git(worktree, &args);
    shown(command, Some(patch))
}

fn untracked(worktree: &Path, path: &str) -> Result<bool> {
    Ok(!run(background(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "--", path],
    ))?
    .trim()
    .is_empty())
}

/// Stages or unstages the index changes under review hunks: the `-U0`
/// hunks whose changed lines meet a review hunk's in the working tree.
pub(crate) fn stage_hunks(
    worktree: &Path,
    path: &str,
    review: &[Hunk],
    unstage: bool,
) -> Result<Shown> {
    let spans: Vec<(u64, u64)> = review.iter().map(|hunk| hunk.span(true)).collect();
    let meets = |span: (u64, u64)| spans.iter().any(|each| diff::overlaps(*each, span));
    let unstaged = zero_diff(worktree, false, path)?;
    let unstaged_hunks = unstaged
        .as_ref()
        .map_or(&[][..], |file| file.hunks.as_slice());
    if unstage {
        let Some(staged) = zero_diff(worktree, true, path)? else {
            bail!("{path} has nothing staged");
        };
        // Staged lines are in the index; carry them to the working tree.
        let ids: Vec<u64> = staged
            .hunks
            .iter()
            .filter(|hunk| meets(diff::carry_span(hunk.span(true), unstaged_hunks)))
            .map(Hunk::id)
            .collect();
        if ids.is_empty() {
            bail!("nothing staged in this hunk of {path}");
        }
        return apply(worktree, &staged.patch(Some(&ids)), true);
    }
    if unstaged.is_none() && untracked(worktree, path)? {
        return shown(git(worktree, &["add", "--", path]), None);
    }
    let Some(unstaged) = unstaged else {
        bail!("nothing to stage in {path}: it is committed or staged");
    };
    let ids: Vec<u64> = unstaged
        .hunks
        .iter()
        .filter(|hunk| meets(hunk.span(true)))
        .map(Hunk::id)
        .collect();
    if ids.is_empty() {
        bail!("nothing to stage in this hunk of {path}: it is committed or staged");
    }
    apply(worktree, &unstaged.patch(Some(&ids)), false)
}

/// Stages or unstages whole paths, or everything without paths.
pub(crate) fn stage_paths(worktree: &Path, paths: &[String], unstage: bool) -> Result<Shown> {
    let mut args: Vec<&str> = if unstage {
        vec!["reset", "--quiet", "--"]
    } else {
        vec!["add", "--all", "--"]
    };
    if paths.is_empty() {
        args.push(".");
    }
    args.extend(paths.iter().map(String::as_str));
    shown(git(worktree, &args), None)
}

/// The command that pushes the worktree's branch: plain `git push` with an
/// upstream, else one that sets it on the branch's remote, else on
/// `origin` or the only remote.
pub(crate) fn push_argv(worktree: &Path) -> Result<Vec<String>> {
    let branch = run(background(
        worktree,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    ))
    .map(|branch| branch.trim().to_owned())
    .map_err(|_| anyhow::anyhow!("HEAD is detached; check out a branch to push it"))?;
    let upstream = run(background(
        worktree,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    ));
    if upstream.is_ok() {
        return Ok(vec!["git".to_owned(), "push".to_owned()]);
    }
    let configured = run(background(
        worktree,
        &["config", "--get", &format!("branch.{branch}.remote")],
    ))
    .ok()
    .map(|remote| remote.trim().to_owned())
    .filter(|remote| !remote.is_empty());
    let remotes: Vec<String> = run(background(worktree, &["remote"]))?
        .lines()
        .map(str::to_owned)
        .collect();
    let remote = configured
        .or_else(|| remotes.iter().find(|remote| *remote == "origin").cloned())
        .or_else(|| (remotes.len() == 1).then(|| remotes[0].clone()));
    let Some(remote) = remote else {
        bail!(match remotes.len() {
            0 => "the repository has no remote; add one with git remote add".to_owned(),
            _ => format!(
                "no upstream and no origin; push once from a shell to one of: {}",
                remotes.join(", ")
            ),
        });
    };
    Ok(["git", "push", "--set-upstream", "--", &remote, &branch]
        .map(str::to_owned)
        .to_vec())
}

/// The worktree's branch, for a pull request's head.
pub(crate) fn branch(worktree: &Path) -> Result<String> {
    run(background(
        worktree,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    ))
    .map(|branch| branch.trim().to_owned())
    .map_err(|_| anyhow::anyhow!("HEAD is detached; check out a branch first"))
}
