//! `config.toml`: read, checked, and turned into the models it changes.
//!
//! An error names the file, the line and the reason.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::inbox::Reason;
use crate::keys::{Chord, Command, Keymap, Mode};
use crate::theme::{Rgb, Theme, Variant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub keymap: Keymap,
    pub theme: ThemeConfig,
    /// `[history] enabled`: whether panes keep a history on disk.
    pub history: bool,
    /// `[worktrees] location`: where a new worktree goes, with `{parent}`,
    /// `{repo}` and `{branch}`.
    pub worktree_location: Option<String>,
    /// `[worktrees] branch-prefix`: what a new task's branch starts with.
    pub branch_prefix: Option<String>,
    /// `[agents] default`: the agent kind an agent pane starts.
    pub default_agent: Option<String>,
    /// `[notifications]`: what each kind of inbox item does.
    pub notifications: Notifications,
    /// `[[check]]`: the user's checks, for every worktree.
    pub checks: Vec<crate::check::Check>,
    /// `[clipboard] osc52`: which panes may set the clipboard.
    pub osc52: Osc52,
}

/// Which panes may set the clipboard with OSC 52. No pane reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Osc52 {
    /// Any pane.
    Allow,
    /// Only a pane that has the focus in an interactive client.
    #[default]
    Focused,
    /// No pane.
    Deny,
}

/// What an inbox item of one kind does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// No inbox item at all.
    Off,
    Inbox,
    /// An inbox item and a notification.
    Notify,
    /// An inbox item and a notification with a sound.
    Sound,
}

/// `[notifications]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notifications {
    pub needs_approval: Notice,
    pub turn_finished: Notice,
    pub failed: Notice,
    pub check_ended: Notice,
    /// A sound file the desktop client plays in place of its own.
    pub sound: Option<String>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            needs_approval: Notice::Sound,
            turn_finished: Notice::Notify,
            failed: Notice::Notify,
            check_ended: Notice::Notify,
            sound: None,
        }
    }
}

