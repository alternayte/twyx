//! The review leaf both clients share: what each worktree's review holds,
//! where the user is in each leaf, and what a key in a leaf asks for.

use std::collections::HashMap;
use std::time::Instant;

use twyx_core::diff::LineKind;
use twyx_core::keys::{Chord, Key};
use twyx_core::layout::LeafId;

use crate::client::{GitState, NoteInfo, Overview, PaneInfo, ReviewInfo, ToDaemon};
use crate::dialog::{Dialog, Purpose};

/// Where the user is in one review leaf.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub file: usize,
    pub hunk: usize,
    /// Whether to show the other layout than the width chooses: two sides
    /// when the diff is wide enough, else one.
    pub flip: bool,
}

/// What a key in a review leaf asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(ToDaemon),
    /// Ask for a note's text, for these lines of this file.
    Note {
        path: String,
        lines: (u32, u32),
    },
    /// Choose the agent pane that gets the open notes.
    SendNotes,
    /// Ask for a commit message.
    Commit,
    /// Ask before pushing the branch.
    Push,
    /// Ask for a pull request's title and body.
    PullRequest,
}

impl Action {
    /// The question an action asks before it does anything; `None` for a
    /// request, and for sending notes, which needs the agent panes.
    pub fn dialog(&self, worktree: u64) -> Option<Dialog> {
        Some(match self {
            Action::Send(_) | Action::SendNotes => return None,
            Action::Note { path, lines } => Dialog::Prompt {
                title: format!("note on {path} lines {}-{}", lines.0, lines.1),
                text: String::new(),
                purpose: Purpose::Note {
                    worktree,
                    path: path.clone(),
                    lines: *lines,
                },
            },
            Action::Commit => Dialog::Prompt {
                title: "commit message".to_owned(),
                text: String::new(),
                purpose: Purpose::Commit { worktree },
            },
            Action::Push => Dialog::Confirm {
                title: "push".to_owned(),
                lines: vec![
                    "Push this worktree's branch to its remote. The first push sets the upstream."
                        .to_owned(),
                ],
                keys: vec![('y', "push".to_owned())],
                purpose: Purpose::Push { worktree },
            },
            Action::PullRequest => Dialog::Form {
                title: "pull request".to_owned(),
                fields: vec![
                    ("title".to_owned(), String::new()),
                    ("body".to_owned(), String::new()),
                ],
                at: 0,
                purpose: Purpose::PullRequest { worktree },
            },
        })
    }
}

/// The choice of agent pane for a worktree's open notes: the first nine
/// agent panes of `panes`, or `None` when no agent runs there.
pub fn notes_target<'a>(
    worktree: u64,
    panes: impl IntoIterator<Item = &'a PaneInfo>,
) -> Option<Dialog> {
    let agents: Vec<&PaneInfo> = panes
        .into_iter()
        .filter(|info| info.agent.is_some())
        .take(9)
        .collect();
    if agents.is_empty() {
        return None;
    }
    let lines = agents
        .iter()
        .enumerate()
        .map(|(number, info)| {
            let kind = info.agent.as_ref().map_or("", |agent| agent.kind.as_str());
            format!("{} {kind} · {}", number + 1, info.title)
        })
        .collect();
    let keys = (1..=agents.len())
        .map(|number| (char::from(b'0' + number as u8), format!("pane {number}")))
        .collect();
    Some(Dialog::Confirm {
        title: "send the open notes to".to_owned(),
        lines,
        keys,
        purpose: Purpose::SendNotes {
            worktree,
            panes: agents.iter().map(|info| info.pane).collect(),
        },
    })
}

/// The keys of a focused review leaf, for its key line: two rows of key
/// and word.
pub const KEYS: [&[(&str, &str)]; 2] = [
    &[
        ("j k", "file"),
        ("n p", "hunk"),
        ("space", "mark hunk"),
        ("f", "mark file"),
        ("s", "sides"),
        ("c", "note"),
        ("S", "send notes"),
        ("r", "reload"),
    ],
    &[
        ("a A", "stage hunk, file"),
        ("u U", "unstage hunk, file"),
        ("+ -", "stage, unstage all"),
        ("R", "stage reviewed"),
        ("C", "commit"),
        ("P", "push"),
        ("O", "pull request"),
    ],
];

#[derive(Debug, Default)]
pub struct Reviews {
    pub infos: HashMap<u64, ReviewInfo>,
    pub notes: HashMap<u64, Vec<NoteInfo>>,
    pub cursors: HashMap<LeafId, Cursor>,
    /// Each asked worktree's Git state when it was asked for, and when.
    asked: HashMap<u64, (Option<GitState>, Instant)>,
}

