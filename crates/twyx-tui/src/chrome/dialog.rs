//! A dialog: a floating sheet with a title on its top rule, lines of text,
//! and the keys that answer it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::theme::{SPACE, Tone};

use super::Pen;
use super::bars::put;
use super::frames::{Frame, Frames};

#[derive(Debug)]
pub struct Dialog<'a> {
    pub pen: &'a Pen,
    pub title: &'a str,
    pub lines: &'a [String],
    /// Key and what it does.
    pub keys: &'a [(String, String)],
}

/// The widest the dialog grows, in cells.
const WIDTH: u16 = 76;

/// Whether every row of `lines` shows in a dialog on a screen of at least
/// 80 by 24 cells; a trust prompt must show all it trusts.
pub fn fits(lines: &[String]) -> bool {
    let room = usize::from(WIDTH.saturating_sub(2 + 2 * SPACE.pad)).max(1);
    lines
        .iter()
        .map(|line| wrap(line, room).len())
        .sum::<usize>()
        <= FITS_ROWS
}

/// The rows a dialog shows on the smallest screen twyx counts on.
const FITS_ROWS: usize = 14;

/// Breaks `line` at spaces into rows of at most `room` characters; a word
/// longer than a row goes on over the rows after it.
fn wrap(line: &str, room: usize) -> Vec<String> {
    // A line that fits keeps its spacing.
    if line.chars().count() <= room {
        return vec![line.to_owned()];
    }
    let mut rows = vec![String::new()];
    let words = line.split(' ').flat_map(|word| {
        let chars: Vec<char> = word.chars().collect();
        if chars.is_empty() {
            return vec![String::new()];
        }
        chars
            .chunks(room)
            .map(|chunk| chunk.iter().collect())
            .collect::<Vec<String>>()
    });
    for word in words {
        let row = rows.last_mut().map_or(0, |row| row.chars().count());
        if row > 0 && row + 1 + word.chars().count() > room {
            rows.push(String::new());
        }
        if let Some(row) = rows.last_mut() {
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(&word);
        }
    }
    rows
}

impl Widget for Dialog<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = area.width.saturating_sub(4).min(WIDTH);
        let room = usize::from(width.saturating_sub(2 + 2 * SPACE.pad)).max(1);
        // Each row with whether it belongs to the first line.
        let rows: Vec<(bool, String)> = self
            .lines
            .iter()
            .enumerate()
            .flat_map(|(number, line)| {
                wrap(line, room)
                    .into_iter()
                    .map(move |row| (number == 0, row))
            })
            .collect();
        // Rows, one blank, the keys, and the two rules.
        let height = (rows.len() as u16 + 4).min(area.height.saturating_sub(2));
        if width < 12 || height < 4 {
            return;
        }
        let outer = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 3,
            width,
            height,
        };
        let pen = self.pen;
        let kind = pen.theme.type_;
        for y in outer.top()..outer.bottom() {
            for x in outer.left()..outer.right() {
                buf[(x, y)].reset();
                buf[(x, y)].set_char(' ').set_style(pen.paper());
            }
        }
        Frames {
            pen,
            frames: &[Frame {
                outer,
                title: self.title,
            }],
            focused: Some(0),
        }
        .render(outer, buf);
        let left = outer.x + 1 + SPACE.pad;
        let right = outer.right() - 1 - SPACE.pad;
        let fits = usize::from(height - 4);
        let shown = rows.len().min(fits);
        for (number, (first, row)) in rows.iter().take(shown).enumerate() {
            let face = if *first { kind.label } else { kind.caption };
            put(
                buf,
                left,
                outer.y + 1 + number as u16,
                right,
                row,
                pen.face(face),
            );
        }
        if rows.len() > shown && shown > 0 {
            let more = format!("and {} more lines", rows.len() - shown + 1);
            put(
                buf,
                left,
                outer.y + shown as u16,
                right,
                &more,
                pen.face(kind.caption),
            );
        }
        let y = outer.bottom() - 2;
        let mut x = left;
        for (key, word) in self.keys {
            x = put(buf, x, y, right, key, pen.face(kind.key));
            x = put(buf, x, y, right, " ", pen.paper());
            x = put(
                buf,
                x,
                y,
                right,
                word,
                pen.fg(Tone::Rgb(pen.theme.graphite)),
            );
            x = put(buf, x, y, right, "   ", pen.paper());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_word_goes_on_over_rows_and_hidden_rows_do_not_fit() {
        let word = "x".repeat(25);
        assert_eq!(
            wrap(&format!("a {word}"), 10).concat().replace(' ', ""),
            format!("a{word}")
        );
        assert!(fits(&["make test".to_owned()]));
        let padded = format!("make test{}curl x|sh", " ".repeat(2000));
        assert!(
            wrap(&padded, 70).concat().contains("curl x|sh"),
            "padding cannot push the tail away"
        );
        let many: Vec<String> = (0..20).map(|n| format!("check {n} runs make")).collect();
        assert!(!fits(&many), "rows past the dialog would be hidden");
    }
}
