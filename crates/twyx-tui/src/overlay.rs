//! Overlays that take every key while they are open: the command palette,
//! the workspace switcher, the key sheet, the prompts and the confirmations.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::keys::{Chord, Key};

pub use twyx_protocol::dialog::Purpose;
use twyx_protocol::dialog::{Answer, Dialog};
use twyx_protocol::files::{FindKey, Finder};
use twyx_protocol::lists::{self, CheckKey, InboxKey};
pub use twyx_protocol::palette::Choice;
use twyx_protocol::palette::{self, Pick, Picker};

use crate::chrome::dialog::Dialog as DialogBox;
use crate::chrome::keys::KeySheet;
use crate::chrome::palette::{Entry, Palette};
use crate::compositor::Scene;

/// What one key in an overlay comes to.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    Choose(Choice),
    Prompted {
        purpose: Purpose,
        text: String,
    },
    /// A key the confirmation offered.
    Confirmed {
        purpose: Purpose,
        key: char,
    },
    /// Mark an inbox item unread, or dismiss it.
    Inbox {
        item: u64,
        dismiss: bool,
    },
    /// A request a key in the finder made.
    Send(twyx_protocol::client::ToDaemon),
    /// A key in the checks list of `worktree`.
    Check {
        worktree: u64,
        key: CheckKey,
    },
    /// A form's values, in field order.
    Submitted {
        purpose: Purpose,
        values: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// The command palette, or the switcher over projects, worktrees and
    /// workspaces.
    Picker(Picker),
    /// A prompt, a form or a confirmation.
    Dialog(Dialog),
    /// The inbox, newest first.
    Inbox { selected: usize },
    /// Every bound key, by mode.
    Keys { scroll: usize },
    /// A worktree's checks with their last results.
    Checks { worktree: u64, selected: usize },
    /// The search of the workspace's files.
    Find(Finder),
}

impl Overlay {
    pub fn palette() -> Self {
        Overlay::Picker(Picker::palette())
    }

    pub fn switcher() -> Self {
        Overlay::Picker(Picker::switcher())
    }

    /// Takes one key. Typing edits the line, up and down move the choice,
    /// enter takes it, and esc closes.
    pub fn key(&mut self, scene: &Scene<'_>, chord: Chord) -> Outcome {
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        match self {
            Overlay::Picker(picker) => {
                let entries = entries(scene, picker.scope);
                match picker.key(chord, &entries) {
                    Pick::Stay => Outcome::Stay,
                    Pick::Close => Outcome::Close,
                    Pick::Choose(choice) => Outcome::Choose(choice),
                }
            }
            Overlay::Keys { scroll } => {
                let sections = scene.keymap.sheet();
                let sheet = KeySheet {
                    pen: scene.pen,
                    sections: &sections,
                    scroll: *scroll,
                };
                let (end, page) = sheet.reach(scene.panes_area());
                *scroll = match (chord.key, typed) {
                    (Key::Escape | Key::Enter, _) | (_, Some('?' | 'q')) => return Outcome::Close,
                    (Key::Down, _) | (_, Some('j')) => *scroll + 1,
                    (Key::Up, _) | (_, Some('k')) => scroll.saturating_sub(1),
                    (Key::PageDown, _) | (_, Some(' ')) => *scroll + page,
                    (Key::PageUp, _) => scroll.saturating_sub(page),
                    (Key::Home, _) | (_, Some('g')) => 0,
                    (Key::End, _) | (_, Some('G')) => end,
                    _ => *scroll,
                }
                .min(end);
                Outcome::Stay
            }
            Overlay::Dialog(dialog) => match dialog.key(chord) {
                Answer::Stay => Outcome::Stay,
                Answer::Close => Outcome::Close,
                Answer::Prompted { purpose, text } => Outcome::Prompted { purpose, text },
                Answer::Submitted { purpose, values } => Outcome::Submitted { purpose, values },
                Answer::Confirmed { purpose, key } => Outcome::Confirmed { purpose, key },
            },
            Overlay::Inbox { selected } => match lists::inbox_key(selected, scene.inbox, chord) {
                InboxKey::Stay => Outcome::Stay,
                InboxKey::Close => Outcome::Close,
                InboxKey::Jump(pane) => Outcome::Choose(lists::jump(pane, scene.overview)),
                InboxKey::Unread(item) => Outcome::Inbox {
                    item,
                    dismiss: false,
                },
                InboxKey::Dismiss(item) => Outcome::Inbox {
                    item,
                    dismiss: true,
                },
            },
            Overlay::Find(finder) => match finder.key(chord) {
                FindKey::Stay => Outcome::Stay,
                FindKey::Close => Outcome::Close,
                FindKey::Send(request) => Outcome::Send(request),
            },
            Overlay::Checks { worktree, selected } => {
                match lists::checks_key(selected, scene.checks_of(*worktree), chord) {
                    CheckKey::Stay => Outcome::Stay,
                    CheckKey::Close => Outcome::Close,
                    key => Outcome::Check {
                        worktree: *worktree,
                        key,
                    },
                }
            }
        }
    }

