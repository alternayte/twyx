//! Review state: a private index per worktree that holds what the user has
//! reviewed, and a ref that keeps it.
//!
//! The index starts as the base tree. Marking a hunk reviewed applies it to
//! that index with `git apply --cached`, so the unreviewed changes are the
//! diff from the index to the working tree. After each change the index is
//! written as a parentless commit at `refs/twyx/review/<name>`, whose
//! message records the base; the commit keeps the objects from `git gc`
//! (Invariant 12) and rebuilds a lost index. Every call here keeps
//! Invariant 10: it reads and writes only its own index files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use twyx_core::diff::{self, FileDiff};
use twyx_core::tuning;

use super::{background, run};

const REFS: &str = "refs/twyx/review/";

/// The review of one worktree now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct State {
    /// The base branch's full ref, and the merge-base commit.
    pub base_ref: String,
    pub base: String,
    /// The diff from the review index to the working tree.
    pub unreviewed: Vec<FileDiff>,
    /// New files over the size cap: listed, not reviewable.
    pub too_large: Vec<String>,
    /// Paths with reviewed changes since the base.
    pub reviewed: Vec<String>,
}

struct Files {
    dir: PathBuf,
    index: PathBuf,
    base: PathBuf,
}

fn files(worktree: &Path) -> Result<Files> {
    let git_dir = run(background(worktree, &["rev-parse", "--absolute-git-dir"]))?;
    let dir = PathBuf::from(git_dir.trim()).join("twyx");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(Files {
        index: dir.join("review.index"),
        base: dir.join("review.base"),
        dir,
    })
}

/// Git with `index` as its index.
fn with_index(worktree: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let mut command = background(worktree, args);
    command.env("GIT_INDEX_FILE", index);
    run(command)
}

fn reference(name: &str) -> String {
    format!("{REFS}{name}")
}

/// The merge-base of HEAD and the base branch.
fn base(worktree: &Path, base_branch: Option<&str>) -> Result<(String, String)> {
    let Some(base_ref) = super::base_branch(worktree, base_branch) else {
        bail!("no base branch: set one for the project, or create main or master");
    };
    let base = run(background(worktree, &["merge-base", "HEAD", &base_ref]))?;
    Ok((base_ref, base.trim().to_owned()))
}

/// Makes the review index ready: from the ref when the file is lost, else
/// from the base; and carried onto a base that moved.
fn prepare(
    worktree: &Path,
    name: &str,
    base_branch: Option<&str>,
) -> Result<(Files, String, String)> {
    let files = files(worktree)?;
    let (base_ref, base) = base(worktree, base_branch)?;
    if !files.index.exists() {
        let saved = run(background(
            worktree,
            &["log", "-1", "--format=%B", &reference(name), "--"],
        ));
        let saved_base = saved.ok().and_then(|message| {
            message
                .lines()
                .find_map(|line| line.strip_prefix("base ").map(str::to_owned))
        });
        match saved_base {
            Some(saved_base) => {
                with_index(
                    worktree,
                    &files.index,
                    &["read-tree", &format!("{}^{{tree}}", reference(name))],
                )?;
                write(&files.base, &saved_base)?;
            }
            None => {
                with_index(worktree, &files.index, &["read-tree", &base])?;
                write(&files.base, &base)?;
                save(worktree, &files, name)?;
            }
        }
    }
    let stored = std::fs::read_to_string(&files.base).unwrap_or_default();
    if stored.trim() != base {
        carry(worktree, &files, stored.trim(), &base)?;
        write(&files.base, &base)?;
        save(worktree, &files, name)?;
    }
    Ok((files, base_ref, base))
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

/// Writes the index as a parentless commit at the worktree's ref.
fn save(worktree: &Path, files: &Files, name: &str) -> Result<()> {
    let tree = with_index(worktree, &files.index, &["write-tree"])?;
    let base = std::fs::read_to_string(&files.base).unwrap_or_default();
    let message = format!("twyx review\n\nbase {}\n", base.trim());
    let mut command = background(worktree, &["commit-tree", tree.trim(), "-m", &message]);
    // The repository may have no identity; this commit is twyx's own.
    for (name, value) in [
        ("GIT_AUTHOR_NAME", "twyx"),
        ("GIT_AUTHOR_EMAIL", "twyx@localhost"),
        ("GIT_COMMITTER_NAME", "twyx"),
        ("GIT_COMMITTER_EMAIL", "twyx@localhost"),
    ] {
        command.env(name, value);
    }
    let commit = run(command)?;
    run(background(
        worktree,
        &["update-ref", &reference(name), commit.trim()],
    ))?;
    Ok(())
}

/// Carries the reviewed changes from the old base onto the new one: each
/// reviewed hunk is applied, three-way, onto the new base tree. A hunk that
/// does not apply becomes unreviewed again.
fn carry(worktree: &Path, files: &Files, old: &str, new: &str) -> Result<()> {
    let reviewed_tree = with_index(worktree, &files.index, &["write-tree"])?;
    let delta = run(background(
        worktree,
        &[
            "diff",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            old,
            reviewed_tree.trim(),
        ],
    ))
    .unwrap_or_default();
    let fresh = files.dir.join("review.index.new");
    let scratch = files.dir.join("review.index.try");
    let patch = files.dir.join("review.patch");
    with_index(worktree, &fresh, &["read-tree", new])?;
    for file in diff::parse(&delta) {
        let pieces: Vec<Option<Vec<u64>>> = if file.hunks.is_empty() {
            vec![None]
        } else {
            file.hunks
                .iter()
                .map(|hunk| Some(vec![hunk.id()]))
                .collect()
        };
        for ids in pieces {
            std::fs::copy(&fresh, &scratch).context("copy the review index")?;
            write(&patch, &file.patch(ids.as_deref()))?;
            let applied = with_index(
                worktree,
                &scratch,
                &["apply", "--cached", "--3way", &patch.to_string_lossy()],
            )
            .is_ok()
                && with_index(worktree, &scratch, &["ls-files", "--unmerged"])
                    .is_ok_and(|unmerged| unmerged.trim().is_empty());
            if applied {
                std::fs::rename(&scratch, &fresh).context("keep the carried hunk")?;
            }
        }
    }
    let _ = std::fs::remove_file(&scratch);
    let _ = std::fs::remove_file(&patch);
    std::fs::rename(&fresh, &files.index).context("replace the review index")?;
    Ok(())
}

/// The review of a worktree: its base, what is unreviewed and what is not.
pub(crate) fn state(worktree: &Path, name: &str, base_branch: Option<&str>) -> Result<State> {
    let (files, base_ref, base) = prepare(worktree, name, base_branch)?;
    let (unreviewed, too_large) = unreviewed(worktree, &files)?;
    let reviewed_tree = with_index(worktree, &files.index, &["write-tree"])?;
    let reviewed = run(background(
        worktree,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &base,
            reviewed_tree.trim(),
        ],
    ))?
    .split('\0')
    .filter(|path| !path.is_empty())
    .map(str::to_owned)
    .collect();
    Ok(State {
        base_ref,
        base,
        unreviewed,
        too_large,
        reviewed,
    })
}

