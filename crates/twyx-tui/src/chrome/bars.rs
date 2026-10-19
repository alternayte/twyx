//! The tab bar, the status bar and the hint bar.
//!
//! The status bar is the sheet's title block: fields kept apart by rules.
//! The hint bar is lettered over the bottom rule of the panes, so it comes and
//! goes without resizing a pane. A bar that runs out of room drops whole
//! fields; it never cuts a word.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::Widget;
use twyx_core::theme::{AgentState, RULES, SPACE, Tone};

use super::Pen;

/// Writes `text` from `x` and stops at `limit`. Returns the next free column.
pub(crate) fn put(buf: &mut Buffer, x: u16, y: u16, limit: u16, text: &str, style: Style) -> u16 {
    if x >= limit {
        return x;
    }
    buf.set_stringn(x, y, text, usize::from(limit - x), style).0
}

fn blank(count: u16) -> String {
    " ".repeat(usize::from(count))
}

/// The rule between two fields, with its spacing.
fn separator() -> String {
    format!("{}{}{}", blank(SPACE.gap), RULES.vertical, blank(SPACE.gap))
}

/// Styled runs that are kept or dropped together.
type Field = Vec<(String, Style)>;

fn width(field: &Field) -> u16 {
    field
        .iter()
        .map(|(text, _)| Span::raw(text.as_str()).width() as u16)
        .sum()
}

fn draw(buf: &mut Buffer, mut x: u16, y: u16, limit: u16, field: &Field) -> u16 {
    for (text, style) in field {
        x = put(buf, x, y, limit, text, *style);
    }
    x
}

/// Joins fields with `between`, keeping each field only while it fits in
/// `room` whole. The first field is always kept, cut if it must be.
fn fit(fields: Vec<Field>, between: &Field, room: u16) -> Field {
    let mut line = Field::new();
    let mut used = 0;
    for (number, field) in fields.into_iter().enumerate() {
        let joint = if number == 0 { 0 } else { width(between) };
        if number > 0 && used + joint + width(&field) > room {
            break;
        }
        if number > 0 {
            line.extend(between.iter().cloned());
        }
        used += joint + width(&field);
        line.extend(field);
    }
    line
}

#[derive(Debug, Clone)]
pub struct Tab<'a> {
    pub name: &'a str,
    /// The most urgent agent state in the tab, if it holds an agent session.
    pub agent: Option<AgentState>,
}

#[derive(Debug)]
pub struct TabBar<'a> {
    pub pen: &'a Pen,
    pub tabs: &'a [Tab<'a>],
    pub active: usize,
    /// The workspace, lettered at the right end while there is room.
    pub workspace: &'a str,
}

impl Widget for TabBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let pen = self.pen;
        let kind = pen.theme.type_;
        buf.set_style(area, pen.paper());
        let fields: Vec<Field> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(number, tab)| {
                let active = number == self.active;
                let (number_face, name_face) = if active {
                    (kind.mode, kind.title)
                } else {
                    (kind.caption, kind.caption)
                };
                let mut field = vec![
                    (format!("{} ", number + 1), pen.face(number_face)),
                    (tab.name.to_owned(), pen.face(name_face)),
                ];
                if let Some(state) = tab.agent {
                    field.push((
                        format!(" {}", state.glyph()),
                        pen.fg(pen.theme.agent(state)),
                    ));
                }
                field
            })
            .collect();
        let between = vec![(separator(), pen.rule())];
        let more = |count: usize| vec![(format!("+{count}"), pen.face(kind.caption))];
        let room = area.width.saturating_sub(2 * SPACE.pad);

        if fields.is_empty() {
            return;
        }
        // The active tab is always shown. The others are added to its right,
        // then to its left, while they fit; the hidden ones are counted.
        let arrange = |first: usize, last: usize| {
            let mut line: Vec<Field> = fields[first..=last].to_vec();
            if first > 0 {
                line.insert(0, more(first));
            }
            if last + 1 < fields.len() {
                line.push(more(fields.len() - last - 1));
            }
            line
        };
        let total = |line: &[Field]| {
            line.iter().map(width).sum::<u16>()
                + (line.len() as u16).saturating_sub(1) * width(&between)
        };
        let active = self.active.min(fields.len() - 1);
        let (mut first, mut last) = (active, active);
        while last + 1 < fields.len() && total(&arrange(first, last + 1)) <= room {
            last += 1;
        }
        while first > 0 && total(&arrange(first - 1, last)) <= room {
            first -= 1;
        }
        let mut shown = arrange(first, last);
        if total(&shown) > room {
            shown = vec![fields[active].clone()];
        }
        let line = fit(shown, &between, room);
        let x = draw(buf, area.x + SPACE.pad, area.y, area.right(), &line);

        let workspace = vec![(self.workspace.to_owned(), pen.face(kind.caption))];
        let start = area.right().saturating_sub(width(&workspace) + SPACE.pad);
        if start >= x + 2 * SPACE.gap && start > area.x {
            draw(buf, start, area.y, area.right(), &workspace);
        }
    }
}