impl Reviews {
    /// Requests for the reviews that `shown` leaves need: not asked for
    /// yet, asked for before the worktree's Git state last changed, or
    /// asked for a refresh interval ago.
    pub fn wanted(&mut self, shown: &[u64], overview: Option<&Overview>) -> Vec<ToDaemon> {
        let mut requests = Vec::new();
        for worktree in shown {
            let git = overview
                .iter()
                .flat_map(|overview| &overview.projects)
                .flat_map(|project| &project.worktrees)
                .find(|each| each.worktree == *worktree)
                .and_then(|each| each.git.clone());
            let fresh = self.asked.get(worktree).is_some_and(|(seen, at)| {
                *seen == git && at.elapsed() < twyx_core::tuning::REVIEW_REFRESH
            });
            if !fresh {
                self.asked.insert(*worktree, (git, Instant::now()));
                requests.push(ToDaemon::Review {
                    worktree: *worktree,
                });
                requests.push(ToDaemon::Notes {
                    worktree: *worktree,
                });
            }
        }
        requests
    }

    pub fn answer(&mut self, info: ReviewInfo) {
        self.infos.insert(info.worktree, info);
    }

    /// The cursor of a leaf, kept inside the worktree's files.
    pub fn cursor(&self, leaf: LeafId, worktree: u64) -> Cursor {
        let mut cursor = self.cursors.get(&leaf).copied().unwrap_or_default();
        let files = self.infos.get(&worktree).map_or(0, |info| info.files.len());
        cursor.file = cursor.file.min(files.saturating_sub(1));
        cursor
    }

    /// Takes a key for the review leaf `leaf` of `worktree`.
    pub fn key(&mut self, leaf: LeafId, worktree: u64, chord: Chord) -> Option<Action> {
        let info = self.infos.get(&worktree)?;
        let cursor = self.cursors.entry(leaf).or_default();
        let files = &info.files;
        cursor.file = cursor.file.min(files.len().saturating_sub(1));
        let hunks = files
            .get(cursor.file)
            .and_then(|file| file.diff.as_ref())
            .map_or(0, |diff| diff.hunks.len());
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        match (chord.key, typed) {
            (Key::Down, _) | (_, Some('j')) => {
                cursor.file = (cursor.file + 1).min(files.len().saturating_sub(1));
                cursor.hunk = 0;
            }
            (Key::Up, _) | (_, Some('k')) => {
                cursor.file = cursor.file.saturating_sub(1);
                cursor.hunk = 0;
            }
            (_, Some('n')) => cursor.hunk = (cursor.hunk + 1).min(hunks.saturating_sub(1)),
            (_, Some('p')) => cursor.hunk = cursor.hunk.saturating_sub(1),
            (_, Some('s')) => cursor.flip = !cursor.flip,
            (_, Some('r')) => return Some(Action::Send(ToDaemon::Review { worktree })),
            (Key::Enter, _) | (_, Some(' ')) => {
                let file = files.get(cursor.file)?;
                let hunk = file.diff.as_ref()?.hunks.get(cursor.hunk)?;
                return Some(Action::Send(ToDaemon::MarkReviewed {
                    worktree,
                    path: file.path.clone(),
                    hunks: Some(vec![hunk.id()]),
                }));
            }
            (_, Some('f')) => {
                let file = files.get(cursor.file).filter(|file| file.diff.is_some())?;
                return Some(Action::Send(ToDaemon::MarkReviewed {
                    worktree,
                    path: file.path.clone(),
                    hunks: None,
                }));
            }
            (_, Some('c')) => {
                let file = files.get(cursor.file)?;
                let hunk = file.diff.as_ref()?.hunks.get(cursor.hunk)?;
                let count = hunk
                    .lines
                    .iter()
                    .filter(|line| matches!(line.kind, LineKind::Context | LineKind::Added))
                    .count() as u32;
                let first = hunk.new_start.max(1);
                return Some(Action::Note {
                    path: file.path.clone(),
                    lines: (first, first + count.max(1) - 1),
                });
            }
            (_, Some('S')) => return Some(Action::SendNotes),
            (_, Some(key @ ('a' | 'u' | 'A' | 'U'))) => {
                let file = files.get(cursor.file)?;
                let hunk = file
                    .diff
                    .as_ref()
                    .and_then(|diff| diff.hunks.get(cursor.hunk))
                    .filter(|_| key.is_lowercase());
                return Some(Action::Send(ToDaemon::Stage {
                    worktree,
                    path: Some(file.path.clone()),
                    hunks: hunk.map(|hunk| vec![hunk.id()]),
                    unstage: key.eq_ignore_ascii_case(&'u'),
                }));
            }
            (_, Some(key @ ('+' | '-'))) => {
                return Some(Action::Send(ToDaemon::Stage {
                    worktree,
                    path: None,
                    hunks: None,
                    unstage: key == '-',
                }));
            }
            (_, Some('R')) => return Some(Action::Send(ToDaemon::StageReviewed { worktree })),
            (_, Some('C')) => return Some(Action::Commit),
            (_, Some('P')) => return Some(Action::Push),
            (_, Some('O')) => return Some(Action::PullRequest),
            _ => {}
        }
        None
    }