/// The diff from the review index to the working tree. New files take part
/// through an intent-to-add entry in a copy of the index.
fn unreviewed(worktree: &Path, files: &Files) -> Result<(Vec<FileDiff>, Vec<String>)> {
    let view = files.dir.join("review.index.view");
    std::fs::copy(&files.index, &view).context("copy the review index")?;
    let others = with_index(
        worktree,
        &view,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let mut too_large = Vec::new();
    let mut new = Vec::new();
    for path in others.split('\0').filter(|path| !path.is_empty()) {
        let size = std::fs::metadata(worktree.join(path)).map_or(0, |metadata| metadata.len());
        if size > tuning::REVIEW_FILE_MAX {
            too_large.push(path.to_owned());
        } else {
            new.push(path);
        }
    }
    for chunk in new.chunks(200) {
        let mut args = vec!["add", "--intent-to-add", "--"];
        args.extend(chunk);
        with_index(worktree, &view, &args)?;
    }
    let text = with_index(
        worktree,
        &view,
        &[
            "diff",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
        ],
    )?;
    let _ = std::fs::remove_file(&view);
    Ok((diff::parse(&text), too_large))
}

/// Marks a file's hunks reviewed, or the whole file with `None`.
pub(crate) fn mark(
    worktree: &Path,
    name: &str,
    base_branch: Option<&str>,
    path: &str,
    hunks: Option<&[u64]>,
) -> Result<()> {
    let (files, _, _) = prepare(worktree, name, base_branch)?;
    let (unreviewed, _) = unreviewed(worktree, &files)?;
    let Some(file) = unreviewed.iter().find(|file| file.path == path) else {
        bail!("{path} has no unreviewed changes");
    };
    let patch = files.dir.join("review.patch");
    write(&patch, &file.patch(hunks))?;
    let applied = with_index(
        worktree,
        &files.index,
        &["apply", "--cached", &patch.to_string_lossy()],
    );
    let _ = std::fs::remove_file(&patch);
    applied?;
    save(worktree, &files, name)
}

/// Deletes a worktree's review ref.
pub(crate) fn delete_ref(dir: &Path, name: &str) -> Result<()> {
    run(background(dir, &["update-ref", "-d", &reference(name)]))?;
    Ok(())
}

/// Deletes the review refs of worktrees that no longer exist.
pub(crate) fn prune_refs(dir: &Path, live: &[String]) -> Result<()> {
    let refs = run(background(
        dir,
        &["for-each-ref", "--format=%(refname)", REFS],
    ))?;
    for full in refs.lines() {
        let name = full.strip_prefix(REFS).unwrap_or(full);
        if !live.iter().any(|live| live == name) {
            run(background(dir, &["update-ref", "-d", full]))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::process::Command;

    struct Repo(PathBuf);

    impl Repo {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("twyx-review-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let repo = Self(root.canonicalize().unwrap());
            repo.git(&["init", "--quiet", "--initial-branch=main"]);
            repo.git(&["config", "user.email", "t@example.com"]);
            repo.git(&["config", "user.name", "t"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
            repo.write("a.txt", &lines);
            repo.commit("first");
            repo.git(&["checkout", "--quiet", "-b", "feature"]);
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .current_dir(&self.0)
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

        fn write(&self, path: &str, text: &str) {
            std::fs::write(self.0.join(path), text).unwrap();
        }

        /// `a.txt` with the given lines replaced.
        fn edit(&self, changes: &[(usize, &str)]) {
            let text = std::fs::read_to_string(self.0.join("a.txt")).unwrap();
            let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
            for (number, line) in changes {
                lines[number - 1] = (*line).to_owned();
            }
            self.write("a.txt", &(lines.join("\n") + "\n"));
        }

        fn commit(&self, message: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "--quiet", "-m", message]);
        }

        fn state(&self) -> State {
            state(&self.0, "feature", None).unwrap()
        }

        fn hunks(&self, path: &str) -> Vec<u64> {
            self.state()
                .unreviewed
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.hunks.iter().map(|hunk| hunk.id()).collect())
                .unwrap_or_default()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(state: &State) -> Vec<&str> {
        state
            .unreviewed
            .iter()
            .map(|file| file.path.as_str())
            .collect()
    }

    #[test]
    fn one_marked_hunk_leaves_the_others_unreviewed_and_the_main_index_alone() {
        let repo = Repo::new("hunk");
        repo.edit(&[(2, "line two"), (18, "line eighteen")]);
        repo.commit("two hunks");
        repo.write("new.txt", "fresh\n");
        let index = std::fs::read(repo.0.join(".git/index")).unwrap();

        let first = repo.state();
        assert_eq!(first.base_ref, "refs/heads/main");
        assert_eq!(paths(&first), vec!["a.txt", "new.txt"]);
        assert!(first.reviewed.is_empty());
        let hunks = repo.hunks("a.txt");
        assert_eq!(hunks.len(), 2);

        mark(&repo.0, "feature", None, "a.txt", Some(&hunks[..1])).unwrap();
        let after = repo.state();
        assert_eq!(after.unreviewed[0].hunks.len(), 1);
        assert!(
            after.unreviewed[0].hunks[0]
                .lines
                .iter()
                .any(|line| line.text == "line eighteen")
        );
        assert_eq!(after.reviewed, vec!["a.txt"]);

        mark(&repo.0, "feature", None, "new.txt", None).unwrap();
        assert_eq!(paths(&repo.state()), vec!["a.txt"]);
        assert_eq!(
            std::fs::read(repo.0.join(".git/index")).unwrap(),
            index,
            "the main index was written"
        );
    }

    #[test]
    fn reviewed_hunks_carry_across_a_rebase_onto_a_moved_base() {
        let repo = Repo::new("rebase");
        repo.edit(&[(2, "line two")]);
        repo.commit("feature change");
        let hunks = repo.hunks("a.txt");
        mark(&repo.0, "feature", None, "a.txt", Some(&hunks)).unwrap();
        assert!(repo.state().unreviewed.is_empty());

        repo.git(&["checkout", "--quiet", "main"]);
        repo.edit(&[(18, "line eighteen on main")]);
        repo.commit("main moves");
        repo.git(&["checkout", "--quiet", "feature"]);
        repo.git(&["rebase", "--quiet", "main"]);

        let after = repo.state();
        assert!(after.unreviewed.is_empty(), "{:?}", paths(&after));
        assert_eq!(after.base, repo.git(&["rev-parse", "main"]).trim());

        // A change on top of the reviewed line is new, so unreviewed.
        repo.edit(&[(2, "line two, again")]);
        assert_eq!(paths(&repo.state()), vec!["a.txt"]);
    }

    #[test]
    fn invariant_12_review_state_survives_gc_and_comes_back_from_its_ref() {
        let repo = Repo::new("gc");
        repo.edit(&[(2, "line two"), (18, "line eighteen")]);
        repo.write("new.txt", "fresh\n");
        mark(&repo.0, "feature", None, "new.txt", None).unwrap();
        let hunks = repo.hunks("a.txt");
        mark(&repo.0, "feature", None, "a.txt", Some(&hunks[1..])).unwrap();
        let before = repo.state();

        std::fs::remove_file(repo.0.join(".git/twyx/review.index")).unwrap();
        repo.git(&["gc", "--quiet", "--prune=now"]);
        assert_eq!(repo.state(), before);

        prune_refs(&repo.0, &["other".to_owned()]).unwrap();
        assert!(repo.git(&["for-each-ref", REFS]).is_empty());
    }
}
