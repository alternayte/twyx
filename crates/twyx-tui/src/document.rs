//! The document view: a Markdown file laid out in cells, with its links
//! chosen by key. Images show as their alt text; a terminal shows no
//! picture.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Widget;
use twyx_core::layout::LeafId;
use twyx_core::markdown::{Align, Block, Document, Inline};
use twyx_core::theme::{RULES, SPACE, Tone};
pub use twyx_protocol::documents::{DocKey, Documents, Shown};

use crate::chrome::Pen;
use crate::chrome::bars::put;

/// The keys a focused document shows on its last row.
const KEYS: [(&str, &str); 5] = [
    ("j k", "scroll"),
    ("tab", "link"),
    ("enter", "follow"),
    ("e", "neovim"),
    ("g G", "top end"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Shade {
    #[default]
    Ink,
    Graphite,
    Accent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Look {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    shade: Shade,
    /// The link this text belongs to, by its place in reading order.
    link: Option<usize>,
}

/// One laid-out row: runs of text with their look.
#[derive(Debug, Clone, Default)]
pub struct Row(Vec<(String, Look)>);

/// A document laid out for one width.
#[derive(Debug, Clone, Default)]
pub struct Laid {
    pub rows: Vec<Row>,
    /// The first row of each link.
    pub links: Vec<usize>,
    /// The row of each heading.
    pub headings: Vec<usize>,
}

/// The first shown row of each document leaf.
#[derive(Debug, Default)]
pub struct Tops(pub HashMap<LeafId, usize>);

impl Tops {
    /// Applies a scroll key for a leaf `height` rows tall.
    pub fn apply(&mut self, leaf: LeafId, key: &DocKey, laid: &Laid, height: u16) {
        let page = usize::from(height.saturating_sub(2)).max(1);
        let last = laid.rows.len().saturating_sub(page);
        let top = self.0.entry(leaf).or_default();
        let reveal = |row: usize, top: usize| {
            if row < top || row >= top + page {
                row.saturating_sub(page / 3)
            } else {
                top
            }
        };
        *top = match *key {
            DocKey::Scroll(rows) => top.saturating_add_signed(rows as isize),
            DocKey::Page(pages) => top.saturating_add_signed(pages as isize * page as isize),
            DocKey::Top => 0,
            DocKey::Bottom => last,
            DocKey::Link(link) => laid.links.get(link).map_or(*top, |row| reveal(*row, *top)),
            DocKey::Heading(heading) => laid.headings.get(heading).copied().unwrap_or(*top),
            _ => *top,
        }
        .min(last);
    }
}

fn cells(text: &str) -> usize {
    Span::raw(text).width()
}

struct Writer {
    width: usize,
    laid: Laid,
    links: usize,
}

impl Writer {
    fn push(&mut self, row: Row) {
        self.laid.rows.push(row);
    }

    fn blank(&mut self) {
        if self.laid.rows.last().is_some_and(|row| !row.0.is_empty()) {
            self.push(Row::default());
        }
    }

    /// Lays out blocks with `first` before the first row and `rest` before
    /// every other row, a blank row between them unless `tight`, as in a
    /// list item.
    fn blocks(&mut self, blocks: &[Block], first: &Row, rest: &Row, tight: bool) {
        for (index, block) in blocks.iter().enumerate() {
            let lead = if index == 0 { first } else { rest };
            if index > 0 && !tight {
                self.prefixed_blank(rest);
            }
            self.block(block, lead, rest);
        }
    }

    fn prefixed_blank(&mut self, rest: &Row) {
        if rest.0.iter().all(|(text, _)| text.trim().is_empty()) {
            self.blank();
        } else {
            self.push(rest.clone());
        }
    }

    fn block(&mut self, block: &Block, first: &Row, rest: &Row) {
        match block {
            Block::Heading { level, text } => {
                self.laid.headings.push(self.laid.rows.len());
                let look = Look {
                    bold: true,
                    shade: match level {
                        1 => Shade::Accent,
                        2 => Shade::Ink,
                        _ => Shade::Graphite,
                    },
                    ..Look::default()
                };
                self.inlines(text, look, first, rest);
            }
            Block::Paragraph(text) => self.inlines(text, Look::default(), first, rest),
            Block::Code { text, .. } => {
                let look = Look {
                    shade: Shade::Graphite,
                    ..Look::default()
                };
                for (index, line) in text.lines().enumerate() {
                    let mut row = if index == 0 { first } else { rest }.clone();
                    row.0.push(("  ".to_owned(), look));
                    row.0.push((line.replace('\t', "    "), look));
                    self.push(row);
                }
            }
            Block::Html(html) => {
                let look = Look {
                    shade: Shade::Graphite,
                    ..Look::default()
                };
                for (index, line) in html.lines().enumerate() {
                    let mut row = if index == 0 { first } else { rest }.clone();
                    row.0.push((line.to_owned(), look));
                    self.push(row);
                }
            }
            Block::Quote(inner) => {
                let bar = (
                    format!("{} ", RULES.vertical),
                    Look {
                        shade: Shade::Graphite,
                        ..Look::default()
                    },
                );
                let mut first = first.clone();
                first.0.push(bar.clone());
                let mut rest = rest.clone();
                rest.0.push(bar);
                self.blocks(inner, &first, &rest, false);
            }
            Block::List { start, items } => {
                for (index, item) in items.iter().enumerate() {
                    let mark = match (item.task, start) {
                        (Some(true), _) => "[x] ".to_owned(),
                        (Some(false), _) => "[ ] ".to_owned(),
                        (None, Some(start)) => format!("{}. ", start + index as u64),
                        (None, None) => "• ".to_owned(),
                    };
                    let mut lead = if index == 0 { first } else { rest }.clone();
                    lead.0.push((
                        mark.clone(),
                        Look {
                            shade: if item.task == Some(true) {
                                Shade::Accent
                            } else {
                                Shade::Graphite
                            },
                            ..Look::default()
                        },
                    ));
                    let mut inner = rest.clone();
                    inner.0.push((" ".repeat(cells(&mark)), Look::default()));
                    self.blocks(&item.blocks, &lead, &inner, true);
                }
            }
            Block::Table { align, head, rows } => self.table(align, head, rows, first, rest),
            Block::Rule => {
                let mut row = first.clone();
                let used: usize = row.0.iter().map(|(text, _)| cells(text)).sum();
                row.0.push((
                    RULES
                        .horizontal
                        .to_string()
                        .repeat(self.width.saturating_sub(used)),
                    Look {
                        shade: Shade::Graphite,
                        ..Look::default()
                    },
                ));
                self.push(row);
            }
        }
    }

    /// Wraps inline text to the width.
    fn inlines(&mut self, text: &[Inline], look: Look, first: &Row, rest: &Row) {
        let mut words: Vec<(String, Look)> = Vec::new();
        self.words(text, look, &mut words);
        let mut row = first.clone();
        let indent = |row: &Row| row.0.iter().map(|(text, _)| cells(text)).sum::<usize>();
        let mut used = indent(&row);
        let mut empty = true;
        for (word, look) in words {
            if word == "\n" {
                self.push(std::mem::replace(&mut row, rest.clone()));
                used = indent(&row);
                empty = true;
                continue;
            }
            let space = word.trim().is_empty();
            if space && empty {
                continue;
            }
            let width = cells(&word);
            if !space && !empty && used + width > self.width {
                while row.0.last().is_some_and(|(text, _)| text == " ") {
                    row.0.pop();
                }
                self.push(std::mem::replace(&mut row, rest.clone()));
                used = indent(&row);
                empty = true;
            }
            // A word wider than the row is cut into pieces that fit.
            let mut word = word;
            while used + cells(&word) > self.width && empty && !space {
                let room = self.width.saturating_sub(used).max(1);
                let cut: String = word.chars().take(room).collect();
                word = word.chars().skip(room).collect();
                row.0.push((cut, look));
                self.push(std::mem::replace(&mut row, rest.clone()));
                used = indent(&row);
            }
            if word.is_empty() {
                continue;
            }
            used += cells(&word);
            empty = false;
            row.0.push((word, look));
        }
        if !empty || self.laid.rows.is_empty() || indent(&row) > 0 {
            self.push(row);
        }
    }

    fn words(&mut self, text: &[Inline], look: Look, words: &mut Vec<(String, Look)>) {
        for inline in text {
            match inline {
                Inline::Text(text) => split(text, look, words),
                Inline::Code(code) => words.push((
                    code.clone(),
                    Look {
                        shade: Shade::Graphite,
                        ..look
                    },
                )),
                Inline::Emphasis(inner) => self.words(
                    inner,
                    Look {
                        italic: true,
                        ..look
                    },
                    words,
                ),
                Inline::Strong(inner) => {
                    self.words(inner, Look { bold: true, ..look }, words);
                }
                Inline::Strike(inner) => self.words(
                    inner,
                    Look {
                        strike: true,
                        ..look
                    },
                    words,
                ),
                Inline::Link { text, .. } => {
                    let link = self.links;
                    self.links += 1;
                    let at = words.len();
                    self.words(
                        text,
                        Look {
                            underline: true,
                            shade: Shade::Accent,
                            link: Some(link),
                            ..look
                        },
                        words,
                    );
                    if words.len() == at {
                        words.push((
                            "link".to_owned(),
                            Look {
                                underline: true,
                                shade: Shade::Accent,
                                link: Some(link),
                                ..look
                            },
                        ));
                    }
                }
                Inline::Image { alt, .. } => {
                    let alt = if alt.is_empty() { "image" } else { alt };
                    words.push((
                        format!("[{alt}]"),
                        Look {
                            shade: Shade::Graphite,
                            ..look
                        },
                    ));
                }
                Inline::Break => words.push(("\n".to_owned(), look)),
                Inline::Html(html) => split(
                    html,
                    Look {
                        shade: Shade::Graphite,
                        ..look
                    },
                    words,
                ),
            }
        }
    }

    fn table(
        &mut self,
        align: &[Align],
        head: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        first: &Row,
        rest: &Row,
    ) {
        // Links in cells keep their reading order, though a cell shows
        // plain text.
        let mut texts: Vec<Vec<(String, Option<usize>)>> = Vec::new();
        for row in std::iter::once(head).chain(rows.iter().map(Vec::as_slice)) {
            let mut cells_text = Vec::new();
            for cell in row {
                let mut words = Vec::new();
                self.words(cell, Look::default(), &mut words);
                let link = words.iter().find_map(|(_, look)| look.link);
                let text: String = words
                    .into_iter()
                    .map(|(word, _)| if word == "\n" { " ".to_owned() } else { word })
                    .collect();
                cells_text.push((text, link));
            }
            texts.push(cells_text);
        }
        let columns = texts.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let indent: usize = first.0.iter().map(|(text, _)| cells(text)).sum();
        let room = self
            .width
            .saturating_sub(indent + 3 * columns.saturating_sub(1));
        let mut widths: Vec<usize> = (0..columns)
            .map(|column| {
                texts
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|(text, _)| cells(text))
                    .max()
                    .unwrap_or(0)
                    .max(1)
            })
            .collect();
        while widths.iter().sum::<usize>() > room && widths.iter().any(|width| *width > 3) {
            if let Some(widest) = widths.iter_mut().max() {
                *widest -= 1;
            }
        }
        let rule = Look {
            shade: Shade::Graphite,
            ..Look::default()
        };
        for (index, row) in texts.iter().enumerate() {
            let mut line = if index == 0 { first } else { rest }.clone();
            for (column, width) in widths.iter().enumerate() {
                if column > 0 {
                    line.0.push((format!(" {} ", RULES.vertical), rule));
                }
                let (text, link) = row.get(column).cloned().unwrap_or_default();
                let fitted = fit(&text, *width, align.get(column).copied());
                line.0.push((
                    fitted,
                    Look {
                        bold: index == 0,
                        underline: link.is_some(),
                        shade: if link.is_some() {
                            Shade::Accent
                        } else {
                            Shade::Ink
                        },
                        link,
                        ..Look::default()
                    },
                ));
            }
            self.push(line);
            if index == 0 {
                let mut line = rest.clone();
                let parts: Vec<String> = widths
                    .iter()
                    .map(|width| RULES.horizontal.to_string().repeat(*width))
                    .collect();
                let joint = format!("{h}{}{h}", RULES.cross, h = RULES.horizontal);
                line.0.push((parts.join(&joint), rule));
                self.push(line);
            }
        }
    }
}

/// Splits text into words and the spaces between them.
fn split(text: &str, look: Look, words: &mut Vec<(String, Look)>) {
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !word.is_empty() {
                words.push((std::mem::take(&mut word), look));
            }
            words.push((" ".to_owned(), look));
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        words.push((word, look));
    }
}

/// `text` cut or padded to `width` cells, placed by the column's
/// alignment.
fn fit(text: &str, width: usize, align: Option<Align>) -> String {
    let mut shown = String::new();
    let mut used = 0;
    let full = cells(text);
    for ch in text.chars() {
        let next = cells(ch.encode_utf8(&mut [0; 4]));
        if used + next > width || (full > width && used + next + 1 > width) {
            shown.push('…');
            used += 1;
            break;
        }
        shown.push(ch);
        used += next;
    }
    let gap = width.saturating_sub(used);
    match align {
        Some(Align::Right) => format!("{}{shown}", " ".repeat(gap)),
        Some(Align::Center) => format!(
            "{}{shown}{}",
            " ".repeat(gap / 2),
            " ".repeat(gap - gap / 2)
        ),
        _ => format!("{shown}{}", " ".repeat(gap)),
    }
}

/// Lays a document out `width` cells wide.
pub fn lay(document: &Document, width: u16) -> Laid {
    let mut writer = Writer {
        width: usize::from(width).max(8),
        laid: Laid::default(),
        links: 0,
    };
    writer.blocks(&document.blocks, &Row::default(), &Row::default(), false);
    writer.laid.links = vec![usize::MAX; writer.links];
    for (index, row) in writer.laid.rows.iter().enumerate() {
        for (_, look) in &row.0 {
            if let Some(link) = look.link
                && writer.laid.links[link] == usize::MAX
            {
                writer.laid.links[link] = index;
            }
        }
    }
    writer.laid
}

/// The width a document is laid out to in a leaf `area` wide.
pub fn text_width(area: u16) -> u16 {
    area.saturating_sub(2 * SPACE.pad)
}

/// One document leaf's content.
#[derive(Debug)]
pub struct DocumentView<'a> {
    pub pen: &'a Pen,
    pub shown: Option<&'a Shown>,
    pub top: usize,
    pub focused: bool,
}

