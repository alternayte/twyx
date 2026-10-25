//! The inbox and checks lists both clients share: their keys and the text
//! of each row.

use twyx_core::keys::{Chord, Key};

use crate::client::{CheckInfo, InboxItem};

/// What a key in the inbox list asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKey {
    Stay,
    Close,
    /// Show the item's pane.
    Jump(u64),
    Unread(u64),
    Dismiss(u64),
}

/// Takes a key in the inbox list, with `selected` its cursor.
pub fn inbox_key(selected: &mut usize, items: &[InboxItem], chord: Chord) -> InboxKey {
    *selected = (*selected).min(items.len().saturating_sub(1));
    let item = items.get(*selected);
    match (chord.key, typed(chord)) {
        (Key::Escape, _) => InboxKey::Close,
        (Key::Up, _) | (_, Some('k')) => {
            *selected = selected.saturating_sub(1);
            InboxKey::Stay
        }
        (Key::Down, _) | (_, Some('j')) => {
            *selected = (*selected + 1).min(items.len().saturating_sub(1));
            InboxKey::Stay
        }
        (Key::Enter, _) => item.map_or(InboxKey::Stay, |item| InboxKey::Jump(item.pane)),
        (_, Some('u')) => item.map_or(InboxKey::Stay, |item| InboxKey::Unread(item.item)),
        (_, Some('x')) => item.map_or(InboxKey::Stay, |item| InboxKey::Dismiss(item.item)),
        _ => InboxKey::Stay,
    }
}

/// The keys of the inbox list, for its hint line.
pub const INBOX_KEYS: &str = "enter jump  u unread  x dismiss";

/// What a key in the checks list asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckKey {
    Stay,
    Close,
    Run(String),
    /// Show the check's last run.
    Show(String),
    Cancel(String),
    /// Ask to trust the repository check file.
    Trust,
}

/// Takes a key in the checks list, with `selected` its cursor.
pub fn checks_key(selected: &mut usize, checks: &[CheckInfo], chord: Chord) -> CheckKey {
    *selected = (*selected).min(checks.len().saturating_sub(1));
    let name = checks.get(*selected).map(|entry| entry.check.name.clone());
    match (chord.key, typed(chord)) {
        (Key::Escape, _) => CheckKey::Close,
        (Key::Up, _) | (_, Some('k')) => {
            *selected = selected.saturating_sub(1);
            CheckKey::Stay
        }
        (Key::Down, _) | (_, Some('j')) => {
            *selected = (*selected + 1).min(checks.len().saturating_sub(1));
            CheckKey::Stay
        }
        (Key::Enter, _) | (_, Some('r')) => name.map_or(CheckKey::Stay, CheckKey::Run),
        (_, Some('o')) => name.map_or(CheckKey::Stay, CheckKey::Show),
        (_, Some('x')) => name.map_or(CheckKey::Stay, CheckKey::Cancel),
        (_, Some('t')) => CheckKey::Trust,
        _ => CheckKey::Stay,
    }
}

/// The keys of the checks list, for its hint line.
pub fn checks_keys(untrusted: bool) -> &'static str {
    if untrusted {
        "enter run  o output  x cancel  t trust the repository's checks"
    } else {
        "enter run  o output  x cancel"
    }
}

fn typed(chord: Chord) -> Option<char> {
    match chord.key {
        Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
        _ => None,
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// An age in the largest whole unit.
pub fn age(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Where an inbox item is: ` · project / worktree`, or nothing.
pub fn place(item: &InboxItem) -> String {
    match (&item.project, &item.worktree) {
        (Some(project), Some(worktree)) => format!(" · {project} / {worktree}"),
        (Some(project), None) => format!(" · {project}"),
        (None, _) => String::new(),
    }
}

/// An inbox item's state word: its mark's, else its reason's.
pub fn state(item: &InboxItem) -> &'static str {
    item.mark.map_or(item.reason.name(), |mark| mark.word())
}

/// A check's last result and how long ago it ended, or what stands in for
/// them.
pub fn result(entry: &CheckInfo, now: u64) -> (String, String) {
    match &entry.run {
        None => ("never run".to_owned(), String::new()),
        Some(run) => match run.outcome {
            None => ("running".to_owned(), String::new()),
            Some(outcome) => (
                twyx_core::check::label(outcome, run.stale),
                age(now.saturating_sub(run.ended_ms.unwrap_or(now))),
            ),
        },
    }
}
