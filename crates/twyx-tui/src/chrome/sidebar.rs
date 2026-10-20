//! The sidebar: projects, their worktrees and workspaces, and the panes of
//! each, down the left edge with one rule on its right.
//!
//! No fills. The client's own workspace and focused pane are lettered in
//! the accent; the workspace mode cursor is the palette's `›`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::theme::{RULES, SPACE};
use twyx_protocol::sidebar::{Kind, Row};

use super::Pen;
use super::bars::put;

#[derive(Debug)]
pub struct Sidebar<'a> {
    pub pen: &'a Pen,
    pub rows: &'a [Row],
    /// The workspace mode cursor, when the mode is open.
    pub cursor: Option<usize>,
    /// Whether to draw the rule on the right. A docked sidebar shares the
    /// panes' own left rule.
    pub rule: bool,
}

impl Sidebar<'_> {
    /// The first row shown in `height` lines, so the cursor stays in view.
    pub fn first(cursor: Option<usize>, height: u16) -> usize {
        let height = usize::from(height).max(1);
        cursor.map_or(0, |cursor| (cursor + 1).saturating_sub(height))
    }

    /// The row at a screen line of the sidebar's area.
    pub fn row_at(rows: usize, cursor: Option<usize>, area: Rect, y: u16) -> Option<usize> {
        if y < area.y || y >= area.bottom() {
            return None;
        }
        let row = Self::first(cursor, area.height) + usize::from(y - area.y);
        (row < rows).then_some(row)
    }
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 || area.height == 0 {
            return;
        }
        let pen = self.pen;
        let kind = pen.theme.type_;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].reset();
                buf[(x, y)].set_char(' ').set_style(pen.paper());
            }
            if self.rule {
                buf[(area.right() - 1, y)]
                    .set_char(RULES.vertical)
                    .set_style(pen.rule());
            }
        }
        let left = area.x + SPACE.pad;
        let right = area
            .right()
            .saturating_sub(u16::from(self.rule) + SPACE.pad);
        let first = Self::first(self.cursor, area.height);
        for (line, (index, row)) in self
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(area.height))
            .enumerate()
        {
            let y = area.y + line as u16;
            let at_cursor = self.cursor == Some(index);
            let marker = if at_cursor { "›" } else { " " };
            let mut x = put(buf, left, y, right, marker, pen.accent());
            x = put(
                buf,
                x,
                y,
                right,
                &" ".repeat(1 + 2 * usize::from(row.depth)),
                pen.paper(),
            );
            let style = match (row.current, at_cursor, row.kind) {
                (true, _, _) => pen.accent(),
                (false, true, _) => pen.face(kind.title),
                (false, false, Kind::Pane) => pen.face(kind.caption),
                (false, false, _) => pen.face(kind.label),
            };
            // The branch goes at the right end while the name keeps room.
            let detail = if row.dirty {
                format!("{}*", row.detail)
            } else {
                row.detail.clone()
            };
            let detail_width = detail.chars().count() as u16;
            let label_limit = if detail.is_empty() {
                right
            } else {
                right.saturating_sub(detail_width + 2 * SPACE.gap)
            };
            // A long label gives way to the mark.
            let mark_width = if row.mark.is_some() { 2 } else { 0 };
            let mut end = put(
                buf,
                x,
                y,
                label_limit.saturating_sub(mark_width),
                &row.label,
                style,
            );
            if let Some(mark) = row.mark {
                let tone = pen.fg(pen.theme.agent(mark));
                end = put(
                    buf,
                    end,
                    y,
                    label_limit,
                    &format!(" {}", mark.glyph()),
                    tone,
                );
            }
            if !detail.is_empty() && end + 2 * SPACE.gap <= right.saturating_sub(detail_width) {
                let start = right - detail_width;
                let next = put(buf, start, y, right, &row.detail, pen.face(kind.caption));
                if row.dirty {
                    put(buf, next, y, right + 1, "*", pen.face(kind.label));
                }
            }
        }
    }
}
