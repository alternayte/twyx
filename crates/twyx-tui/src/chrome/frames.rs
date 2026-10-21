//! Pane frames: one hairline box around each pane.
//!
//! Each frame is the pane's rectangle grown by one cell. Tiled panes sit apart;
//! the panes of a stack overlap on a shared rule, and each rule cell takes its
//! glyph from the directions it connects, so junctions are exact at every size.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use twyx_core::theme::{RULES, SPACE};

use super::Pen;

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// One pane's frame: its outer rectangle and its title.
#[derive(Debug, Clone)]
pub struct Frame<'a> {
    pub outer: Rect,
    pub title: &'a str,
}

/// The frames of one tab. The focused frame is drawn in the accent.
#[derive(Debug)]
pub struct Frames<'a> {
    pub pen: &'a Pen,
    pub frames: &'a [Frame<'a>],
    pub focused: Option<usize>,
}

impl Widget for Frames<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = usize::from(area.width);
        let mut lines = vec![0_u8; width * usize::from(area.height)];
        let mut accent = vec![false; lines.len()];
        let index = |x: u16, y: u16| -> Option<usize> {
            let inside = x >= area.x && y >= area.y && x < area.right() && y < area.bottom();
            inside.then(|| usize::from(y - area.y) * width + usize::from(x - area.x))
        };

        for (number, frame) in self.frames.iter().enumerate() {
            let outer = frame.outer;
            if outer.width < 2 || outer.height < 2 {
                continue;
            }
            let focused = self.focused == Some(number);
            let (left, right) = (outer.x, outer.right() - 1);
            let (top, bottom) = (outer.y, outer.bottom() - 1);
            let mut mark = |x: u16, y: u16, directions: u8| {
                if let Some(at) = index(x, y) {
                    lines[at] |= directions;
                    accent[at] |= focused;
                }
            };
            for x in left..=right {
                let along = if x > left { LEFT } else { 0 } | if x < right { RIGHT } else { 0 };
                mark(
                    x,
                    top,
                    along | if x == left || x == right { DOWN } else { 0 },
                );
                mark(
                    x,
                    bottom,
                    along | if x == left || x == right { UP } else { 0 },
                );
            }
            for y in top + 1..bottom {
                mark(left, y, UP | DOWN);
                mark(right, y, UP | DOWN);
            }
        }

        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let Some(at) = index(x, y) else { continue };
                let Some(glyph) = glyph(lines[at]) else {
                    continue;
                };
                let style = if accent[at] {
                    self.pen.accent()
                } else {
                    self.pen.rule()
                };
                buf[(x, y)].set_char(glyph).set_style(style);
            }
        }

        for (number, frame) in self.frames.iter().enumerate() {
            let face = if self.focused == Some(number) {
                self.pen.theme.type_.title
            } else {
                self.pen.theme.type_.caption
            };
            title(buf, area, frame, self.pen.face(face));
        }
    }
}

/// The title sits on the top rule, one rule cell in from the corner and
/// padded by one cell on each side. A title that does not fit is cut.
/// A frame that runs past `area` keeps its title only where it shows.
fn title(buf: &mut Buffer, area: Rect, frame: &Frame<'_>, style: ratatui::style::Style) {
    let pad = SPACE.pad;
    let room = frame.outer.width.saturating_sub(4 + 2 * pad);
    let y = frame.outer.y;
    if room == 0 || frame.title.is_empty() || frame.outer.height < 2 {
        return;
    }
    if y < area.top() || y >= area.bottom() {
        return;
    }
    let x = frame.outer.x + 2 + pad;
    let limit = (x + room).min(area.right());
    if x >= limit {
        return;
    }
    let (end, _) = buf.set_stringn(x, y, frame.title, usize::from(limit - x), style);
    // The padding cells around the title are blanks on the rule.
    for blank in [x - pad, end] {
        if blank < area.right() {
            buf[(blank, y)].set_char(' ');
        }
    }
}

fn glyph(directions: u8) -> Option<char> {
    Some(match directions {
        0 => return None,
        d if d == LEFT | RIGHT || d == LEFT || d == RIGHT => RULES.horizontal,
        d if d == UP | DOWN || d == UP || d == DOWN => RULES.vertical,
        d if d == DOWN | RIGHT => RULES.top_left,
        d if d == DOWN | LEFT => RULES.top_right,
        d if d == UP | RIGHT => RULES.bottom_left,
        d if d == UP | LEFT => RULES.bottom_right,
        d if d == UP | DOWN | RIGHT => RULES.tee_right,
        d if d == UP | DOWN | LEFT => RULES.tee_left,
        d if d == LEFT | RIGHT | DOWN => RULES.tee_down,
        d if d == LEFT | RIGHT | UP => RULES.tee_up,
        _ => RULES.cross,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Depth;
    use twyx_core::theme::{Theme, Variant};

    fn draw(frames: &[Frame<'_>], area: Rect) -> Vec<String> {
        let pen = Pen::on_paper(Theme::sheet(Variant::Dark), Depth::Truecolor);
        let mut buf = Buffer::empty(area);
        Frames {
            pen: &pen,
            frames,
            focused: Some(0),
        }
        .render(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn stacked_frames_share_one_rule_and_meet_in_exact_junctions() {
        let frames = [
            Frame {
                outer: Rect::new(0, 0, 5, 5),
                title: "",
            },
            Frame {
                outer: Rect::new(4, 0, 5, 3),
                title: "",
            },
            Frame {
                outer: Rect::new(4, 2, 5, 3),
                title: "",
            },
        ];
        assert_eq!(
            draw(&frames, Rect::new(0, 0, 9, 5)),
            [
                "╭───┬───╮",
                "│   │   │",
                "│   ├───┤",
                "│   │   │",
                "╰───┴───╯",
            ]
        );
    }

    #[test]
    fn a_title_is_padded_and_cut_to_the_rule() {
        let frames = [Frame {
            outer: Rect::new(0, 0, 10, 2),
            title: "a-long-title",
        }];
        assert_eq!(draw(&frames, Rect::new(0, 0, 10, 2))[0], "╭─ a-lo ─╮");
    }
}
