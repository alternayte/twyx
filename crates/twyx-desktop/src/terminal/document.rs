//! A document leaf: a Markdown file drawn with its headings, lists,
//! tables, task lists, coloured code and images, laid over the leaf's
//! cells. Links open by click or by key; a remote address opens only after
//! the reader sees it and agrees, and a remote image loads only after the
//! reader allows it (Invariant 18).

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Div, FontStyle, FontWeight, HighlightStyle, Image, ImageFormat,
    InteractiveText, MouseButton, MouseDownEvent, ObjectFit, Pixels, ScrollWheelEvent,
    StrikethroughStyle, StyledText, UnderlineStyle, div, img, point, px,
};
use twyx_core::keys::{Chord, Key};
use twyx_core::layout::LeafId;
use twyx_core::markdown::{self, Align, Block, Inline, Load};
use twyx_core::theme::DESKTOP;
use twyx_core::tuning;
use twyx_protocol::documents::{DocKey, Shown};

use super::Terminal;
use super::review::coloured;
use crate::highlight::Highlighter;
use crate::paint::{Palette, Tones};

/// The widest a document's text runs, in pixels.
const MEASURE: f32 = 760.0;

/// One document leaf to lay over the tab area.
#[derive(Debug, Clone, Copy)]
pub(super) struct DocumentLeaf {
    pub leaf: LeafId,
    /// The leaf's inner cells, from the tab area's corner.
    pub bounds: Bounds<Pixels>,
    pub focused: bool,
}

/// A remote image the reader allowed.
#[derive(Debug, Clone)]
pub(super) enum Picture {
    Loading,
    Ready(Arc<Image>),
    Failed(String),
}

/// What draws one document: the leaf's file, the theme and the images.
struct Draw<'a> {
    leaf: LeafId,
    shown: &'a Shown,
    tones: Tones,
    palette: Palette,
    highlighter: &'a Highlighter,
    pictures: &'a HashMap<String, Picture>,
    /// The next link's place in reading order.
    links: usize,
    /// Tables drawn so far, for their scroll ids.
    tables: u64,
}