impl Widget for DocumentView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height < 2 {
            return;
        }
        let pen = self.pen;
        let kind = pen.theme.type_;
        buf.set_style(area, pen.paper());
        let left = area.x + SPACE.pad;
        let right = area.right().saturating_sub(SPACE.pad);
        let Some(shown) = self.shown else {
            put(
                buf,
                left,
                area.y,
                right,
                "reading the document",
                pen.face(kind.caption),
            );
            return;
        };
        let mut y = area.y;
        let mut bottom = area.bottom();
        if let Some(error) = &shown.text.error {
            put(buf, left, y, right, error, pen.face(kind.label));
            y += 1;
        } else if shown.text.truncated {
            put(
                buf,
                left,
                y,
                right,
                "the file is longer than twyx shows",
                pen.face(kind.caption),
            );
            y += 1;
        }
        if let Some(address) = &shown.confirm {
            bottom -= 1;
            let mut x = put(buf, left, bottom, right, "open ", pen.face(kind.label));
            x = put(buf, x, bottom, right, address, pen.accent());
            x = put(buf, x, bottom, right, "?  ", pen.face(kind.label));
            x = put(buf, x, bottom, right, "enter", pen.face(kind.key));
            put(
                buf,
                x,
                bottom,
                right,
                " opens · any key cancels",
                pen.face(kind.caption),
            );
        } else if self.focused && area.height > 6 {
            bottom -= 1;
            let mut x = left;
            for (key, word) in KEYS {
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
        let laid = lay(&shown.document, text_width(area.width));
        for (row, line) in laid.rows.iter().skip(self.top).zip(y..bottom) {
            let mut x = left;
            for (text, look) in &row.0 {
                x = put(buf, x, line, right, text, style(pen, *look, shown.link));
            }
        }
    }
}