    /// Pasted text goes into the line, without its line breaks.
    pub fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
        match self {
            Overlay::Picker(picker) => picker.paste(&text),
            Overlay::Dialog(dialog) => dialog.paste(&text),
            Overlay::Find(finder) => {
                finder.paste(&text);
            }
            Overlay::Inbox { .. } | Overlay::Checks { .. } | Overlay::Keys { .. } => {}
        }
    }

    pub fn render(&self, scene: &Scene<'_>, area: Rect, buf: &mut Buffer) {
        match self {
            Overlay::Picker(picker) => {
                let entries = entries(scene, picker.scope);
                let found = picker.found(&entries);
                let mut shown: Vec<Entry<'_>> = found
                    .iter()
                    .map(|found| {
                        let entry = &entries[found.index];
                        Entry {
                            text: &entry.label,
                            kind: &entry.kind,
                            matched: &found.positions,
                        }
                    })
                    .collect();
                // Quick open with no list says why, such as in the default
                // workspace.
                if let (true, palette::Scope::Files, Some(error)) =
                    (shown.is_empty(), picker.scope, scene.files_error)
                {
                    shown.push(Entry {
                        text: error,
                        kind: "",
                        matched: &[],
                    });
                }
                // The list scrolls so the choice stays in view.
                let room = usize::from(area.height.saturating_sub(6)).max(1);
                let selected = picker.selected;
                let skip = selected.saturating_sub(room - 1);
                Palette {
                    pen: scene.pen,
                    title: picker.scope.title(),
                    query: &picker.query,
                    entries: &shown[skip.min(shown.len())..],
                    selected: selected - skip.min(selected),
                }
                .render(area, buf);
            }
            Overlay::Find(finder) => {
                let lines: Vec<(String, String)> = finder
                    .hits()
                    .iter()
                    .map(|hit| {
                        (
                            hit.text.trim().to_owned(),
                            format!("{}:{}", hit.path, hit.line),
                        )
                    })
                    .collect();
                let mut entries: Vec<Entry<'_>> = lines
                    .iter()
                    .map(|(text, kind)| Entry {
                        text,
                        kind,
                        matched: &[],
                    })
                    .collect();
                if let (true, Some(error)) = (
                    entries.is_empty(),
                    finder
                        .found
                        .as_ref()
                        .and_then(|found| found.error.as_deref()),
                ) {
                    entries.push(Entry {
                        text: error,
                        kind: "",
                        matched: &[],
                    });
                }
                let room = usize::from(area.height.saturating_sub(6)).max(1);
                let selected = finder.selected.min(entries.len().saturating_sub(1));
                let skip = selected.saturating_sub(room - 1);
                Palette {
                    pen: scene.pen,
                    title: "find in files",
                    query: &finder.query,
                    entries: &entries[skip.min(entries.len())..],
                    selected: selected - skip.min(selected),
                }
                .render(area, buf);
            }
            Overlay::Inbox { selected } => {
                let now = lists::now_ms();
                let lines: Vec<(String, String)> = scene
                    .inbox
                    .iter()
                    .map(|item| {
                        let mark = item.mark.map_or(' ', |mark| mark.glyph());
                        let unread = if item.read { ' ' } else { '•' };
                        (
                            format!(
                                "{unread} {mark} {}{} · {}",
                                lists::state(item),
                                lists::place(item),
                                item.title
                            ),
                            lists::age(now.saturating_sub(item.created_ms)),
                        )
                    })
                    .collect();
                let entries: Vec<Entry<'_>> = lines
                    .iter()
                    .map(|(text, kind)| Entry {
                        text,
                        kind,
                        matched: &[],
                    })
                    .collect();
                let room = usize::from(area.height.saturating_sub(6)).max(1);
                let selected = (*selected).min(entries.len().saturating_sub(1));
                let skip = selected.saturating_sub(room - 1);
                Palette {
                    pen: scene.pen,
                    title: "inbox",
                    query: lists::INBOX_KEYS,
                    entries: &entries[skip.min(entries.len())..],
                    selected: selected - skip.min(selected),
                }
                .render(area, buf);
            }
            Overlay::Checks { worktree, selected } => {
                let now = lists::now_ms();
                let checks = scene.checks_of(*worktree);
                let width = checks
                    .iter()
                    .map(|entry| entry.check.name.chars().count())
                    .max()
                    .unwrap_or(0);
                let lines: Vec<(String, String)> = checks
                    .iter()
                    .map(|entry| {
                        let (result, when) = lists::result(entry, now);
                        let name = &entry.check.name;
                        (
                            format!("{name:width$}  {result} · {}", entry.check.command),
                            when,
                        )
                    })
                    .collect();
                let entries: Vec<Entry<'_>> = lines
                    .iter()
                    .map(|(text, kind)| Entry {
                        text,
                        kind,
                        matched: &[],
                    })
                    .collect();
                let room = usize::from(area.height.saturating_sub(6)).max(1);
                let selected = (*selected).min(entries.len().saturating_sub(1));
                let skip = selected.saturating_sub(room - 1);
                let untrusted = scene
                    .checks
                    .and_then(|info| info.file.as_ref())
                    .is_some_and(|file| !file.trusted);
                let query = lists::checks_keys(untrusted);
                Palette {
                    pen: scene.pen,
                    title: "checks",
                    query,
                    entries: &entries[skip.min(entries.len())..],
                    selected: selected - skip.min(selected),
                }
                .render(area, buf);
            }
            Overlay::Keys { scroll } => KeySheet {
                pen: scene.pen,
                sections: &scene.keymap.sheet(),
                scroll: *scroll,
            }
            .render(area, buf),
            Overlay::Dialog(Dialog::Prompt { title, text, .. }) => Palette {
                pen: scene.pen,
                title,
                query: text,
                entries: &[],
                selected: 0,
            }
            .render(area, buf),
            Overlay::Dialog(Dialog::Form {
                title,
                fields,
                at,
                purpose,
            }) => {
                let width = fields
                    .iter()
                    .map(|(label, _)| label.chars().count())
                    .max()
                    .unwrap_or(0);
                let mut lines: Vec<String> =
                    purpose.hint().into_iter().map(str::to_owned).collect();
                lines.extend(fields.iter().enumerate().map(|(number, (label, value))| {
                    let (marker, cursor) = if number == *at {
                        ("›", "▏")
                    } else {
                        (" ", "")
                    };
                    format!("{marker} {label:width$}  {value}{cursor}")
                }));
                let keys = vec![
                    ("tab".to_owned(), "next".to_owned()),
                    ("enter".to_owned(), purpose.submit().to_owned()),
                    ("esc".to_owned(), "cancel".to_owned()),
                ];
                DialogBox {
                    pen: scene.pen,
                    title,
                    lines: &lines,
                    keys: &keys,
                }
                .render(area, buf);
            }
            Overlay::Dialog(Dialog::Confirm {
                title, lines, keys, ..
            }) => {
                let mut keys: Vec<(String, String)> = keys
                    .iter()
                    .map(|(key, word)| (key.to_string(), word.clone()))
                    .collect();
                keys.push(("esc".to_owned(), "cancel".to_owned()));
                DialogBox {
                    pen: scene.pen,
                    title,
                    lines,
                    keys: &keys,
                }
                .render(area, buf);
            }
        }
    }
}

/// What the palette, the switcher or quick open offers in this scene. The
/// TUI's palette leaves the files to quick open.
fn entries(scene: &Scene<'_>, scope: palette::Scope) -> Vec<palette::Entry> {
    let files: &[String] = match scope {
        palette::Scope::Files => scene.files,
        palette::Scope::Palette | palette::Scope::Switcher => &[],
    };
    palette::entries(
        scene.keymap,
        scene.workspace,
        scene.overview,
        |leaf| scene.leaf_title(leaf),
        |pane| scene.title(pane),
        files,
        scope,
    )
}
