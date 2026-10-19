//! Overlays that take every key while they are open: the command palette
//! and the prompt that renames a tab.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::keys::{Chord, Command, Key, Mode};
use twyx_core::layout::LeafId;
use twyx_core::palette::search;

use crate::chrome::palette::{Entry, Palette};
use crate::compositor::Scene;

/// What a palette entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Command(Command),
    Tab(u64),
    Leaf(LeafId),
}

/// What one key in an overlay comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Choose(Choice),
    Rename { tab: u64, name: String },
    Search(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Palette {
        query: String,
        selected: usize,
    },
    Rename {
        tab: u64,
        name: String,
    },
    /// The needle for a copy mode search.
    Search(String),
}

/// One line of the palette, before the query filters it.
struct Line {
    label: String,
    kind: String,
    choice: Choice,
}

impl Overlay {
    pub fn palette() -> Self {
        Overlay::Palette {
            query: String::new(),
            selected: 0,
        }
    }

    /// Takes one key. Typing edits the line, up and down move the choice,
    /// enter takes it, and esc closes.
    pub fn key(&mut self, scene: &Scene<'_>, chord: Chord) -> Outcome {
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        match self {
            Overlay::Palette { query, selected } => {
                let lines = lines(scene);
                let found = search(lines.iter().map(|line| line.label.as_str()), query);
                let count = found.len();
                match (chord.key, typed) {
                    (Key::Escape, _) => Outcome::Close,
                    (Key::Enter, _) => found.get(*selected).map_or(Outcome::Close, |found| {
                        Outcome::Choose(lines[found.index].choice)
                    }),
                    (Key::Up, _) => {
                        *selected = selected.saturating_sub(1);
                        Outcome::Stay
                    }
                    (Key::Down, _) => {
                        *selected = (*selected + 1).min(count.saturating_sub(1));
                        Outcome::Stay
                    }
                    (Key::Backspace, _) => {
                        query.pop();
                        *selected = 0;
                        Outcome::Stay
                    }
                    (_, Some(ch)) => {
                        query.push(ch);
                        *selected = 0;
                        Outcome::Stay
                    }
                    _ => Outcome::Stay,
                }
            }
            Overlay::Search(needle) => match (chord.key, typed) {
                (Key::Escape, _) => Outcome::Close,
                (Key::Enter, _) => Outcome::Search(needle.clone()),
                (Key::Backspace, _) => {
                    needle.pop();
                    Outcome::Stay
                }
                (_, Some(ch)) => {
                    needle.push(ch);
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
            Overlay::Rename { tab, name } => match (chord.key, typed) {
                (Key::Escape, _) => Outcome::Close,
                (Key::Enter, _) => Outcome::Rename {
                    tab: *tab,
                    name: name.trim().to_owned(),
                },
                (Key::Backspace, _) => {
                    name.pop();
                    Outcome::Stay
                }
                (_, Some(ch)) => {
                    name.push(ch);
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
        }
    }

    /// Pasted text goes into the line, without its line breaks.
    pub fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
        match self {
            Overlay::Palette { query, selected } => {
                query.push_str(&text);
                *selected = 0;
            }
            Overlay::Rename { name, .. } => name.push_str(&text),
            Overlay::Search(needle) => needle.push_str(&text),
        }
    }

    pub fn render(&self, scene: &Scene<'_>, area: Rect, buf: &mut Buffer) {
        match self {
            Overlay::Palette { query, selected } => {
                let lines = lines(scene);
                let found = search(lines.iter().map(|line| line.label.as_str()), query);
                let entries: Vec<Entry<'_>> = found
                    .iter()
                    .map(|found| {
                        let line = &lines[found.index];
                        Entry {
                            text: &line.label,
                            kind: &line.kind,
                            matched: &found.positions,
                        }
                    })
                    .collect();
                // The list scrolls so the choice stays in view.
                let room = usize::from(area.height.saturating_sub(6)).max(1);
                let skip = selected.saturating_sub(room - 1);
                Palette {
                    pen: scene.pen,
                    title: "palette",
                    query,
                    entries: &entries[skip.min(entries.len())..],
                    selected: selected - skip.min(*selected),
                }
                .render(area, buf);
            }
            Overlay::Rename { name, .. } => Palette {
                pen: scene.pen,
                title: "rename tab",
                query: name,
                entries: &[],
                selected: 0,
            }
            .render(area, buf),
            Overlay::Search(needle) => Palette {
                pen: scene.pen,
                title: "search",
                query: needle,
                entries: &[],
                selected: 0,
            }
            .render(area, buf),
        }
    }
}

/// Everything the palette offers: the commands with their keys, then the
/// tabs, then the panes of every tab.
fn lines(scene: &Scene<'_>) -> Vec<Line> {
    let mut lines: Vec<Line> = Command::every()
        .into_iter()
        .filter(|command| {
            !matches!(
                command,
                Command::Enter(_) | Command::GoToTab(_) | Command::Palette
            )
        })
        .filter(|command| {
            Mode::BOUND.iter().any(|(mode, _)| {
                scene
                    .keymap
                    .bindings(*mode)
                    .iter()
                    .any(|(_, c)| c == command)
            }) || matches!(command, Command::RenameTab)
        })
        .map(|command| Line {
            label: command.name().replace('-', " "),
            kind: scene
                .keys_for(command)
                .unwrap_or_else(|| "command".to_owned()),
            choice: Choice::Command(command),
        })
        .collect();
    let Some(workspace) = scene.workspace else {
        return lines;
    };
    for (number, tab) in workspace.tabs.iter().enumerate() {
        let name = if tab.name.is_empty() {
            format!("tab {}", number + 1)
        } else {
            format!("tab {} {}", number + 1, tab.name)
        };
        lines.push(Line {
            label: name,
            kind: "tab".to_owned(),
            choice: Choice::Tab(tab.tab),
        });
    }
    for tab in &workspace.tabs {
        for leaf in tab.layout.leaves() {
            lines.push(Line {
                label: scene.title(leaf.pane.raw()),
                kind: "pane".to_owned(),
                choice: Choice::Leaf(leaf.id),
            });
        }
    }
    lines
}
