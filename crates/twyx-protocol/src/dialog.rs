//! The questions both clients put to the user: a line of text, a form, or
//! lines to read with the keys that answer them. Each client draws them in
//! its own style; the keys work the same in both.

use twyx_core::keys::{Chord, Key};

/// What a prompt or a confirmation is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    NewWorktree {
        project: u64,
    },
    RemoveWorktree {
        plan: u64,
    },
    TrustSetup {
        worktree: u64,
        hash: String,
    },
    /// Resume the focused pane's agent session, or all of the workspace's.
    Resume {
        this: u64,
        all: Vec<u64>,
    },
    Integrations,
    /// Start a task in this workspace's repository.
    NewTask {
        workspace: u64,
    },
    /// A review note's text, for these lines.
    Note {
        worktree: u64,
        path: String,
        lines: (u32, u32),
    },
    /// A commit message for the worktree's staged changes.
    Commit {
        worktree: u64,
    },
    /// Push the worktree's branch.
    Push {
        worktree: u64,
    },
    /// A pull request's title and body.
    PullRequest {
        worktree: u64,
    },
    /// Trust a worktree's repository check file at this hash.
    TrustChecks {
        worktree: u64,
        hash: String,
    },
    /// Send a worktree's open notes to one of these agent panes, by digit.
    SendNotes {
        worktree: u64,
        panes: Vec<u64>,
    },
}

impl Purpose {
    /// The line a form shows above its fields.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Purpose::NewTask { .. } => Some(
                "Each worktree starts from the ref with its agent and the prompt. Agents: kinds or shell, comma separated.",
            ),
            _ => None,
        }
    }

    /// What enter does in a form.
    pub fn submit(&self) -> &'static str {
        match self {
            Purpose::NewTask { .. } => "start",
            _ => "create",
        }
    }
}

/// A question that takes every key while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// One line of text, such as a branch name.
    Prompt {
        title: String,
        text: String,
        purpose: Purpose,
    },
    /// Fields to fill: label and value. Tab and the arrows move, enter
    /// submits, esc declines.
    Form {
        title: String,
        fields: Vec<(String, String)>,
        at: usize,
        purpose: Purpose,
    },
    /// Lines to read and the keys that answer them; esc declines.
    Confirm {
        title: String,
        lines: Vec<String>,
        keys: Vec<(char, String)>,
        purpose: Purpose,
    },
}

/// What one key in a dialog comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Stay,
    Close,
    Prompted {
        purpose: Purpose,
        text: String,
    },
    /// A form's values, in field order.
    Submitted {
        purpose: Purpose,
        values: Vec<String>,
    },
    /// A key the confirmation offered.
    Confirmed {
        purpose: Purpose,
        key: char,
    },
}

impl Dialog {
    pub fn key(&mut self, chord: Chord) -> Answer {
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        match self {
            Dialog::Prompt { text, purpose, .. } => match (chord.key, typed) {
                (Key::Escape, _) => Answer::Close,
                (Key::Enter, _) if !text.trim().is_empty() => Answer::Prompted {
                    purpose: purpose.clone(),
                    text: text.trim().to_owned(),
                },
                (Key::Backspace, _) => {
                    text.pop();
                    Answer::Stay
                }
                (_, Some(ch)) => {
                    text.push(ch);
                    Answer::Stay
                }
                _ => Answer::Stay,
            },
            Dialog::Confirm { keys, purpose, .. } => match (chord.key, typed) {
                (Key::Escape, _) => Answer::Close,
                (_, Some(ch)) if keys.iter().any(|(key, _)| *key == ch) => Answer::Confirmed {
                    purpose: purpose.clone(),
                    key: ch,
                },
                _ => Answer::Stay,
            },
            Dialog::Form {
                fields,
                at,
                purpose,
                ..
            } => match (chord.key, typed) {
                (Key::Escape, _) => Answer::Close,
                (Key::Enter, _) => Answer::Submitted {
                    purpose: purpose.clone(),
                    values: fields
                        .iter()
                        .map(|(_, value)| value.trim().to_owned())
                        .collect(),
                },
                (Key::Tab, _) if chord.shift => {
                    *at = (*at + fields.len() - 1) % fields.len().max(1);
                    Answer::Stay
                }
                (Key::Tab | Key::Down, _) => {
                    *at = (*at + 1) % fields.len().max(1);
                    Answer::Stay
                }
                (Key::Up, _) => {
                    *at = (*at + fields.len() - 1) % fields.len().max(1);
                    Answer::Stay
                }
                (Key::Backspace, _) => {
                    if let Some((_, value)) = fields.get_mut(*at) {
                        value.pop();
                    }
                    Answer::Stay
                }
                (_, Some(ch)) => {
                    if let Some((_, value)) = fields.get_mut(*at) {
                        value.push(ch);
                    }
                    Answer::Stay
                }
                _ => Answer::Stay,
            },
        }
    }

    /// Pasted text goes where typing would.
    pub fn paste(&mut self, pasted: &str) {
        match self {
            Dialog::Prompt { text, .. } => text.push_str(pasted),
            Dialog::Form { fields, at, .. } => {
                if let Some((_, value)) = fields.get_mut(*at) {
                    value.push_str(pasted);
                }
            }
            Dialog::Confirm { .. } => {}
        }
    }
}