impl Notifications {
    pub fn for_reason(&self, reason: Reason) -> Notice {
        match reason {
            Reason::NeedsApproval => self.needs_approval,
            Reason::TurnFinished => self.turn_finished,
            Reason::Failed => self.failed,
            Reason::CheckEnded => self.check_ended,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keymap: Keymap::default(),
            theme: ThemeConfig::default(),
            history: true,
            worktree_location: None,
            branch_prefix: None,
            default_agent: None,
            notifications: Notifications::default(),
            checks: Vec::new(),
            osc52: Osc52::default(),
        }
    }
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

impl ConfigError {
    /// The error with the file's name alone, for a status bar.
    pub fn brief(&self) -> String {
        let name = self.file.file_name().map_or_else(
            || self.file.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        format!("{name}:{}: {}", self.line, self.reason)
    }
}

impl std::error::Error for ConfigError {}

type Bindings = BTreeMap<Spanned<String>, Spanned<String>>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    keys: Option<RawKeys>,
    theme: Option<RawTheme>,
    history: Option<RawHistory>,
    worktrees: Option<RawWorktrees>,
    agents: Option<RawAgents>,
    notifications: Option<RawNotifications>,
    #[serde(default)]
    check: Vec<crate::check::RawCheck>,
    clipboard: Option<RawClipboard>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClipboard {
    osc52: Option<Spanned<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawNotifications {
    needs_approval: Option<Spanned<String>>,
    turn_finished: Option<Spanned<String>>,
    failed: Option<Spanned<String>>,
    check_ended: Option<Spanned<String>>,
    sound: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgents {
    default: Option<Spanned<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawWorktrees {
    location: Option<Spanned<String>>,
    branch_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHistory {
    enabled: Option<bool>,
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
    workspace: Option<Bindings>,
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
    let history = raw
        .history
        .and_then(|history| history.enabled)
        .unwrap_or(true);
    let branch_prefix = raw
        .worktrees
        .as_ref()
        .and_then(|worktrees| worktrees.branch_prefix.clone());
    let worktree_location = match raw.worktrees.and_then(|worktrees| worktrees.location) {
        Some(location) => {
            let text = location.get_ref();
            if !text.contains("{branch}") {
                return Err(fail(
                    Some(location.span()),
                    "the worktree location needs {branch}, or every worktree gets one directory"
                        .to_owned(),
                ));
            }
            if !text.starts_with('/') && !text.starts_with("~/") && !text.starts_with("{parent}") {
                return Err(fail(
                    Some(location.span()),
                    "the worktree location must start with /, ~/ or {parent}".to_owned(),
                ));
            }
            Some(text.clone())
        }
        None => None,
    };
    let known = crate::agent::names_in(&file.with_file_name("agents"));
    let default_agent = match raw.agents.and_then(|agents| agents.default) {
        Some(kind) if !known.contains(kind.get_ref()) => {
            return Err(fail(
                Some(kind.span()),
                format!(
                    "{:?} is not an agent kind; the kinds are {}",
                    kind.get_ref(),
                    known.join(", ")
                ),
            ));
        }
        Some(kind) => Some(kind.into_inner()),
        None => None,
    };
    let mut notifications = Notifications::default();
    if let Some(raw_notifications) = raw.notifications {
        let notice = |value: Option<Spanned<String>>, default: Notice| match value {
            None => Ok(default),
            Some(value) => match value.get_ref().as_str() {
                "off" => Ok(Notice::Off),
                "inbox" => Ok(Notice::Inbox),
                "notify" => Ok(Notice::Notify),
                "sound" => Ok(Notice::Sound),
                other => Err(fail(
                    Some(value.span()),
                    format!("{other:?} is not off, inbox, notify or sound"),
                )),
            },
        };
        notifications = Notifications {
            needs_approval: notice(
                raw_notifications.needs_approval,
                notifications.needs_approval,
            )?,
            turn_finished: notice(raw_notifications.turn_finished, notifications.turn_finished)?,
            failed: notice(raw_notifications.failed, notifications.failed)?,
            check_ended: notice(raw_notifications.check_ended, notifications.check_ended)?,
            sound: raw_notifications.sound,
        };
    }
    let checks = crate::check::read(raw.check, fail)?;
    let osc52 = match raw.clipboard.and_then(|clipboard| clipboard.osc52) {
        None => Osc52::default(),
        Some(value) => match value.get_ref().as_str() {
            "allow" => Osc52::Allow,
            "focused" => Osc52::Focused,
            "deny" => Osc52::Deny,
            other => {
                return Err(fail(
                    Some(value.span()),
                    format!("{other:?} is not allow, focused or deny"),
                ));
            }
        },
    };
    let mut keymap = Keymap::default();
    let Some(keys) = raw.keys else {
        return Ok(Config {
            keymap,
            theme,
            history,
            worktree_location,
            branch_prefix,
            default_agent,
            notifications,
            checks,
            osc52,
        });
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
        (Mode::Workspace, keys.workspace),
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
    Ok(Config {
        keymap,
        theme,
        history,
        worktree_location,
        branch_prefix,
        default_agent,
        notifications,
        checks,
        osc52,
    })
}

/// Notices when `config.toml` changes, by its modification time, at most
/// once per reload interval.
#[derive(Debug)]
pub struct Watch {
    file: PathBuf,
    modified: Option<std::time::SystemTime>,
    checked: std::time::Instant,
}

impl Watch {
    pub fn new(file: &Path) -> Self {
        Self {
            file: file.to_path_buf(),
            modified: modified(file),
            checked: std::time::Instant::now(),
        }
    }

    /// The config again, if the file changed since the last look.
    pub fn poll(&mut self) -> Option<Result<Config, ConfigError>> {
        if self.checked.elapsed() < crate::tuning::CONFIG_POLL {
            return None;
        }
        self.checked = std::time::Instant::now();
        let now = modified(&self.file);
        if now == self.modified {
            return None;
        }
        self.modified = now;
        Some(load(&self.file))
    }
}

fn modified(file: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(file)
        .and_then(|meta| meta.modified())
        .ok()
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
        let error = read("[worktrees]\nlocation = \"/wt/{repo}\"\n")
            .expect_err("no branch in the location");
        assert_eq!(error.line, 2);
        let error = read("[worktrees]\nlocation = \"wt/{branch}\"\n").expect_err("a relative path");
        assert_eq!(error.line, 2);
        let error = read("[agents]\ndefault = \"gpt\"\n").expect_err("an unknown agent");
        assert_eq!(error.line, 2);
        let error = read("[notifications]\n\nfailed = \"loud\"\n").expect_err("an unknown notice");
        assert_eq!(error.line, 3);
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
