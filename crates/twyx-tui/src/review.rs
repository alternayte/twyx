//! The review view: a worktree's changed files and the unreviewed hunks of
//! the chosen file, unified or side by side, with keys to move and to mark
//! hunks and files reviewed.
//!
//! An added line is in a muted green, a removed line in a muted red and
//! context in ink. The `+` and `-` column says which is which without colour.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::diff::{FileDiff, LineKind};
use twyx_core::theme::{RULES, SPACE, Tone};
use twyx_protocol::client::{IndexState, NoteInfo, NoteState, ReviewInfo, Reviewed};
pub use twyx_protocol::review::{Action, Cursor, Reviews};
use twyx_protocol::review::{KEYS, SideRow, sides};

use crate::chrome::Pen;
use crate::chrome::bars::put;

/// The narrowest diff column that shows two sides.
const SPLIT_WIDTH: u16 = 120;
/// The narrowest view that shows the file list beside the diff.
const LIST_WIDTH: u16 = 60;

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
        for (row, keys) in KEYS.iter().enumerate().take(usize::from(keys_rows)) {
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
                for row in sides(hunk) {
                    rows.push((
                        number,
                        match row {
                            SideRow::Same(_, _, text) => Row::Same(text.to_owned()),
                            SideRow::Pair(old, new) => Row::Pair(
                                old.map(|(_, text)| text.to_owned()),
                                new.map(|(_, text)| text.to_owned()),
                            ),
                        },
                    ));
                }
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
            let added = pen.fg(Tone::Rgb(pen.theme.added));
            let removed = pen.fg(Tone::Rgb(pen.theme.removed));
            match row {
                Row::Header(header) => {
                    put(buf, x, y, area.right(), header, pen.rule());
                }
                Row::Line(kind, text) => {
                    let (mark, style) = match kind {
                        LineKind::Added => ("+", added),
                        LineKind::Removed => ("-", removed),
                        LineKind::Context => (" ", ink),
                        LineKind::NoNewline => ("\\", pen.rule()),
                    };
                    let after = put(buf, x, y, area.right(), mark, style);
                    put(buf, after, y, area.right(), text, style);
                }
                Row::Same(text) => {
                    put(buf, x + 1, y, x + 1 + half, text, ink);
                    buf[(x + 1 + half, y)]
                        .set_char(RULES.vertical)
                        .set_style(pen.rule());
                    put(buf, x + 3 + half, y, area.right(), text, ink);
                }
                Row::Pair(old, new) => {
                    if let Some(old) = old {
                        let after = put(buf, x, y, x + 1 + half, "-", removed);
                        put(buf, after, y, x + 1 + half, old, removed);
                    }
                    buf[(x + 1 + half, y)]
                        .set_char(RULES.vertical)
                        .set_style(pen.rule());
                    if let Some(new) = new {
                        let after = put(buf, x + 1 + half + 1, y, area.right(), "+", added);
                        put(buf, after, y, area.right(), new, added);
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
