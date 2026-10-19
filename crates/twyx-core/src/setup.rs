//! `.twyx/worktree.toml`: what a new worktree gets from the main one, and
//! the commands that set it up.
//!
//! ```toml
//! shared = ["node_modules"]           # linked from the main worktree
//! setup = ["npm install", "make env"] # run in order, in one pane
//! ```

use std::path::{Component, Path};

use serde::Deserialize;
use toml::Spanned;

use crate::config::ConfigError;

/// Where the setup file lives in a worktree.
pub const FILE: &str = ".twyx/worktree.toml";

/// Where the list of gitignored files to copy lives in a repository.
pub const INCLUDE_FILE: &str = ".worktreeinclude";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Setup {
    /// Paths relative to the worktree, linked from the main worktree.
    pub shared: Vec<String>,
    /// Commands for `/bin/sh`, run in order; the first failure stops them.
    pub commands: Vec<String>,
}

impl Setup {
    /// One script that runs every command in order and stops at the first
    /// failure.
    pub fn script(&self) -> String {
        let mut script = String::from("set -e\n");
        for command in &self.commands {
            script.push_str(command);
            script.push('\n');
        }
        script
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    shared: Vec<Spanned<String>>,
    #[serde(default)]
    setup: Vec<Spanned<String>>,
}

pub fn parse(file: &Path, text: &str) -> Result<Setup, ConfigError> {
    let fail = |at: Option<std::ops::Range<usize>>, reason: String| ConfigError {
        file: file.to_path_buf(),
        line: at.map_or(1, |at| {
            text[..at.start.min(text.len())].matches('\n').count() + 1
        }),
        reason,
    };
    let raw: Raw = toml::from_str(text).map_err(|error| {
        fail(
            error.span(),
            error.message().trim_end_matches('\n').to_owned(),
        )
    })?;
    let mut shared = Vec::new();
    for path in raw.shared {
        let inside = !path.get_ref().is_empty()
            && Path::new(path.get_ref())
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
        if !inside {
            return Err(fail(
                Some(path.span()),
                format!(
                    "shared path {:?} must stay inside the worktree, with no .. or leading /",
                    path.get_ref()
                ),
            ));
        }
        shared.push(path.into_inner());
    }
    let mut commands = Vec::new();
    for command in raw.setup {
        if command.get_ref().trim().is_empty() {
            return Err(fail(
                Some(command.span()),
                "a setup command is empty".to_owned(),
            ));
        }
        commands.push(command.into_inner());
    }
    Ok(Setup { shared, commands })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_setup_file_reads_and_refuses_paths_outside_the_worktree() {
        let file = Path::new("worktree.toml");
        let setup = parse(
            file,
            "shared = [\"node_modules\"]\nsetup = [\"a\", \"b\"]\n",
        )
        .unwrap();
        assert_eq!(setup.shared, vec!["node_modules"]);
        assert_eq!(setup.script(), "set -e\na\nb\n");
        for bad in ["../x", "/etc", ""] {
            let error = parse(file, &format!("\nshared = [\"{bad}\"]\n")).expect_err(bad);
            assert_eq!(error.line, 2, "{bad}");
        }
        assert_eq!(parse(file, "setup = [\" \"]\n").expect_err("empty").line, 1);
    }
}
