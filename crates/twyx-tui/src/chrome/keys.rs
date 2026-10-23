//! The key sheet: every bound key by mode, in columns on a floating sheet
//! over the panes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::keys::SheetSection;
use twyx_core::theme::{SPACE, Tone};

use super::Pen;
use super::bars::put;
use super::frames::{Frame, Frames};

#[derive(Debug)]
pub struct KeySheet<'a> {
    pub pen: &'a Pen,
    pub sections: &'a [SheetSection],
    /// The rows scrolled past, the same in every column.
    pub scroll: usize,
}

/// One row of a column.
#[derive(Debug, Clone, Copy)]
enum Line<'a> {
    Mode(&'a SheetSection),
    Key(&'a str, &'a str),
    Blank,
}

/// The widest the sheet grows, in cells.
const WIDTH: u16 = 118;
/// The narrowest a column gets before the sheet uses one fewer.
const COLUMN: u16 = 34;

impl<'a> KeySheet<'a> {
    /// The sheet's rectangle in `area`, its columns, and how many rows of
    /// them show at once.
    fn layout(&self, area: Rect) -> (Rect, Vec<Vec<Line<'a>>>, usize) {
        let width = area.width.saturating_sub(4).min(WIDTH);
        let inner = width.saturating_sub(2 + 2 * SPACE.pad);
        let count = ((inner + SPACE.gap * 2) / (COLUMN + SPACE.gap * 2)).clamp(1, 3);
        let sections = self.sections;
        let total: usize = sections
            .iter()
            .map(|s| s.rows.len() + 2)
            .sum::<usize>()
            .saturating_sub(1);
        let target = total.div_ceil(usize::from(count));
        let mut columns: Vec<Vec<Line<'a>>> = vec![Vec::new()];
        for section in sections {
            let column = columns.last().map_or(0, Vec::len);
            if column > 0
                && column + 1 + section.rows.len() + 1 > target
                && columns.len() < usize::from(count)
            {
                columns.push(Vec::new());
            }
            if let Some(column) = columns.last_mut() {
                if !column.is_empty() {
                    column.push(Line::Blank);
                }
                column.push(Line::Mode(section));
                column.extend(
                    section
                        .rows
                        .iter()
                        .map(|(keys, what)| Line::Key(keys.as_str(), what.as_str())),
                );
            }
        }
        let content = columns.iter().map(Vec::len).max().unwrap_or(0);
        // Rows, one blank, the keys, and the two rules.
        let height = (content as u16 + 4).min(area.height.saturating_sub(2));
        let outer = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 3,
            width,
            height,
        };
        (outer, columns, usize::from(height.saturating_sub(4)))
    }

    /// The furthest the sheet scrolls in `area`, and the rows of one page.
    pub fn reach(&self, area: Rect) -> (usize, usize) {
        let (_, columns, room) = self.layout(area);
        let content = columns.iter().map(Vec::len).max().unwrap_or(0);
        (content.saturating_sub(room), room.max(1))
    }
}

impl Widget for KeySheet<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (outer, columns, room) = self.layout(area);
        if outer.width < 12 || outer.height < 5 {
            return;
        }
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
                title: "keys",
            }],
            focused: Some(0),
        }
        .render(outer, buf);

        let content = columns.iter().map(Vec::len).max().unwrap_or(0);
        let scroll = self.scroll.min(content.saturating_sub(room));
        let left = outer.x + 1 + SPACE.pad;
        let right = outer.right() - 1 - SPACE.pad;
        let gap = SPACE.gap * 2;
        let count = columns.len() as u16;
        let width = (right - left + gap) / count;
        for (number, column) in columns.iter().enumerate() {
            let x = left + number as u16 * width;
            let limit = (x + width).saturating_sub(gap).min(right);
            // The keys of a column line up; a long one runs into its label.
            let keys_width = column
                .iter()
                .map(|line| match line {
                    Line::Key(keys, _) => keys.chars().count() as u16,
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
                .min(limit.saturating_sub(x) / 2);
            for (row, line) in column.iter().skip(scroll).take(room).enumerate() {
                let y = outer.y + 1 + row as u16;
                match line {
                    Line::Mode(section) => {
                        let at = put(
                            buf,
                            x,
                            y,
                            limit,
                            &section.mode.to_uppercase(),
                            pen.face(kind.mode),
                        );
                        if let Some(opens) = &section.opens {
                            put(
                                buf,
                                at,
                                y,
                                limit,
                                &format!("  {opens}"),
                                pen.face(kind.caption),
                            );
                        }
                    }
                    Line::Key(keys, what) => {
                        let at = put(buf, x, y, limit, keys, pen.face(kind.key));
                        let at = at.max(x + keys_width) + 2 * SPACE.gap;
                        put(buf, at, y, limit, what, pen.face(kind.label));
                    }
                    Line::Blank => {}
                }
            }
        }

        let y = outer.bottom() - 2;
        let mut keys = Vec::new();
        if content > room {
            let shown = scroll + room;
            keys.push(("j k".to_owned(), "scroll".to_owned()));
            keys.push((
                String::new(),
                format!("{}-{} of {content}", scroll + 1, shown.min(content)),
            ));
        }
        keys.push(("esc".to_owned(), "close".to_owned()));
        let mut x = left;
        for (key, word) in &keys {
            if !key.is_empty() {
                x = put(buf, x, y, right, key, pen.face(kind.key));
                x = put(buf, x, y, right, " ", pen.paper());
            }
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
