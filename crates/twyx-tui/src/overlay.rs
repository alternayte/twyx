//! Overlays that take every key while they are open: the command palette,
//! the workspace switcher, the key sheet, the prompts and the confirmations.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::keys::{Chord, Command, Key};
use twyx_core::layout::LeafId;
use twyx_core::palette::search;

pub use twyx_protocol::dialog::Purpose;
use twyx_protocol::dialog::{Answer, Dialog};
use twyx_protocol::sidebar::Target;

use crate::chrome::dialog::Dialog as DialogBox;
use crate::chrome::keys::KeySheet;
use crate::chrome::palette::{Entry, Palette};
use crate::compositor::Scene;

/// What a palette entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Command(Command),
    Tab(u64),
    Leaf(LeafId),
    /// Show a pane: its leaf, or a new tab for a pane without one.
    Pane(u64),
    /// Show a workspace, or open a worktree that has none.
    Go(Target),
}

/// What one key in an overlay comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Choose(Choice),
    Rename {
        tab: u64,
        name: String,
    },
    Search(String),
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
    /// A key on a check: `r` runs it, `o` shows its last run, `x` cancels
    /// it, `t` asks to trust the repository check file.
    Check {
        worktree: u64,
        name: String,
        key: char,
    },
    /// A form's values, in field order.
    Submitted {
        purpose: Purpose,
        values: Vec<String>,
    },
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
    /// The palette over projects, worktrees and workspaces only.
    Switcher {
        query: String,
        selected: usize,
    },
    /// A prompt, a form or a confirmation.
    Dialog(Dialog),
    /// The inbox, newest first.
    Inbox {
        selected: usize,
    },
    /// Every bound key, by mode.
    Keys {
        scroll: usize,
    },
    /// A worktree's checks with their last results.
    Checks {
        worktree: u64,
        selected: usize,
    },
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

    pub fn switcher() -> Self {
        Overlay::Switcher {
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
            Overlay::Palette { .. } | Overlay::Switcher { .. } => {
                let switcher = matches!(self, Overlay::Switcher { .. });
                let (query, selected) = match self {
                    Overlay::Palette { query, selected }
                    | Overlay::Switcher { query, selected } => (query, selected),
                    _ => return Outcome::Stay,
                };
                let lines = lines(scene, switcher);
                let found = search(lines.iter().map(|line| line.label.as_str()), query);
                let count = found.len();
                match (chord.key, typed) {
                    (Key::Escape, _) => Outcome::Close,
                    (Key::Enter, _) => found.get(*selected).map_or(Outcome::Close, |found| {
                        Outcome::Choose(lines[found.index].choice.clone())
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
            Overlay::Dialog(dialog) => match dialog.key(chord) {
                Answer::Stay => Outcome::Stay,
                Answer::Close => Outcome::Close,
                Answer::Prompted { purpose, text } => Outcome::Prompted { purpose, text },
                Answer::Submitted { purpose, values } => Outcome::Submitted { purpose, values },
                Answer::Confirmed { purpose, key } => Outcome::Confirmed { purpose, key },
            },
            Overlay::Inbox { selected } => {
                let items = scene.inbox;
                *selected = (*selected).min(items.len().saturating_sub(1));
                let item = items.get(*selected);
                match (chord.key, typed) {
                    (Key::Escape, _) => Outcome::Close,
                    (Key::Up, _) | (_, Some('k')) => {
                        *selected = selected.saturating_sub(1);
                        Outcome::Stay
                    }
                    (Key::Down, _) | (_, Some('j')) => {
                        *selected = (*selected + 1).min(items.len().saturating_sub(1));
                        Outcome::Stay
                    }
                    (Key::Enter, _) => item.map_or(Outcome::Stay, |item| {
                        Outcome::Choose(match scene.leaf_of(item.pane) {
                            Some(leaf) => Choice::Leaf(leaf),
                            None => Choice::Pane(item.pane),
                        })
                    }),
                    (_, Some(key @ ('u' | 'x'))) => {
                        item.map_or(Outcome::Stay, |item| Outcome::Inbox {
                            item: item.item,
                            dismiss: key == 'x',
                        })
                    }
                    _ => Outcome::Stay,
                }
            }
            Overlay::Checks { worktree, selected } => {
                let checks = scene.checks_of(*worktree);
                *selected = (*selected).min(checks.len().saturating_sub(1));
                let name = checks.get(*selected).map(|entry| entry.check.name.clone());
                match (chord.key, typed) {
                    (Key::Escape, _) => Outcome::Close,
                    (Key::Up, _) | (_, Some('k')) => {
                        *selected = selected.saturating_sub(1);
                        Outcome::Stay
                    }
                    (Key::Down, _) | (_, Some('j')) => {
                        *selected = (*selected + 1).min(checks.len().saturating_sub(1));
                        Outcome::Stay
                    }
                    (Key::Enter, _) | (_, Some('r')) => {
                        name.map_or(Outcome::Stay, |name| Outcome::Check {
                            worktree: *worktree,
                            name,
                            key: 'r',
                        })
                    }
                    (_, Some(key @ ('o' | 'x' | 't'))) => {
                        name.map_or(Outcome::Stay, |name| Outcome::Check {
                            worktree: *worktree,
                            name,
                            key,
                        })
                    }
                    _ => Outcome::Stay,
                }
            }
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
            Overlay::Palette { query, selected } | Overlay::Switcher { query, selected } => {
                query.push_str(&text);
                *selected = 0;
            }
            Overlay::Rename { name, .. } => name.push_str(&text),
            Overlay::Dialog(dialog) => dialog.paste(&text),
            Overlay::Search(needle) => needle.push_str(&text),
            Overlay::Inbox { .. } | Overlay::Checks { .. } | Overlay::Keys { .. } => {}
        }
    }

    pub fn render(&self, scene: &Scene<'_>, area: Rect, buf: &mut Buffer) {
        match self {
            Overlay::Palette { query, selected } | Overlay::Switcher { query, selected } => {
                let switcher = matches!(self, Overlay::Switcher { .. });
                let lines = lines(scene, switcher);
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
                    title: if switcher { "workspaces" } else { "palette" },
                    query,
                    entries: &entries[skip.min(entries.len())..],
                    selected: selected - skip.min(*selected),
                }
                .render(area, buf);
            }
            Overlay::Inbox { selected } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() as u64);
                let lines: Vec<(String, String)> = scene
                    .inbox
                    .iter()
                    .map(|item| {
                        let place = match (&item.project, &item.worktree) {
                            (Some(project), Some(worktree)) => format!(" · {project} / {worktree}"),
                            (Some(project), None) => format!(" · {project}"),
                            (None, _) => String::new(),
                        };
                        let mark = item.mark.map_or(' ', |mark| mark.glyph());
                        let state = item.mark.map_or(item.reason.name(), |mark| mark.word());
                        let unread = if item.read { ' ' } else { '•' };
                        (
                            format!("{unread} {mark} {state}{place} · {}", item.title),
                            age(now.saturating_sub(item.created_ms)),
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
                    query: "enter jump  u unread  x dismiss",
                    entries: &entries[skip.min(entries.len())..],
                    selected: selected - skip.min(selected),
                }
                .render(area, buf);
            }
            Overlay::Checks { worktree, selected } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() as u64);
                let checks = scene.checks_of(*worktree);
                let width = checks
                    .iter()
                    .map(|entry| entry.check.name.chars().count())
                    .max()
                    .unwrap_or(0);
                let lines: Vec<(String, String)> = checks
                    .iter()
                    .map(|entry| {
                        let (result, when) = match &entry.run {
                            None => ("never run".to_owned(), String::new()),
                            Some(run) => match run.outcome {
                                None => ("running".to_owned(), String::new()),
                                Some(outcome) => (
                                    twyx_core::check::label(outcome, run.stale),
                                    age(now.saturating_sub(run.ended_ms.unwrap_or(now))),
                                ),
                            },
                        };
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
                let query = if untrusted {
                    "enter run  o output  x cancel  t trust the repository's checks"
                } else {
                    "enter run  o output  x cancel"
                };
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

/// An age in the largest whole unit.
fn age(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Everything the palette offers: the commands with their keys, the tabs
/// of this workspace, the projects, worktrees and workspaces, and the panes
/// of every workspace. The switcher offers the projects, worktrees and
/// workspaces alone.
fn lines(scene: &Scene<'_>, switcher: bool) -> Vec<Line> {
    let mut lines = Vec::new();
    if !switcher {
        lines.extend(
            Command::every()
                .into_iter()
                .filter(|command| {
                    !matches!(
                        command,
                        Command::Enter(_) | Command::GoToTab(_) | Command::Palette
                    )
                })
                // Every command stays in reach of the keyboard, bound or not;
                // copy motions act only inside copy mode.
                .filter(|command| !matches!(command, Command::CopyMove(_)))
                .map(|command| Line {
                    label: command.name().replace('-', " "),
                    kind: scene
                        .keys_for(command)
                        .unwrap_or_else(|| "command".to_owned()),
                    choice: Choice::Command(command),
                }),
        );
        if let Some(workspace) = scene.workspace {
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
        }
    }
    let Some(overview) = scene.overview else {
        // Before the first overview, this workspace's panes.
        if let (false, Some(workspace)) = (switcher, scene.workspace) {
            for tab in &workspace.tabs {
                for leaf in tab.layout.leaves() {
                    lines.push(Line {
                        label: scene.leaf_title(&leaf),
                        kind: "pane".to_owned(),
                        choice: Choice::Leaf(leaf.id),
                    });
                }
            }
        }
        return lines;
    };
    for project in &overview.projects {
        if !project.git {
            continue;
        }
        for worktree in &project.worktrees {
            let target = match worktree.workspace {
                Some(workspace) => Target::Workspace(workspace),
                None => Target::Open(worktree.path.clone()),
            };
            lines.push(Line {
                label: format!("{} / {}", project.name, worktree.name),
                kind: "worktree".to_owned(),
                choice: Choice::Go(target),
            });
        }
    }
    for summary in &overview.workspaces {
        if summary.worktree.is_some() {
            continue;
        }
        let is_project = summary.project.is_some_and(|project| {
            overview
                .projects
                .iter()
                .any(|entry| entry.project == project && !entry.git)
        });
        lines.push(Line {
            label: summary.name.clone(),
            kind: if is_project { "project" } else { "workspace" }.to_owned(),
            choice: Choice::Go(Target::Workspace(summary.workspace)),
        });
    }
    if switcher {
        return lines;
    }
    for summary in &overview.workspaces {
        for pane in &summary.panes {
            lines.push(Line {
                label: format!("{} · {}", scene.title(pane.pane), summary.name),
                kind: "pane".to_owned(),
                choice: Choice::Leaf(pane.leaf),
            });
            if let Some(agent) = &pane.agent {
                let state = pane.mark.map_or("", |mark| mark.word());
                lines.push(Line {
                    label: format!("{agent} {state} · {}", summary.name),
                    kind: "agent session".to_owned(),
                    choice: Choice::Leaf(pane.leaf),
                });
            }
        }
    }
    lines
}