impl Terminal {
    pub(super) fn document_leaf(
        &mut self,
        shown: DocumentLeaf,
        tones: Tones,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let DocumentLeaf {
            leaf,
            bounds,
            focused,
        } = shown;
        let document = self.shared().documents.shown.get(&leaf).cloned();
        let root = div()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .overflow_hidden()
            .bg(tones.paper)
            .text_color(tones.ink)
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.focus_leaf(leaf);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            // The body scrolls itself; the wheel goes to no pane.
            .on_scroll_wheel(cx.listener(|_, _: &ScrollWheelEvent, _, cx| {
                cx.stop_propagation();
            }));
        let caption = |text: String| {
            div()
                .px(px(DESKTOP.pad * 2.0))
                .py(px(DESKTOP.gap / 2.0))
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text)
        };
        let Some(document) = document else {
            return root
                .child(caption("reading the document".to_owned()))
                .into_any_element();
        };
        if document.remote_allowed {
            self.fetch_pictures(&document, cx);
        }
        let mut notes = Vec::new();
        if let Some(error) = &document.text.error {
            notes.push(error.clone());
        } else if document.text.truncated {
            notes.push("the file is longer than twyx shows".to_owned());
        }
        if document.document.has_remote_images() && !document.remote_allowed {
            notes.push("remote images are not loaded · a loads them".to_owned());
        }
        let handle = self.document_scroll.entry(leaf).or_default().clone();
        let palette = Palette::of(tones.theme.variant);
        let pictures = std::mem::take(&mut self.pictures);
        let mut draw = Draw {
            leaf,
            shown: &document,
            tones,
            palette,
            highlighter: &self.highlighter,
            pictures: &pictures,
            links: 0,
            tables: 0,
        };
        let blocks: Vec<AnyElement> = document
            .document
            .blocks
            .iter()
            .map(|block| draw.block(block, cx).into_any_element())
            .collect();
        self.pictures = pictures;
        let body = div()
            .id(("document", leaf.0))
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&handle)
            .px(px(DESKTOP.pad * 3.0))
            .py(px(DESKTOP.pad * 2.0))
            .flex()
            .flex_col()
            .gap(px(DESKTOP.gap * 1.5))
            .max_w(px(MEASURE + DESKTOP.pad * 6.0))
            .children(blocks);
        let foot = match &document.confirm {
            Some(address) => Some(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(DESKTOP.gap))
                    .px(px(DESKTOP.pad * 2.0))
                    .py(px(DESKTOP.gap / 2.0))
                    .border_t(px(DESKTOP.rule))
                    .border_color(tones.rule)
                    .child("open")
                    .child(div().text_color(tones.accent).child(address.clone()))
                    .child(
                        div()
                            .text_color(tones.graphite)
                            .child("? enter opens · any key cancels"),
                    ),
            ),
            None if focused => Some(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(DESKTOP.gap * 2.0))
                    .px(px(DESKTOP.pad * 2.0))
                    .py(px(DESKTOP.gap / 2.0))
                    .text_size(px(DESKTOP.size.caption))
                    .text_color(tones.graphite)
                    .children(
                        [
                            ("tab", "link"),
                            ("enter", "follow"),
                            ("e", "neovim"),
                            ("a", "remote images"),
                            ("g G", "top end"),
                        ]
                        .into_iter()
                        .map(|(key, word)| {
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(DESKTOP.gap / 2.0))
                                .child(
                                    div()
                                        .text_color(tones.ink)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(key),
                                )
                                .child(word)
                        }),
                    ),
            ),
            None => None,
        };
        root.children(notes.into_iter().map(caption))
            .child(body)
            .children(foot)
            .into_any_element()
    }

    /// Takes a key in a focused document leaf.
    pub(super) fn document_key(&mut self, leaf: LeafId, chord: Chord, cx: &mut Context<Self>) {
        let key = self.shared().documents.key(leaf, chord);
        self.document_does(leaf, key, cx);
    }

    /// Follows a clicked link.
    fn follow_link(&mut self, leaf: LeafId, link: usize, cx: &mut Context<Self>) {
        let key = {
            let mut shared = self.shared();
            if let Some(shown) = shared.documents.shown.get_mut(&leaf) {
                shown.link = Some(link);
                shown.confirm = None;
            }
            shared.documents.key(leaf, Chord::plain(Key::Enter))
        };
        self.document_does(leaf, key, cx);
    }

    fn document_does(&mut self, leaf: LeafId, key: DocKey, cx: &mut Context<Self>) {
        let handle = self.document_scroll.entry(leaf).or_default().clone();
        let step = px(DESKTOP.unit * 6.0);
        let offset = handle.offset();
        let page = handle.bounds().size.height * 0.9;
        let blocks = {
            let shared = self.shared();
            shared
                .documents
                .shown
                .get(&leaf)
                .map(|shown| (shown.document.heading_blocks(), shown.document.clone()))
        };
        match key {
            DocKey::Stay => {}
            DocKey::Scroll(lines) => {
                handle.set_offset(point(offset.x, offset.y - step * lines as f32));
            }
            DocKey::Page(pages) => {
                handle.set_offset(point(offset.x, offset.y - page * pages as f32));
            }
            DocKey::Top => handle.set_offset(point(offset.x, px(0.0))),
            DocKey::Bottom => handle.scroll_to_bottom(),
            DocKey::Link(link) => {
                if let Some(block) = blocks
                    .as_ref()
                    .and_then(|(_, document)| document.block_of_link(link))
                {
                    handle.scroll_to_item(block);
                }
            }
            DocKey::Heading(heading) => {
                if let Some(block) = blocks
                    .as_ref()
                    .and_then(|(headings, _)| headings.get(heading).copied())
                {
                    handle.scroll_to_top_of_item(block);
                }
            }
            DocKey::Send(request) => self.request(request),
            DocKey::Browse(address) => cx.open_url(&address),
            DocKey::Notice(notice) => self.shared().workspaces.message = Some(notice),
        }
        cx.notify();
    }

    /// Starts fetching the allowed remote images not asked for yet.
    fn fetch_pictures(&mut self, shown: &Shown, cx: &mut Context<Self>) {
        let path = Path::new(&shown.text.path);
        let root = Path::new(&shown.text.root);
        let mut sources = Vec::new();
        collect_images(&shown.document.blocks, &mut sources);
        for source in sources {
            let Load::Fetch(address) = markdown::image(&source, path, root, true) else {
                continue;
            };
            if self.pictures.contains_key(&address) {
                continue;
            }
            self.pictures.insert(address.clone(), Picture::Loading);
            cx.spawn(async move |this, cx| {
                let fetched = cx
                    .background_executor()
                    .spawn({
                        let address = address.clone();
                        async move { fetch(&address) }
                    })
                    .await;
                let _ = this.update(cx, |terminal, cx| {
                    let picture = match fetched {
                        Ok(image) => Picture::Ready(image),
                        Err(error) => Picture::Failed(error),
                    };
                    terminal.pictures.insert(address, picture);
                    cx.notify();
                });
            })
            .detach();
        }
    }
}

