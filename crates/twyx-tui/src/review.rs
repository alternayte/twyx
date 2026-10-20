//! The review view: a worktree's changed files and the unreviewed hunks of
//! the chosen file, unified or side by side, with keys to move and to mark
//! hunks and files reviewed.
//!
//! The diff carries no hues: the TUI keeps colour for agent states. An added
//! line is in ink, a removed line and context in grey, and the `+` and `-`
//! column says which is which.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::diff::{FileDiff, LineKind};
use twyx_core::keys::{Chord, Key};
use twyx_core::layout::LeafId;
use twyx_core::theme::{RULES, SPACE, Tone};
use twyx_protocol::client::{
    GitState, IndexState, NoteInfo, NoteState, Overview, ReviewInfo, Reviewed, ToDaemon,
};

use crate::chrome::Pen;
use crate::chrome::bars::put;

/// The narrowest diff column that shows two sides.
const SPLIT_WIDTH: u16 = 120;
/// The narrowest view that shows the file list beside the diff.
const LIST_WIDTH: u16 = 60;

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
}

/// One review leaf's content.
#[derive(Debug)]
pub struct ReviewView<'a> {
    pub pen: &'a Pen,
    pub info: Option<&'a ReviewInfo>,
    pub notes: &'a [NoteInfo],
    pub cursor: Cursor,
    pub focused: bool,
}

impl Widget for ReviewView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height < 3 {
            return;
        }
        let pen = self.pen;
        let kind = pen.theme.type_;
        buf.set_style(area, pen.paper());
        let left = area.x + SPACE.pad;
        let right = area.right().saturating_sub(SPACE.pad);
        let Some(info) = self.info else {
            put(
                buf,
                left,
                area.y,
                right,
                "reading the review",
                pen.face(kind.caption),
            );
            return;
        };
        if let Some(error) = &info.error {
            put(buf, left, area.y, right, error, pen.face(kind.label));
            if info.files.is_empty() {
                return;
            }
        }
        let open = info
            .files
            .iter()
            .filter(|file| file.reviewed != Reviewed::Yes)
            .count();
        let base: String = info.base.chars().take(8).collect();
        let branch = info.base_ref.rsplit('/').next().unwrap_or_default();
        let summary = format!(
            "base {base} · {branch} · {open} to review · {} reviewed",
            info.files.len() - open
        );
        let y = area.y + u16::from(info.error.is_some());
        put(buf, left, y, right, &summary, pen.face(kind.caption));
        // The keys, on the last rows, while the view has the focus.
        let keys_rows = if self.focused && area.height > 8 {
            2
        } else {
            0
        };
        let rows: [&[(&str, &str)]; 2] = [
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
        for (row, keys) in rows.iter().enumerate().take(usize::from(keys_rows)) {
            let mut x = left;
            let bottom = area.bottom() - keys_rows + row as u16;
            for (key, word) in keys.iter().copied() {
                x = put(buf, x, bottom, right, key, pen.face(kind.key));
                x = put(buf, x, bottom, right, " ", pen.paper());
                x = put(
                    buf,
                    x,
                    bottom,
                    right,
                    word,
                    pen.fg(Tone::Rgb(pen.theme.graphite)),
                );
                x = put(buf, x, bottom, right, "   ", pen.paper());
            }
        }
        let body = Rect {
            x: area.x,
            y: y + 2,
            width: area.width,
            height: area.bottom().saturating_sub(y + 2 + keys_rows),
        };
        if info.files.is_empty() {
            put(
                buf,
                left,
                body.y,
                right,
                "nothing changed since the base",
                pen.face(kind.caption),
            );
            return;
        }
        let selected = self.cursor.file.min(info.files.len() - 1);
        let diff_area = if area.width >= LIST_WIDTH {
            let list_width = (area.width / 3).clamp(20, 40);
            self.list(
                info,
                selected,
                Rect {
                    width: list_width,
                    ..body
                },
                buf,
            );
            for y in body.top()..body.bottom() {
                buf[(body.x + list_width, y)]
                    .set_char(RULES.vertical)
                    .set_style(pen.rule());
            }
            Rect {
                x: body.x + list_width + 1,
                width: body.width - list_width - 1,
                ..body
            }
        } else {
            let file = &info.files[selected];
            let line = format!("{}/{} {}", selected + 1, info.files.len(), file.path);
            put(buf, left, body.y, right, &line, pen.face(kind.label));
            Rect {
                y: body.y + 1,
                height: body.height.saturating_sub(1),
                ..body
            }
        };
        let file = &info.files[selected];
        // The file's notes, above its diff.
        let notes: Vec<&NoteInfo> = self
            .notes
            .iter()
            .filter(|note| note.path == file.path)
            .collect();
        let shown = notes.len().min(usize::from(diff_area.height / 3));
        for (row, note) in notes.iter().take(shown).enumerate() {
            let state = match note.state {
                NoteState::Open => "",
                NoteState::Sent => " (sent)",
                NoteState::Outdated => " (outdated)",
            };
            let text = format!("✎ {}-{}{state}: {}", note.lines.0, note.lines.1, note.body);
            put(
                buf,
                diff_area.x + 1,
                diff_area.y + row as u16,
                diff_area.right(),
                &text,
                pen.face(kind.label),
            );
        }
        let diff_area = Rect {
            y: diff_area.y + shown as u16,
            height: diff_area.height - shown as u16,
            ..diff_area
        };
        match (&file.diff, file.reviewed) {
            (Some(diff), _) => {
                let split = (diff_area.width >= SPLIT_WIDTH) != self.cursor.flip;
                self.diff(diff, split, diff_area, buf);
            }
            (None, Reviewed::TooLarge) => {
                put(
                    buf,
                    diff_area.x + SPACE.pad,
                    diff_area.y,
                    diff_area.right(),
                    "too large to review",
                    pen.face(kind.caption),
                );
            }
            (None, _) => {
                put(
                    buf,
                    diff_area.x + SPACE.pad,
                    diff_area.y,
                    diff_area.right(),
                    "reviewed",
                    pen.face(kind.caption),
                );
            }
        }
    }
}

