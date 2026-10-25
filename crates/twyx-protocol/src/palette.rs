//! The command palette and the workspace switcher both clients show: what
//! they offer, how typing narrows it, and what a choice sends.

use twyx_core::keys::{Chord, Command, Key, Keymap};
use twyx_core::layout::{Leaf, LeafId};
use twyx_core::palette::{Found, search};

use crate::client::{Overview, ToDaemon, WorkspaceInfo};
use crate::sidebar::Target;

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
    /// Open a file of the shown workspace in Neovim.
    File(String),
}

impl Choice {
    /// The request a choice sends; a command runs in the client instead.
    pub fn request(&self) -> Option<ToDaemon> {
        Some(match self {
            Choice::Command(_) => return None,
            Choice::Tab(tab) => ToDaemon::SelectTab { tab: *tab },
            Choice::Leaf(leaf) => ToDaemon::FocusLeaf { leaf: *leaf },
            Choice::Pane(pane) => ToDaemon::ShowPane { pane: *pane },
            Choice::Go(Target::Workspace(workspace)) => ToDaemon::SwitchWorkspace {
                workspace: *workspace,
            },
            Choice::Go(Target::Open(path)) => ToDaemon::Open {
                path: path.clone(),
                switch: true,
            },
            Choice::Go(Target::Pane(leaf)) => ToDaemon::FocusLeaf { leaf: *leaf },
            Choice::File(path) => ToDaemon::OpenInEditor {
                path: path.clone(),
                line: None,
            },
        })
    }
}

/// One line of the palette, before the query narrows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    /// What the entry is, or the keys of a command.
    pub kind: String,
    pub choice: Choice,
}

/// What a picker offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// Everything.
    #[default]
    Palette,
    /// The projects, worktrees and workspaces alone.
    Switcher,
    /// The files of the shown workspace alone: quick open.
    Files,
}

impl Scope {
    pub fn title(self) -> &'static str {
        match self {
            Scope::Palette => "palette",
            Scope::Switcher => "workspaces",
            Scope::Files => "open file",
        }
    }
}

/// The palette, the switcher or quick open while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Picker {
    pub query: String,
    pub selected: usize,
    pub scope: Scope,
}

/// What one key in the palette comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Stay,
    Close,
    Choose(Choice),
}

impl Picker {
    pub fn palette() -> Self {
        Self::default()
    }

    pub fn switcher() -> Self {
        Self {
            scope: Scope::Switcher,
            ..Self::default()
        }
    }

    pub fn files() -> Self {
        Self {
            scope: Scope::Files,
            ..Self::default()
        }
    }

    /// The entries the query keeps, best first.
    pub fn found(&self, entries: &[Entry]) -> Vec<Found> {
        search(
            entries.iter().map(|entry| entry.label.as_str()),
            &self.query,
        )
    }

    /// Typing edits the query, up and down move the choice, enter takes
    /// it, and esc closes.
    pub fn key(&mut self, chord: Chord, entries: &[Entry]) -> Pick {
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        let found = self.found(entries);
        match (chord.key, typed) {
            (Key::Escape, _) => Pick::Close,
            (Key::Enter, _) => found.get(self.selected).map_or(Pick::Close, |found| {
                Pick::Choose(entries[found.index].choice.clone())
            }),
            (Key::Up, _) => {
                self.selected = self.selected.saturating_sub(1);
                Pick::Stay
            }
            (Key::Down, _) => {
                self.selected = (self.selected + 1).min(found.len().saturating_sub(1));
                Pick::Stay
            }
            (Key::Backspace, _) => {
                self.query.pop();
                self.selected = 0;
                Pick::Stay
            }
            (_, Some(ch)) => {
                self.query.push(ch);
                self.selected = 0;
                Pick::Stay
            }
            _ => Pick::Stay,
        }
    }

    /// Pasted text goes into the query.
    pub fn paste(&mut self, text: &str) {
        self.query.push_str(text);
        self.selected = 0;
    }
}

/// Everything the palette offers: the commands with their keys, the tabs
/// of this workspace, the projects, worktrees and workspaces, the panes
/// and agent sessions of every workspace, and `files`. The switcher offers
/// the projects, worktrees and workspaces alone, quick open the files.
pub fn entries(
    keymap: &Keymap,
    workspace: Option<&WorkspaceInfo>,
    overview: Option<&Overview>,
    leaf_title: impl Fn(&Leaf) -> String,
    pane_title: impl Fn(u64) -> String,
    files: &[String],
    scope: Scope,
) -> Vec<Entry> {
    let file_entries = || {
        files.iter().map(|path| Entry {
            label: path.clone(),
            kind: "file".to_owned(),
            choice: Choice::File(path.clone()),
        })
    };
    if scope == Scope::Files {
        return file_entries().collect();
    }
    let switcher = scope == Scope::Switcher;
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
                .map(|command| Entry {
                    label: command.name().replace('-', " "),
                    kind: keymap
                        .keys_for(command)
                        .unwrap_or_else(|| "command".to_owned()),
                    choice: Choice::Command(command),
                }),
        );
        if let Some(workspace) = workspace {
            for (number, tab) in workspace.tabs.iter().enumerate() {
                let name = if tab.name.is_empty() {
                    format!("tab {}", number + 1)
                } else {
                    format!("tab {} {}", number + 1, tab.name)
                };
                lines.push(Entry {
                    label: name,
                    kind: "tab".to_owned(),
                    choice: Choice::Tab(tab.tab),
                });
            }
        }
    }
    let Some(overview) = overview else {
        // Before the first overview, this workspace's panes.
        if let (false, Some(workspace)) = (switcher, workspace) {
            for tab in &workspace.tabs {
                for leaf in tab.layout.leaves() {
                    lines.push(Entry {
                        label: leaf_title(&leaf),
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
            lines.push(Entry {
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
        lines.push(Entry {
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
            lines.push(Entry {
                label: format!("{} · {}", pane_title(pane.pane), summary.name),
                kind: "pane".to_owned(),
                choice: Choice::Leaf(pane.leaf),
            });
            if let Some(agent) = &pane.agent {
                let state = pane.mark.map_or("", |mark| mark.word());
                lines.push(Entry {
                    label: format!("{agent} {state} · {}", summary.name),
                    kind: "agent session".to_owned(),
                    choice: Choice::Leaf(pane.leaf),
                });
            }
        }
    }
    lines.extend(file_entries());
    lines
}