fn collect_images(blocks: &[Block], sources: &mut Vec<String>) {
    fn inlines(list: &[Inline], sources: &mut Vec<String>) {
        for inline in list {
            match inline {
                Inline::Image { source, .. } => sources.push(source.clone()),
                Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                    inlines(inner, sources);
                }
                Inline::Link { text, .. } => inlines(text, sources),
                _ => {}
            }
        }
    }
    for block in blocks {
        match block {
            Block::Heading { text, .. } | Block::Paragraph(text) => inlines(text, sources),
            Block::Quote(inner) => collect_images(inner, sources),
            Block::List { items, .. } => {
                for item in items {
                    collect_images(&item.blocks, sources);
                }
            }
            Block::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    inlines(cell, sources);
                }
            }
            Block::Code { .. } | Block::Rule | Block::Html(_) => {}
        }
    }
}

/// Fetches an image the reader allowed, with `curl`: `http` and `https`
/// only, redirects included, within a size and a time.
fn fetch(address: &str) -> Result<Arc<Image>, String> {
    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--max-filesize",
            &tuning::REMOTE_IMAGE_BYTES.to_string(),
            "--max-time",
            &tuning::REMOTE_IMAGE_TIMEOUT.as_secs().to_string(),
            "--",
            address,
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("run curl: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let format = sniff(&output.stdout).ok_or("not an image twyx can show")?;
    Ok(Arc::new(Image::from_bytes(format, output.stdout)))
}

/// An image's format from its first bytes.
fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    let head = &bytes[..bytes.len().min(512)];
    Some(match head {
        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
        [0xff, 0xd8, 0xff, ..] => ImageFormat::Jpeg,
        [b'G', b'I', b'F', b'8', ..] => ImageFormat::Gif,
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => ImageFormat::Webp,
        [b'B', b'M', ..] => ImageFormat::Bmp,
        _ if String::from_utf8_lossy(head).contains("<svg") => ImageFormat::Svg,
        _ => return None,
    })
}

/// A run of inline text: the text, its styles and its links.
#[derive(Default)]
struct Runs {
    text: String,
    styles: Vec<(Range<usize>, HighlightStyle)>,
    /// Each link's text and its place in reading order.
    links: Vec<(Range<usize>, usize)>,
}

