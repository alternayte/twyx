//! `config.toml`: read, checked, and turned into the models it changes.
//!
//! An error names the file, the line and the reason.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::keys::{Chord, Command, Keymap, Mode};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    pub keymap: Keymap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub file: PathBuf,
    /// One-based; zero when the file could not be read at all.
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file.display(), self.line, self.reason)
    }
}

impl std::error::Error for ConfigError {}

type Bindings = BTreeMap<Spanned<String>, Spanned<String>>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    keys: Option<RawKeys>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeys {
    prefix: Option<Spanned<String>>,
    command: Option<Bindings>,
    pane: Option<Bindings>,
    tab: Option<Bindings>,
    resize: Option<Bindings>,
    #[serde(rename = "move")]
    move_: Option<Bindings>,
    floating: Option<Bindings>,
}

/// Reads the config file. A missing file is the defaults.
pub fn load(file: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(file) {
        Ok(text) => parse(file, &text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(ConfigError {
            file: file.to_path_buf(),
            line: 0,
            reason: error.to_string(),
        }),
    }
}

pub fn parse(file: &Path, text: &str) -> Result<Config, ConfigError> {
    let fail = |at: Option<std::ops::Range<usize>>, reason: String| ConfigError {
        file: file.to_path_buf(),
        line: at.map_or(1, |at| {
            text[..at.start.min(text.len())].lines().count().max(1)
        }),
        reason,
    };
    let raw: Raw = toml::from_str(text).map_err(|error| {
        fail(
            error.span(),
            error.message().trim_end_matches('\n').to_owned(),
        )
    })?;
    let mut keymap = Keymap::default();
    let Some(keys) = raw.keys else {
        return Ok(Config { keymap });
    };
    if let Some(prefix) = keys.prefix {
        keymap.prefix = prefix
            .get_ref()
            .parse()
            .map_err(|reason| fail(Some(prefix.span()), reason))?;
    }
    let modes = [
        (Mode::Command, keys.command),
        (Mode::Pane, keys.pane),
        (Mode::Tab, keys.tab),
        (Mode::Resize, keys.resize),
        (Mode::Move, keys.move_),
        (Mode::Floating, keys.floating),
    ];
    for (mode, bindings) in modes {
        for (key, command) in bindings.unwrap_or_default() {
            // A table key carries no span of its own; its value is on its line.
            let line = Some(command.span());
            let chord: Chord = key
                .get_ref()
                .parse()
                .map_err(|reason| fail(line.clone(), reason))?;
            if chord == keymap.prefix || chord == Chord::plain(crate::keys::Key::Escape) {
                return Err(fail(
                    line,
                    format!("{chord} is taken: the prefix and esc leave every mode"),
                ));
            }
            let command = match command.get_ref().as_str() {
                "none" => None,
                name => Some(
                    name.parse::<Command>()
                        .map_err(|reason| fail(Some(command.span()), reason))?,
                ),
            };
            keymap.bind(mode, chord, command);
        }
    }
    Ok(Config { keymap })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, Outcome};

    fn read(text: &str) -> Result<Config, ConfigError> {
        parse(Path::new("config.toml"), text)
    }

    #[test]
    fn bindings_replace_and_unbind_defaults_in_their_mode() {
        let mut keymap =
            read("[keys]\nprefix = \"ctrl-a\"\n\n[keys.pane]\nv = \"split-right\"\nr = \"none\"\n")
                .expect("a config")
                .keymap;
        assert_eq!(keymap.prefix, Chord::ctrl(Key::Char('a')));
        keymap.press(Chord::ctrl(Key::Char('a')));
        keymap.press(Chord::plain(Key::Char('p')));
        assert_eq!(
            keymap.press(Chord::plain(Key::Char('v'))),
            Outcome::Run(Command::SplitRight)
        );
        assert_eq!(
            keymap.press(Chord::plain(Key::Char('r'))),
            Outcome::Consumed
        );
    }

    #[test]
    fn errors_name_the_line_and_the_reason() {
        let error = read("[keys]\n\n[keys.tab]\nn = \"new-tab\"\nq = \"quit-everything\"\n")
            .expect_err("an unknown command");
        assert_eq!(
            error.to_string(),
            "config.toml:5: \"quit-everything\" is not a command"
        );
        let error = read("[keys]\nprefix = \"hyper-x\"\n").expect_err("a bad key");
        assert_eq!(error.line, 2);
        let error = read("[keys.pane]\nesc = \"close-pane\"\n").expect_err("esc is taken");
        assert_eq!(error.line, 2);
        let error = read("[theme]\nname = \"x\"\n").expect_err("an unknown section");
        assert_eq!(error.line, 1);
        assert!(error.reason.contains("theme"), "{}", error.reason);
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        assert_eq!(
            load(Path::new("/nonexistent/twyx/config.toml")),
            Ok(Config::default())
        );
    }
}