#[derive(Debug)]
pub struct StatusBar<'a> {
    pub pen: &'a Pen,
    pub mode: &'a str,
    pub worktree: &'a str,
    pub branch: &'a str,
    pub dirty: bool,
    /// How many agent sessions are in each state.
    pub roll_up: &'a [(AgentState, usize)],
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let pen = self.pen;
        let kind = pen.theme.type_;
        buf.set_style(area, pen.paper());
        let room = area.width.saturating_sub(2 * SPACE.pad);

        let mut branch = vec![(self.branch.to_owned(), pen.face(kind.caption))];
        if self.dirty {
            branch.push(("*".to_owned(), pen.face(kind.label)));
        }
        let fields: Vec<Field> = vec![
            vec![(self.mode.to_uppercase(), pen.face(kind.mode))],
            vec![(self.worktree.to_owned(), pen.face(kind.label))],
            branch,
        ]
        .into_iter()
        .filter(|field| width(field) > 0)
        .collect();
        let left = fit(fields, &vec![(separator(), pen.rule())], room);
        let x = draw(buf, area.x + SPACE.pad, area.y, area.right(), &left);

        let roll_up: Vec<Field> = self
            .roll_up
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(state, count)| {
                vec![(
                    format!("{} {count}", state.glyph()),
                    pen.fg(pen.theme.agent(*state)),
                )]
            })
            .collect();
        // The roll-up is shown whole or not at all, so no count is misread.
        let mut line = Field::new();
        for (number, field) in roll_up.into_iter().enumerate() {
            if number > 0 {
                line.push((blank(2 * SPACE.gap), pen.paper()));
            }
            line.extend(field);
        }
        let free = area
            .right()
            .saturating_sub(SPACE.pad)
            .saturating_sub(x + 2 * SPACE.gap);
        if line.is_empty() || width(&line) > free {
            return;
        }
        let start = area.right() - SPACE.pad - width(&line);
        draw(buf, start, area.y, area.right(), &line);
    }
}

/// The keys of the mode the prefix opened, lettered over one rule row.
#[derive(Debug)]
pub struct HintBar<'a> {
    pub pen: &'a Pen,
    pub hints: &'a [(&'a str, &'a str)],
}

impl Widget for HintBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let pen = self.pen;
        let kind = pen.theme.type_;
        // One rule cell stays at each end, as on a pane title, and the text
        // is padded by one blank on each side.
        let room = area.width.saturating_sub(4 + 2 * SPACE.pad);
        let fields: Vec<Field> = self
            .hints
            .iter()
            .map(|(key, word)| {
                if key.is_empty() {
                    return vec![((*word).to_owned(), pen.fg(Tone::Rgb(pen.theme.graphite)))];
                }
                vec![
                    ((*key).to_owned(), pen.face(kind.key)),
                    (" ".to_owned(), pen.paper()),
                    ((*word).to_owned(), pen.fg(Tone::Rgb(pen.theme.graphite))),
                ]
            })
            .collect();
        let line = fit(fields, &vec![(blank(2 * SPACE.gap), pen.paper())], room);
        if line.is_empty() || width(&line) > room {
            return;
        }
        let mut x = area.x + 2;
        x = put(buf, x, area.y, area.right(), &blank(SPACE.pad), pen.paper());
        x = draw(buf, x, area.y, area.right(), &line);
        put(buf, x, area.y, area.right(), &blank(SPACE.pad), pen.paper());
    }
}