impl Draw<'_> {
    fn block(&mut self, block: &Block, cx: &mut Context<Terminal>) -> Div {
        let tones = self.tones;
        match block {
            Block::Heading { level, text } => {
                let size = match level {
                    1 => DESKTOP.size.heading[2],
                    2 => DESKTOP.size.heading[1],
                    3 => DESKTOP.size.heading[0],
                    _ => DESKTOP.size.body,
                };
                let heading = div()
                    .pt(px(DESKTOP.gap))
                    .text_size(px(size))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.inlines(text, cx));
                if *level <= 2 {
                    heading
                        .pb(px(DESKTOP.gap / 2.0))
                        .border_b(px(DESKTOP.rule))
                        .border_color(tones.rule)
                } else {
                    heading
                }
            }
            Block::Paragraph(text) => self.inlines(text, cx),
            Block::Code { language, text } => {
                let tokens = language
                    .as_deref()
                    .and_then(|language| self.highlighter.code(language, text));
                div()
                    .p(px(DESKTOP.pad))
                    .bg(tones.selection)
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .children(text.lines().enumerate().map(|(index, line)| {
                        let shown = line.replace('\t', "    ");
                        match tokens.as_ref().and_then(|tokens| tokens.get(index)) {
                            Some(tokens) => div().whitespace_nowrap().child(coloured(
                                line,
                                tokens,
                                tones,
                                self.palette,
                            )),
                            None => div()
                                .whitespace_nowrap()
                                .child(shown.replace(' ', "\u{a0}")),
                        }
                    }))
            }
            Block::Quote(inner) => div()
                .pl(px(DESKTOP.pad * 1.5))
                .border_l(px(DESKTOP.mark))
                .border_color(tones.rule)
                .text_color(tones.graphite)
                .flex()
                .flex_col()
                .gap(px(DESKTOP.gap))
                .children(inner.iter().map(|block| self.block(block, cx))),
            Block::List { start, items } => div()
                .flex()
                .flex_col()
                .gap(px(DESKTOP.gap / 2.0))
                .children(items.iter().enumerate().map(|(index, item)| {
                    let (mark, colour) = match (item.task, start) {
                        (Some(true), _) => ("☑".to_owned(), tones.accent),
                        (Some(false), _) => ("☐".to_owned(), tones.graphite),
                        (None, Some(start)) => {
                            (format!("{}.", start + index as u64), tones.graphite)
                        }
                        (None, None) => ("•".to_owned(), tones.graphite),
                    };
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.gap))
                        .child(
                            div()
                                .flex_shrink_0()
                                .min_w(px(DESKTOP.unit * 5.0))
                                .text_color(colour)
                                .child(mark),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap(px(DESKTOP.gap / 2.0))
                                .children(item.blocks.iter().map(|block| self.block(block, cx))),
                        )
                })),
            Block::Table { align, head, rows } => {
                let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
                // Column by column, so each column is as wide as its widest
                // cell; every row is one line tall, so the rows line up.
                let mut table = div()
                    .flex()
                    .flex_row()
                    .border(px(DESKTOP.rule))
                    .border_color(tones.rule);
                for column in 0..columns {
                    let mut cells = div().flex().flex_col();
                    if column > 0 {
                        cells = cells.border_l(px(DESKTOP.rule)).border_color(tones.rule);
                    }
                    let place = |cell: Div| match align.get(column) {
                        Some(Align::Right) => cell.justify_end(),
                        Some(Align::Center) => cell.justify_center(),
                        _ => cell,
                    };
                    let empty = Vec::new();
                    let first = head.get(column).unwrap_or(&empty);
                    cells = cells.child(
                        place(self.cell(first, cx))
                            .font_weight(FontWeight::SEMIBOLD)
                            .border_b(px(DESKTOP.rule))
                            .border_color(tones.rule),
                    );
                    for row in rows {
                        let text = row.get(column).unwrap_or(&empty);
                        cells = cells.child(place(self.cell(text, cx)));
                    }
                    table = table.child(cells);
                }
                // A wide table scrolls inside its own row.
                self.tables += 1;
                div().child(
                    div()
                        .id(("table", (self.leaf.0 << 20) | self.tables))
                        .overflow_x_scroll()
                        .child(table),
                )
            }
            Block::Rule => div().h(px(DESKTOP.rule)).bg(tones.rule),
            Block::Html(html) => {
                div()
                    .text_color(tones.graphite)
                    .flex()
                    .flex_col()
                    .children(html.lines().map(|line| {
                        div()
                            .whitespace_nowrap()
                            .child(line.replace('\t', "    ").replace(' ', "\u{a0}"))
                    }))
            }
        }
    }

    fn cell(&mut self, text: &[Inline], cx: &mut Context<Terminal>) -> Div {
        div()
            .h(px(DESKTOP.unit * 7.0))
            .px(px(DESKTOP.pad))
            .flex()
            .flex_row()
            .items_center()
            .whitespace_nowrap()
            .child(self.inlines(text, cx))
    }

    /// Inline content: text runs, each with its styles and clickable
    /// links, and the images between them.
    fn inlines(&mut self, text: &[Inline], cx: &mut Context<Terminal>) -> Div {
        let mut column = div().flex().flex_col().gap(px(DESKTOP.gap / 2.0));
        let mut runs = Runs::default();
        let mut pieces = Vec::new();
        self.runs(text, HighlightStyle::default(), &mut runs, &mut pieces);
        pieces.push(Piece::Text(runs));
        for piece in pieces {
            column = match piece {
                Piece::Text(runs) if runs.text.is_empty() => column,
                Piece::Text(runs) => column.child(self.text(runs, cx)),
                Piece::Image { source, alt } => column.child(self.image(&source, &alt)),
            };
        }
        column
    }

    fn runs(
        &mut self,
        text: &[Inline],
        style: HighlightStyle,
        runs: &mut Runs,
        pieces: &mut Vec<Piece>,
    ) {
        let tones = self.tones;
        for inline in text {
            match inline {
                Inline::Text(text) => push(runs, text, style),
                Inline::Code(code) => push(
                    runs,
                    &code.replace(' ', "\u{a0}"),
                    HighlightStyle {
                        color: Some(tones.graphite),
                        background_color: Some(tones.selection),
                        ..style
                    },
                ),
                Inline::Emphasis(inner) => self.runs(
                    inner,
                    HighlightStyle {
                        font_style: Some(FontStyle::Italic),
                        ..style
                    },
                    runs,
                    pieces,
                ),
                Inline::Strong(inner) => self.runs(
                    inner,
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..style
                    },
                    runs,
                    pieces,
                ),
                Inline::Strike(inner) => self.runs(
                    inner,
                    HighlightStyle {
                        strikethrough: Some(StrikethroughStyle {
                            thickness: px(1.0),
                            color: None,
                        }),
                        ..style
                    },
                    runs,
                    pieces,
                ),
                Inline::Link { text, .. } => {
                    let link = self.links;
                    self.links += 1;
                    let chosen = self.shown.link == Some(link);
                    let start = runs.text.len();
                    self.runs(
                        text,
                        HighlightStyle {
                            color: Some(tones.accent),
                            underline: Some(UnderlineStyle {
                                thickness: px(1.0),
                                color: Some(tones.accent),
                                wavy: false,
                            }),
                            background_color: chosen.then(|| tones.accent.opacity(0.18)),
                            ..style
                        },
                        runs,
                        pieces,
                    );
                    if runs.text.len() == start {
                        push(
                            runs,
                            "link",
                            HighlightStyle {
                                color: Some(tones.accent),
                                ..style
                            },
                        );
                    }
                    runs.links.push((start..runs.text.len(), link));
                }
                Inline::Image { source, alt } => {
                    pieces.push(Piece::Text(std::mem::take(runs)));
                    pieces.push(Piece::Image {
                        source: source.clone(),
                        alt: alt.clone(),
                    });
                }
                Inline::Break => push(runs, "\n", style),
                Inline::Html(html) => push(
                    runs,
                    html,
                    HighlightStyle {
                        color: Some(tones.graphite),
                        ..style
                    },
                ),
            }
        }
    }

    fn text(&self, runs: Runs, cx: &mut Context<Terminal>) -> AnyElement {
        let styled = StyledText::new(runs.text).with_highlights(runs.styles);
        if runs.links.is_empty() {
            return div().child(styled).into_any_element();
        }
        let leaf = self.leaf;
        let first = runs.links[0].1;
        let ranges: Vec<Range<usize>> = runs.links.iter().map(|(range, _)| range.clone()).collect();
        let places: Vec<usize> = runs.links.iter().map(|(_, link)| *link).collect();
        let terminal = cx.entity().downgrade();
        InteractiveText::new(("links", (leaf.0 << 20) | first as u64), styled)
            .on_click(ranges, move |index, _, cx| {
                let Some(link) = places.get(index).copied() else {
                    return;
                };
                let _ = terminal.update(cx, |terminal, cx| terminal.follow_link(leaf, link, cx));
            })
            .into_any_element()
    }

    fn image(&self, source: &str, alt: &str) -> Div {
        let tones = self.tones;
        let path = Path::new(&self.shown.text.path);
        let root = Path::new(&self.shown.text.root);
        let note = |text: String| {
            div()
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text)
        };
        let alt = if alt.is_empty() { "image" } else { alt };
        let picture = |image: gpui::Img| {
            div().child(image.max_w(px(MEASURE)).object_fit(ObjectFit::ScaleDown))
        };
        match markdown::image(source, path, root, self.shown.remote_allowed) {
            Load::File(file) => picture(img(file)),
            Load::Fetch(address) => match self.pictures.get(&address) {
                Some(Picture::Ready(image)) => picture(img(Arc::clone(image))),
                Some(Picture::Failed(error)) => note(format!("{alt}: {error}")),
                Some(Picture::Loading) | None => note(format!("{alt}: loading {address}")),
            },
            Load::Ask(address) => note(format!("{alt} · remote image {address}")),
            Load::Refused(reason) => note(format!("{alt}: {reason}")),
        }
    }
}

enum Piece {
    Text(Runs),
    Image { source: String, alt: String },
}

fn push(runs: &mut Runs, text: &str, style: HighlightStyle) {
    let start = runs.text.len();
    runs.text.push_str(text);
    if style != HighlightStyle::default() {
        runs.styles.push((start..runs.text.len(), style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_formats_come_from_their_first_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(ImageFormat::Png));
        assert_eq!(
            sniff(b"<?xml version=\"1.0\"?><svg>"),
            Some(ImageFormat::Svg)
        );
        assert_eq!(sniff(b"<html>not found</html>"), None);
    }
}
