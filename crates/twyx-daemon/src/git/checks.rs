//! Source keys for check runs.

use std::path::Path;

use anyhow::{Context, Result};

use super::{background, run};

/// The tree ID of the worktree's tracked files and its untracked files that
/// are not ignored, built in a copy of its index so the main index is never
/// written (Invariant 10).
pub(crate) fn source_key(worktree: &Path) -> Result<String> {
    let git_dir = run(background(worktree, &["rev-parse", "--absolute-git-dir"]))?;
    let git_dir = Path::new(git_dir.trim());
    let dir = git_dir.join("twyx");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let copy = dir.join(format!("source.index.{}", std::process::id()));
    let main = git_dir.join("index");
    if main.exists() {
        std::fs::copy(&main, &copy).context("copy the index")?;
    }
    let key = (|| {
        let mut add = background(worktree, &["add", "--all", "--", "."]);
        add.env("GIT_INDEX_FILE", &copy);
        run(add)?;
        let mut write = background(worktree, &["write-tree"]);
        write.env("GIT_INDEX_FILE", &copy);
        Ok(run(write)?.trim().to_owned())
    })();
    let _ = std::fs::remove_file(&copy);
    key
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::process::Command;

    #[test]
    fn the_source_key_follows_tracked_and_new_files_but_not_ignored_ones() {
        let root = std::env::temp_dir().join(format!("twyx-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        git(&["init", "--quiet", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.email=t@e",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            "one",
        ]);
        let index = std::fs::read(root.join(".git/index")).unwrap();

        let clean = source_key(&root).unwrap();
        assert_eq!(source_key(&root).unwrap(), clean, "stable");
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/out"), "built\n").unwrap();
        assert_eq!(
            source_key(&root).unwrap(),
            clean,
            "an ignored file is not source"
        );
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        let with_new = source_key(&root).unwrap();
        assert_ne!(with_new, clean, "a new file is source");
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        assert_ne!(source_key(&root).unwrap(), with_new, "an edit is source");
        assert_eq!(
            std::fs::read(root.join(".git/index")).unwrap(),
            index,
            "the main index was written"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
