//! `config.toml`: read, checked, and turned into the models it changes.
//!
//! An error names the file, the line and the reason.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::keys::{Chord, Command, Keymap, Mode};
use crate::theme::{Rgb, Theme, Variant};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    pub keymap: Keymap,
    pub theme: ThemeConfig,
}

/// `[theme]`: the reference sheet's colours apply on any paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThemeConfig {
    /// Light or dark; `None` reads it from the terminal's paper.
    pub variant: Option<Variant>,
    /// Replaces the accent.
    pub accent: Option<Rgb>,
}

impl ThemeConfig {
    /// The tokens for a terminal's paper and ink.
    pub fn resolve(&self, paper: Option<Rgb>, ink: Option<Rgb>) -> Theme {
        let theme = match self.variant {
            Some(variant) => Theme::forced(variant, paper, ink),
            None => Theme::on(paper, ink, Variant::Dark),
        };
        match self.accent {
            Some(accent) => theme.with_accent(accent),
            None => theme,
        }
    }
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
    theme: Option<RawTheme>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTheme {
    variant: Option<Spanned<String>>,
    accent: Option<Spanned<String>>,
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
    let mut theme = ThemeConfig::default();
    if let Some(raw_theme) = raw.theme {
        if let Some(variant) = raw_theme.variant {
            theme.variant = match variant.get_ref().as_str() {
                "auto" => None,
                "light" => Some(Variant::Light),
                "dark" => Some(Variant::Dark),
                other => {
                    return Err(fail(
                        Some(variant.span()),
                        format!("variant {other:?} is not auto, light or dark"),
                    ));
                }
            };
        }
        if let Some(accent) = raw_theme.accent {
            theme.accent = Some(
                accent
                    .get_ref()
                    .parse()
                    .map_err(|reason| fail(Some(accent.span()), reason))?,
            );
        }
    }
    let mut keymap = Keymap::default();
    let Some(keys) = raw.keys else {
        return Ok(Config { keymap, theme });
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
    Ok(Config { keymap, theme })
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
        let error = read("[themes]\nname = \"x\"\n").expect_err("an unknown section");
        assert_eq!(error.line, 1);
        assert!(error.reason.contains("themes"), "{}", error.reason);
        let error = read("[theme]\n\naccent = \"blue\"\n").expect_err("a named colour");
        assert_eq!(error.line, 3);
        let error = read("[theme]\nvariant = \"sepia\"\n").expect_err("an unknown variant");
        assert_eq!(error.line, 2);
    }

    #[test]
    fn the_theme_forces_a_variant_and_replaces_the_accent() {
        let theme = read("[theme]\nvariant = \"light\"\naccent = \"#cc3311\"\n")
            .expect("a config")
            .theme
            .resolve(Some(Rgb::hex(0x000000)), None);
        assert_eq!(theme.variant, Variant::Light);
        assert_eq!(theme.accent, Rgb::hex(0xCC3311));
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        assert_eq!(
            load(Path::new("/nonexistent/twyx/config.toml")),
            Ok(Config::default())
        );
    }
}
