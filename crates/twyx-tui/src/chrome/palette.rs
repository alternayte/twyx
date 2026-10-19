//! The command palette: a floating sheet over the panes, with a query line,
//! one rule, and the entries.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::theme::{Face, RULES, SPACE};

use super::Pen;
use super::bars::put;
use super::frames::{Frame, Frames};

#[derive(Debug, Clone)]
pub struct Entry<'a> {
    pub text: &'a str,
    /// What the entry is: command, project, worktree, pane, agent session.
    pub kind: &'a str,
    /// The character positions of `text` that match the query.
    pub matched: &'a [usize],
}

#[derive(Debug)]
pub struct Palette<'a> {
    pub pen: &'a Pen,
    pub query: &'a str,
    pub entries: &'a [Entry<'a>],
    pub selected: usize,
}

/// The widest the palette grows, in cells.
const WIDTH: u16 = 72;

impl Palette<'_> {
    /// Where the palette sits in `area`: centred, a third of the way down.
    pub fn place(&self, area: Rect) -> Rect {
        let width = area.width.saturating_sub(4).min(WIDTH);
        let height = (self.entries.len() as u16 + 4).min(area.height.saturating_sub(2));
        Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 3,
            width,
            height,
        }
    }
}

impl Widget for Palette<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let outer = self.place(area);
        if outer.width < 8 || outer.height < 4 {
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
            frames: &[Frame { outer, title: "" }],
            focused: Some(0),
        }
        .render(outer, buf);

        let left = outer.x + 1 + SPACE.pad;
        let right = outer.right() - 1 - SPACE.pad;
        let y = outer.y + 1;
        let x = put(buf, left, y, right, "› ", pen.accent());
        put(buf, x, y, right, self.query, pen.face(kind.label));

        let rule = outer.y + 2;
        buf[(outer.x, rule)]
            .set_char(RULES.tee_right)
            .set_style(pen.accent());
        buf[(outer.right() - 1, rule)]
            .set_char(RULES.tee_left)
            .set_style(pen.accent());
        for x in outer.x + 1..outer.right() - 1 {
            buf[(x, rule)]
                .set_char(RULES.horizontal)
                .set_style(pen.rule());
        }

        let rows = usize::from(outer.height.saturating_sub(4));
        for (number, entry) in self.entries.iter().take(rows).enumerate() {
            let y = outer.y + 3 + number as u16;
            let selected = number == self.selected;
            let marker = if selected { "› " } else { "  " };
            let mut x = put(buf, left, y, right, marker, pen.accent());
            let kind_width = entry.kind.chars().count() as u16;
            let text_limit = right.saturating_sub(kind_width + 2 * SPACE.gap);
            let base = if selected { kind.title } else { kind.label };
            for (position, ch) in entry.text.chars().enumerate() {
                let face = if entry.matched.contains(&position) {
                    Face {
                        bold: base.bold,
                        ..kind.matched
                    }
                } else {
                    base
                };
                x = put(buf, x, y, text_limit, &ch.to_string(), pen.face(face));
            }
            if x + 2 * SPACE.gap <= right.saturating_sub(kind_width) {
                put(
                    buf,
                    right - kind_width,
                    y,
                    right,
                    entry.kind,
                    pen.face(kind.caption),
                );
            }
        }
    }
}