fn style(pen: &Pen, look: Look, chosen: Option<usize>) -> Style {
    let mut style = match look.shade {
        Shade::Ink => pen.fg(Tone::Ink),
        Shade::Graphite => pen.fg(Tone::Rgb(pen.theme.graphite)),
        Shade::Accent => pen.accent(),
    };
    for (on, modifier) in [
        (look.bold, Modifier::BOLD),
        (look.italic, Modifier::ITALIC),
        (look.strike, Modifier::CROSSED_OUT),
        (look.underline, Modifier::UNDERLINED),
    ] {
        if on {
            style = style.add_modifier(modifier);
        }
    }
    if look.link.is_some() && look.link == chosen {
        style = style.bg(pen.rgb(pen.theme.selection));
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(laid: &Laid) -> Vec<String> {
        laid.rows
            .iter()
            .map(|row| row.0.iter().map(|(text, _)| text.as_str()).collect())
            .collect()
    }

    #[test]
    fn a_document_wraps_lists_quotes_and_tables_to_the_width() {
        let document = Document::parse(
            "# Title\n\nOne two three four five six.\n\n\
             - [x] done item that wraps\n- plain\n\n\
             > quoted [link](a.md)\n\n\
             | name | n |\n|---|--:|\n| alpha | 1 |\n",
        );
        let laid = lay(&document, 20);
        assert_eq!(
            text(&laid),
            [
                "Title",
                "",
                "One two three four",
                "five six.",
                "",
                "[x] done item that",
                "    wraps",
                "• plain",
                "",
                "│ quoted link",
                "",
                "name  │ n",
                "──────┼──",
                "alpha │ 1",
            ]
        );
        assert_eq!(laid.headings, [0]);
        assert_eq!(laid.links, [9]);
    }
}