    /// Chooses a file of a leaf with the mouse.
    pub fn choose_file(&mut self, leaf: LeafId, file: usize) {
        let cursor = self.cursors.entry(leaf).or_default();
        if cursor.file != file {
            cursor.file = file;
            cursor.hunk = 0;
        }
    }

    /// Chooses a hunk of a leaf's file with the mouse.
    pub fn choose_hunk(&mut self, leaf: LeafId, hunk: usize) {
        self.cursors.entry(leaf).or_default().hunk = hunk;
    }
}

/// One row of a diff shown on two sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideRow<'a> {
    /// A context line on both sides, with its old and new line numbers.
    Same(u32, u32, &'a str),
    /// A removed line and the added line beside it; either may be missing.
    Pair(Option<(u32, &'a str)>, Option<(u32, &'a str)>),
}

/// A hunk's lines as two sides: each run of removed lines beside the run of
/// added lines that follows it.
pub fn sides(hunk: &twyx_core::diff::Hunk) -> Vec<SideRow<'_>> {
    fn flush<'a>(
        rows: &mut Vec<SideRow<'a>>,
        removed: &mut Vec<(u32, &'a str)>,
        added: &mut Vec<(u32, &'a str)>,
    ) {
        for at in 0..removed.len().max(added.len()) {
            rows.push(SideRow::Pair(
                removed.get(at).copied(),
                added.get(at).copied(),
            ));
        }
        removed.clear();
        added.clear();
    }
    let mut rows = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for (line, (old, new)) in hunk.lines.iter().zip(hunk.numbers()) {
        match line.kind {
            LineKind::Removed => removed.push((old.unwrap_or(0), line.text.as_str())),
            LineKind::Added => added.push((new.unwrap_or(0), line.text.as_str())),
            LineKind::Context => {
                flush(&mut rows, &mut removed, &mut added);
                rows.push(SideRow::Same(
                    old.unwrap_or(0),
                    new.unwrap_or(0),
                    &line.text,
                ));
            }
            LineKind::NoNewline => {}
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{IndexState, ReviewFile, Reviewed};

    fn info() -> ReviewInfo {
        let diff = twyx_core::diff::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-one\n+ONE\n two\n@@ -9,1 +9,2 @@\n nine\n+ten\n",
        );
        ReviewInfo {
            worktree: 3,
            base_ref: "refs/heads/main".to_owned(),
            base: "abc".to_owned(),
            files: vec![
                ReviewFile {
                    path: "a.txt".to_owned(),
                    reviewed: Reviewed::Partly,
                    diff: diff.into_iter().next(),
                    index: IndexState::Unstaged,
                },
                ReviewFile {
                    path: "b.txt".to_owned(),
                    reviewed: Reviewed::Yes,
                    diff: None,
                    index: IndexState::Committed,
                },
            ],
            error: None,
        }
    }

    #[test]
    fn keys_move_through_hunks_and_mark_the_chosen_one() {
        let mut reviews = Reviews::default();
        assert_eq!(
            reviews.wanted(&[3, 3], None).len(),
            2,
            "the review and its notes, asked once"
        );
        reviews.answer(info());
        let leaf = LeafId(1);
        let key = |ch| Chord::plain(Key::Char(ch));
        assert_eq!(reviews.key(leaf, 3, key('n')), None);
        let second = info().files[0].diff.as_ref().unwrap().hunks[1].id();
        assert_eq!(
            reviews.key(leaf, 3, key(' ')),
            Some(Action::Send(ToDaemon::MarkReviewed {
                worktree: 3,
                path: "a.txt".to_owned(),
                hunks: Some(vec![second]),
            }))
        );
        assert_eq!(
            reviews.key(leaf, 3, key('c')),
            Some(Action::Note {
                path: "a.txt".to_owned(),
                lines: (9, 10),
            }),
            "a note covers the hunk's lines in the file as it is now"
        );
        reviews.key(leaf, 3, key('j'));
        assert_eq!(
            reviews.key(leaf, 3, key('f')),
            None,
            "a reviewed file has nothing to mark"
        );
        assert_eq!(
            reviews.key(leaf, 3, key('r')),
            Some(Action::Send(ToDaemon::Review { worktree: 3 }))
        );
    }

    #[test]
    fn two_sides_pair_removed_lines_with_the_added_lines_after_them() {
        let diff = twyx_core::diff::parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n keep\n-old\n+new\n+more\n",
        );
        let hunk = &diff[0].hunks[0];
        assert_eq!(
            sides(hunk),
            vec![
                SideRow::Same(1, 1, "keep"),
                SideRow::Pair(Some((2, "old")), Some((2, "new"))),
                SideRow::Pair(None, Some((3, "more"))),
            ]
        );
    }
}