impl ReviewView<'_> {
    fn list(&self, info: &ReviewInfo, selected: usize, area: Rect, buf: &mut Buffer) {
        let pen = self.pen;
        let kind = pen.theme.type_;
        let rows = usize::from(area.height).max(1);
        let first = (selected + 1).saturating_sub(rows);
        for (line, (index, file)) in info
            .files
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .enumerate()
        {
            let y = area.y + line as u16;
            let marker = if index == selected { "›" } else { " " };
            let marker_style = if self.focused {
                pen.accent()
            } else {
                pen.face(kind.caption)
            };
            let x = put(
                buf,
                area.x + SPACE.pad,
                y,
                area.right(),
                marker,
                marker_style,
            );
            let (glyph, face) = match file.reviewed {
                Reviewed::No => (" ", kind.label),
                Reviewed::Partly => ("~", kind.label),
                Reviewed::Yes => ("✓", kind.caption),
                Reviewed::TooLarge => ("!", kind.caption),
            };
            let x = put(buf, x + 1, y, area.right(), glyph, pen.face(face));
            // Staged on the main index: all of it, or some.
            let staged = match file.index {
                IndexState::Staged => "+",
                IndexState::Partly => "±",
                IndexState::Committed | IndexState::Unstaged => " ",
            };
            let x = put(buf, x, y, area.right(), staged, pen.face(kind.caption));
            put(buf, x + 1, y, area.right(), &file.path, pen.face(face));
        }
    }

    /// The hunks, scrolled so the chosen one starts in view.
    fn diff(&self, diff: &FileDiff, split: bool, area: Rect, buf: &mut Buffer) {
        let pen = self.pen;
        let kind = pen.theme.type_;
        let chosen = self.cursor.hunk.min(diff.hunks.len().saturating_sub(1));
        if diff.binary {
            put(
                buf,
                area.x + SPACE.pad,
                area.y,
                area.right(),
                "a binary file: f marks it reviewed",
                pen.face(kind.caption),
            );
            return;
        }
        // Rows: (hunk, left text and kind, right text and kind).
        let mut rows: Vec<(usize, Row)> = Vec::new();
        for (number, hunk) in diff.hunks.iter().enumerate() {
            rows.push((number, Row::Header(hunk.header.clone())));
            if split {
                let mut removed: Vec<String> = Vec::new();
                let mut added: Vec<String> = Vec::new();
                let flush = |rows: &mut Vec<(usize, Row)>,
                             removed: &mut Vec<String>,
                             added: &mut Vec<String>| {
                    for at in 0..removed.len().max(added.len()) {
                        rows.push((
                            number,
                            Row::Pair(removed.get(at).cloned(), added.get(at).cloned()),
                        ));
                    }
                    removed.clear();
                    added.clear();
                };
                for line in &hunk.lines {
                    match line.kind {
                        LineKind::Removed => removed.push(line.text.clone()),
                        LineKind::Added => added.push(line.text.clone()),
                        LineKind::Context => {
                            flush(&mut rows, &mut removed, &mut added);
                            rows.push((number, Row::Same(line.text.clone())));
                        }
                        LineKind::NoNewline => {}
                    }
                }
                flush(&mut rows, &mut removed, &mut added);
            } else {
                for line in &hunk.lines {
                    rows.push((number, Row::Line(line.kind, line.text.clone())));
                }
            }
        }
        let start = rows
            .iter()
            .position(|(hunk, _)| *hunk == chosen)
            .unwrap_or(0);
        let half = area.width.saturating_sub(3) / 2;
        for (line, (hunk, row)) in rows
            .iter()
            .skip(start)
            .take(usize::from(area.height))
            .enumerate()
        {
            let y = area.y + line as u16;
            let gutter = if *hunk == chosen && self.focused {
                pen.accent()
            } else {
                pen.rule()
            };
            buf[(area.x, y)]
                .set_char(if *hunk == chosen { '▌' } else { ' ' })
                .set_style(gutter);
            let x = area.x + 1;
            let ink = pen.fg(Tone::Ink);
            let grey = pen.fg(Tone::Rgb(pen.theme.graphite));
            match row {
                Row::Header(header) => {
                    put(buf, x, y, area.right(), header, pen.rule());
                }
                Row::Line(kind, text) => {
                    let (mark, style) = match kind {
                        LineKind::Added => ("+", ink),
                        LineKind::Removed => ("-", grey),
                        LineKind::Context => (" ", grey),
                        LineKind::NoNewline => ("\\", pen.rule()),
                    };
                    let after = put(buf, x, y, area.right(), mark, style);
                    put(buf, after, y, area.right(), text, style);
                }
                Row::Same(text) => {
                    put(buf, x + 1, y, x + 1 + half, text, grey);
                    buf[(x + 1 + half, y)]
                        .set_char(RULES.vertical)
                        .set_style(pen.rule());
                    put(buf, x + 3 + half, y, area.right(), text, grey);
                }
                Row::Pair(old, new) => {
                    if let Some(old) = old {
                        let after = put(buf, x, y, x + 1 + half, "-", grey);
                        put(buf, after, y, x + 1 + half, old, grey);
                    }
                    buf[(x + 1 + half, y)]
                        .set_char(RULES.vertical)
                        .set_style(pen.rule());
                    if let Some(new) = new {
                        let after = put(buf, x + 1 + half + 1, y, area.right(), "+", ink);
                        put(buf, after, y, area.right(), new, ink);
                    }
                }
            }
        }
    }
}

enum Row {
    Header(String),
    Line(LineKind, String),
    /// A context line on both sides.
    Same(String),
    /// A removed line and the added line beside it; either may be missing.
    Pair(Option<String>, Option<String>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use twyx_protocol::client::ReviewFile;

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
}
