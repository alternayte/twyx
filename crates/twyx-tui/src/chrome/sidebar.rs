//! The sidebar: the spaces section (projects, their worktrees and
//! workspaces, and the plain panes of each) and the agents section, down the
//! left edge.
//!
//! The selected row sits on a tinted bar: the workspace mode cursor while
//! the mode is open, else the client's own workspace. The focused pane is
//! lettered in the accent. A state glyph leads each row that has one.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
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
        let edge = area.right().saturating_sub(u16::from(self.rule));
        let left = area.x + SPACE.pad;
        let right = edge.saturating_sub(SPACE.pad);
        let selected = self.cursor.or_else(|| {
            self.rows
                .iter()
                .position(|row| row.current && row.kind != Kind::Pane)
        });
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
            let bar = selected == Some(index);
            let ground = |style: Style| {
                if bar {
                    style.bg(pen.rgb(pen.theme.selection))
                } else {
                    style
                }
            };
            if bar {
                // The pad on the right stays blank, so the bar never meets a pane.
                for x in area.x..right {
                    buf[(x, y)].set_style(ground(pen.paper()));
                }
            }
            if row.kind == Kind::Heading {
                put(
                    buf,
                    left,
                    y,
                    right,
                    &row.label,
                    pen.face(kind.caption).add_modifier(Modifier::BOLD),
                );
                continue;
            }
            if let Some(mark) = row.mark {
                let tone = ground(pen.fg(pen.theme.agent(mark)));
                put(buf, left, y, right, &mark.glyph().to_string(), tone);
            }
            let x = left + 2 + 2 * u16::from(row.depth);
            let style = match (row.current && row.kind == Kind::Pane, bar, row.kind) {
                (true, _, _) => pen.accent(),
                (false, true, _) | (false, false, Kind::Project) => pen.face(kind.title),
                (false, false, Kind::Pane) => pen.face(kind.caption),
                (false, false, _) => pen.face(kind.label),
            };
            // A branch named like its worktree says nothing new.
            let detail = if row.detail == row.label {
                ""
            } else {
                row.detail.as_str()
            };
            let label_width = text_width(&row.label);
            let dirty = u16::from(row.dirty);
            // The name keeps its room; the detail takes what is left, cut,
            // and goes when too little is left to read.
            let room = right.saturating_sub(x + label_width + 2 * SPACE.gap + dirty);
            let detail = if text_width(detail) <= room {
                detail.to_owned()
            } else if room > MIN_DETAIL {
                cut(detail, room)
            } else {
                String::new()
            };
            let label_room = right.saturating_sub(if row.dirty { dirty + SPACE.gap } else { 0 });
            put(
                buf,
                x,
                y,
                label_room,
                &cut(&row.label, label_room.saturating_sub(x)),
                ground(style),
            );
            let start = right.saturating_sub(text_width(&detail) + dirty);
            let next = put(
                buf,
                start,
                y,
                right,
                &detail,
                ground(pen.face(kind.caption)),
            );
            if row.dirty {
                put(buf, next, y, right, "*", ground(pen.face(kind.label)));
            }
        }
    }
}

/// The fewest cells a cut detail keeps, its `…` included.
const MIN_DETAIL: u16 = 4;

fn text_width(text: &str) -> u16 {
    ratatui::text::Span::raw(text).width() as u16
}

/// `text` in `room` cells, ending in `…` when it is cut.
fn cut(text: &str, room: u16) -> String {
    if text_width(text) <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    for character in text.chars() {
        if text_width(&kept) + text_width(&character.to_string()) + 1 > room {
            break;
        }
        kept.push(character);
    }
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Depth;
    use twyx_core::theme::{AgentState, Theme, Variant};

    fn row(kind: Kind, depth: u8, label: &str, detail: &str, dirty: bool) -> Row {
        Row {
            kind,
            depth,
            label: label.to_owned(),
            detail: detail.to_owned(),
            dirty,
            current: false,
            target: None,
            worktree: None,
            project: None,
            mark: None,
        }
    }

    fn draw(rows: &[Row], width: u16) -> Vec<String> {
        let pen = Pen::on_paper(Theme::sheet(Variant::Dark), Depth::Truecolor);
        let area = Rect::new(0, 0, width, rows.len() as u16);
        let mut buf = Buffer::empty(area);
        Sidebar {
            pen: &pen,
            rows,
            cursor: None,
            rule: false,
        }
        .render(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn a_name_keeps_its_room_the_branch_is_cut_into_the_rest_and_shows_once() {
        let mut agent = row(Kind::Worktree, 1, "sign-in-form", "sign-in-form", false);
        agent.mark = Some(AgentState::NeedsApproval);
        let rows = [
            row(Kind::Heading, 0, "spaces", "", false),
            row(Kind::Project, 0, "acme", "", false),
            row(Kind::Worktree, 1, "main", "main", true),
            row(
                Kind::Worktree,
                1,
                "fix-session-expiry",
                "topic/session",
                true,
            ),
            row(Kind::Worktree, 1, "fix", "topic/session-expiry", false),
            agent,
            row(Kind::Worktree, 1, "a-very-long-worktree-name", "", false),
        ];
        assert_eq!(
            draw(&rows, 24),
            [
                " spaces",
                "   acme",
                "     main             *",
                "     fix-session-exp… *",
                "     fix  topic/sessio…",
                " !   sign-in-form",
                "     a-very-long-workt…",
            ]
        );
    }
}
