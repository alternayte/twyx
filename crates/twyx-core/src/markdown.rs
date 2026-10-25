//! Markdown documents: one tree that both clients render, and the rules for
//! what a document may load and open.
//!
//! HTML in a document is text. It never runs, and an image or link written
//! in HTML is not one (Invariant 18). An image or link loads or opens only
//! through [`image`] and [`link`].

use std::iter::Peekable;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        text: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Code {
        language: Option<String>,
        text: String,
    },
    Quote(Vec<Block>),
    List {
        /// The first number of an ordered list.
        start: Option<u64>,
        items: Vec<Item>,
    },
    Table {
        align: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    /// HTML, shown as its source.
    Html(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Whether a task item is done; `None` for a plain item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link {
        target: String,
        text: Vec<Inline>,
    },
    Image {
        source: String,
        alt: String,
    },
    /// A hard line break.
    Break,
    /// Inline HTML, shown as its source.
    Html(String),
}

impl Inline {
    /// The text as read, without marks.
    pub fn plain(inlines: &[Inline]) -> String {
        let mut text = String::new();
        for inline in inlines {
            match inline {
                Inline::Text(part) | Inline::Code(part) | Inline::Html(part) => text.push_str(part),
                Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                    text.push_str(&Self::plain(inner));
                }
                Inline::Link { text: inner, .. } => text.push_str(&Self::plain(inner)),
                Inline::Image { alt, .. } => text.push_str(alt),
                Inline::Break => text.push('\n'),
            }
        }
        text
    }
}

impl Document {
    pub fn parse(text: &str) -> Self {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_GFM;
        let mut events = Parser::new_ext(text, options).peekable();
        Self {
            blocks: blocks(&mut events, None),
        }
    }

    /// Every link target, in reading order.
    pub fn links(&self) -> Vec<String> {
        let mut found = Vec::new();
        visit(&self.blocks, &mut |inline| {
            if let Inline::Link { target, .. } = inline {
                found.push(target.clone());
            }
        });
        found
    }

    /// The top-level block that holds the link with this place in reading
    /// order.
    pub fn block_of_link(&self, link: usize) -> Option<usize> {
        let mut seen = 0;
        for (index, block) in self.blocks.iter().enumerate() {
            visit(std::slice::from_ref(block), &mut |inline| {
                if matches!(inline, Inline::Link { .. }) {
                    seen += 1;
                }
            });
            if seen > link {
                return Some(index);
            }
        }
        None
    }

    /// The top-level block of each heading, in order.
    pub fn heading_blocks(&self) -> Vec<usize> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| matches!(block, Block::Heading { .. }))
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether any image is remote, so the reader can be offered to allow
    /// it.
    pub fn has_remote_images(&self) -> bool {
        let mut remote = false;
        visit(&self.blocks, &mut |inline| {
            if let Inline::Image { source, .. } = inline {
                remote |= is_remote(source);
            }
        });
        remote
    }

    /// The anchor of each heading, as GitHub makes it, in order.
    pub fn anchors(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut anchors = Vec::new();
        for block in &self.blocks {
            if let Block::Heading { text, .. } = block {
                let base = slug(&Inline::plain(text));
                let count = seen.iter().filter(|each| **each == base).count();
                seen.push(base.clone());
                anchors.push(if count == 0 {
                    base
                } else {
                    format!("{base}-{count}")
                });
            }
        }
        anchors
    }
}

fn visit(blocks: &[Block], each: &mut impl FnMut(&Inline)) {
    fn inlines(list: &[Inline], each: &mut impl FnMut(&Inline)) {
        for inline in list {
            each(inline);
            match inline {
                Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strike(inner) => {
                    inlines(inner, each);
                }
                Inline::Link { text, .. } => inlines(text, each),
                _ => {}
            }
        }
    }
    for block in blocks {
        match block {
            Block::Heading { text, .. } | Block::Paragraph(text) => inlines(text, each),
            Block::Quote(inner) => visit(inner, each),
            Block::List { items, .. } => {
                for item in items {
                    visit(&item.blocks, each);
                }
            }
            Block::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    inlines(cell, each);
                }
            }
            Block::Code { .. } | Block::Rule | Block::Html(_) => {}
        }
    }
}

type Events<'a> = Peekable<Parser<'a>>;

/// Blocks until `until` ends, or to the end. Inline content outside a
/// paragraph (a tight list item's) becomes a paragraph.
fn blocks(events: &mut Events, until: Option<TagEnd>) -> Vec<Block> {
    let mut found = Vec::new();
    while let Some(event) = events.peek() {
        if inline_start(event) {
            let text = inlines(events, None);
            if !text.is_empty() {
                found.push(Block::Paragraph(text));
            }
            continue;
        }
        let Some(event) = events.next() else {
            break;
        };
        match event {
            Event::End(end) if Some(end) == until => break,
            Event::Start(tag) => {
                if let Some(block) = block(events, tag) {
                    found.push(block);
                }
            }
            Event::Rule => found.push(Block::Rule),
            Event::Html(html) => found.push(Block::Html(html.into_string())),
            Event::DisplayMath(math) => found.push(Block::Code {
                language: None,
                text: math.into_string(),
            }),
            _ => {}
        }
    }
    found
}

fn block(events: &mut Events, tag: Tag) -> Option<Block> {
    Some(match tag {
        Tag::Paragraph => Block::Paragraph(inlines(events, Some(TagEnd::Paragraph))),
        Tag::Heading { level, .. } => Block::Heading {
            level: level as u8,
            text: inlines(events, Some(TagEnd::Heading(level))),
        },
        Tag::BlockQuote(kind) => Block::Quote(blocks(events, Some(TagEnd::BlockQuote(kind)))),
        Tag::CodeBlock(kind) => {
            let language = match kind {
                CodeBlockKind::Fenced(info) => info
                    .split_whitespace()
                    .next()
                    .map(str::to_owned)
                    .filter(|language| !language.is_empty()),
                CodeBlockKind::Indented => None,
            };
            let mut text = String::new();
            for event in events.by_ref() {
                match event {
                    Event::Text(part) => text.push_str(&part),
                    Event::End(TagEnd::CodeBlock) => break,
                    _ => {}
                }
            }
            if text.ends_with('\n') {
                text.pop();
            }
            Block::Code { language, text }
        }
        Tag::HtmlBlock => {
            let mut html = String::new();
            for event in events.by_ref() {
                match event {
                    Event::Html(part) | Event::Text(part) => html.push_str(&part),
                    Event::End(TagEnd::HtmlBlock) => break,
                    _ => {}
                }
            }
            Block::Html(html.trim_end().to_owned())
        }
        Tag::List(start) => {
            let mut items = Vec::new();
            while let Some(event) = events.next() {
                match event {
                    Event::Start(Tag::Item) => {
                        let task = match events.peek() {
                            Some(Event::TaskListMarker(done)) => {
                                let done = *done;
                                events.next();
                                Some(done)
                            }
                            _ => None,
                        };
                        items.push(Item {
                            task,
                            blocks: blocks(events, Some(TagEnd::Item)),
                        });
                    }
                    Event::End(TagEnd::List(_)) => break,
                    _ => {}
                }
            }
            Block::List { start, items }
        }
        Tag::Table(align) => {
            let align = align
                .into_iter()
                .map(|align| match align {
                    Alignment::None => Align::None,
                    Alignment::Left => Align::Left,
                    Alignment::Center => Align::Center,
                    Alignment::Right => Align::Right,
                })
                .collect();
            let mut head = Vec::new();
            let mut rows = Vec::new();
            let mut row: Vec<Vec<Inline>> = Vec::new();
            while let Some(event) = events.next() {
                match event {
                    Event::Start(Tag::TableCell) => {
                        row.push(inlines(events, Some(TagEnd::TableCell)));
                    }
                    Event::End(TagEnd::TableHead) => head = std::mem::take(&mut row),
                    Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
                    Event::End(TagEnd::Table) => break,
                    _ => {}
                }
            }
            Block::Table { align, head, rows }
        }
        Tag::FootnoteDefinition(label) => {
            let mut inner = blocks(events, Some(TagEnd::FootnoteDefinition));
            if let Some(Block::Paragraph(text)) = inner.first_mut() {
                text.insert(0, Inline::Text(format!("[^{label}]: ")));
            }
            Block::Quote(inner)
        }
        Tag::MetadataBlock(kind) => {
            for event in events.by_ref() {
                if event == Event::End(TagEnd::MetadataBlock(kind)) {
                    break;
                }
            }
            return None;
        }
        other => {
            // Definition lists and the rest: keep their text.
            let end = other.to_end();
            Block::Quote(blocks(events, Some(end)))
        }
    })
}

fn inline_start(event: &Event) -> bool {
    match event {
        Event::Text(_)
        | Event::Code(_)
        | Event::InlineHtml(_)
        | Event::InlineMath(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::FootnoteReference(_) => true,
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Superscript
                | Tag::Subscript
                | Tag::Link { .. }
                | Tag::Image { .. }
        ),
        _ => false,
    }
}

/// Inline content until `until` ends; with `None`, until the next event
/// that is not inline.
fn inlines(events: &mut Events, until: Option<TagEnd>) -> Vec<Inline> {
    let mut found: Vec<Inline> = Vec::new();
    loop {
        if until.is_none() && !events.peek().is_some_and(inline_start) {
            break;
        }
        let Some(event) = events.next() else {
            break;
        };
        match event {
            Event::End(end) if Some(end) == until => break,
            Event::Text(text) => push_text(&mut found, &text),
            Event::SoftBreak => push_text(&mut found, " "),
            Event::HardBreak => found.push(Inline::Break),
            Event::Code(code) => found.push(Inline::Code(code.into_string())),
            Event::InlineMath(math) => found.push(Inline::Code(math.into_string())),
            Event::InlineHtml(html) | Event::Html(html) => {
                found.push(Inline::Html(html.into_string()));
            }
            Event::FootnoteReference(label) => push_text(&mut found, &format!("[^{label}]")),
            Event::Start(Tag::Emphasis) => {
                found.push(Inline::Emphasis(inlines(events, Some(TagEnd::Emphasis))));
            }
            Event::Start(Tag::Strong) => {
                found.push(Inline::Strong(inlines(events, Some(TagEnd::Strong))));
            }
            Event::Start(Tag::Strikethrough) => {
                found.push(Inline::Strike(inlines(events, Some(TagEnd::Strikethrough))));
            }
            Event::Start(Tag::Superscript) => {
                found.extend(inlines(events, Some(TagEnd::Superscript)));
            }
            Event::Start(Tag::Subscript) => found.extend(inlines(events, Some(TagEnd::Subscript))),
            Event::Start(Tag::Link { dest_url, .. }) => found.push(Inline::Link {
                target: dest_url.into_string(),
                text: inlines(events, Some(TagEnd::Link)),
            }),
            Event::Start(Tag::Image { dest_url, .. }) => found.push(Inline::Image {
                source: dest_url.into_string(),
                alt: Inline::plain(&inlines(events, Some(TagEnd::Image))),
            }),
            _ => {}
        }
    }
    found
}

fn push_text(found: &mut Vec<Inline>, text: &str) {
    match found.last_mut() {
        Some(Inline::Text(last)) => last.push_str(text),
        _ => found.push(Inline::Text(text.to_owned())),
    }
}

/// A heading's anchor: lower case, spaces as hyphens, only letters, digits,
/// hyphens and underscores kept.
pub fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter_map(|ch| match ch {
            ' ' => Some('-'),
            ch if ch.is_alphanumeric() || ch == '-' || ch == '_' => Some(ch),
            _ => None,
        })
        .collect()
}

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file inside the project.
    File {
        path: PathBuf,
        /// The heading anchor after `#`, if any.
        anchor: Option<String>,
    },
    /// A heading of this document.
    Anchor(String),
    /// An `http` or `https` address; it opens only after the client shows
    /// it and the user agrees.
    Remote(String),
    /// Anything else; the reason says why it does not open.
    Refused(String),
}

/// How a client gets an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    /// A file inside the project.
    File(PathBuf),
    /// An `http` or `https` address the user allowed.
    Fetch(String),
    /// An `http` or `https` address the user has not allowed yet.
    Ask(String),
    Refused(String),
}

/// Whether a file is Markdown, by its extension.
pub fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["md", "markdown", "mdown", "mkd"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn is_remote(target: &str) -> bool {
    let scheme = target
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https"))
}

/// Where the link `target` in the document at `document` goes. Local
/// files resolve against the document's directory, or against `root` when
/// the target starts with `/`, and must stay inside `root`.
pub fn link(target: &str, document: &Path, root: &Path) -> Target {
    if is_remote(target) {
        return Target::Remote(target.to_owned());
    }
    if let Some(anchor) = target.strip_prefix('#') {
        return Target::Anchor(decode(anchor));
    }
    let (path, anchor) = match target.split_once('#') {
        Some((path, anchor)) => (path, Some(decode(anchor))),
        None => (target, None),
    };
    match local(path, document, root) {
        Ok(path) => Target::File { path, anchor },
        Err(reason) => Target::Refused(reason),
    }
}

/// How the image `source` in the document at `document` loads.
/// `remote_allowed` is the user's permission for this document.
pub fn image(source: &str, document: &Path, root: &Path, remote_allowed: bool) -> Load {
    if is_remote(source) {
        return if remote_allowed {
            Load::Fetch(source.to_owned())
        } else {
            Load::Ask(source.to_owned())
        };
    }
    match local(source, document, root) {
        Ok(path) => Load::File(path),
        Err(reason) => Load::Refused(reason),
    }
}

fn local(target: &str, document: &Path, root: &Path) -> Result<PathBuf, String> {
    let target = target.split_once('?').map_or(target, |(path, _)| path);
    let path = match target.split_once(':') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("file") => {
            PathBuf::from(decode(rest.trim_start_matches("//")))
        }
        // A Windows drive letter is not a scheme, but twyx has none.
        Some((scheme, _))
            if scheme
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "+-.".contains(ch)) =>
        {
            return Err(format!("{scheme}: links do not open"));
        }
        _ => {
            let decoded = decode(target);
            match decoded.strip_prefix('/') {
                Some(from_root) => root.join(from_root),
                None => document.parent().unwrap_or(root).join(decoded),
            }
        }
    };
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    let path = path
        .canonicalize()
        .map_err(|_| format!("{} does not exist", lexical(&path).display()))?;
    if path.starts_with(&root) {
        Ok(path)
    } else {
        Err(format!("{} is outside the project", path.display()))
    }
}

/// `path` with `.` and `..` taken out, for messages.
fn lexical(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    clean
}

/// Percent-decoding, as link targets carry it.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let Some(hex) = text.get(at + 1..at + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text(text.to_owned())
    }

    #[test]
    fn a_document_parses_into_one_tree() {
        let document = Document::parse(
            "# Title\n\nSome *soft*\nand **hard** `code`.\n\n\
             - [x] done\n- [ ] open\n  1. inner\n\n\
             | a | b |\n|:--|--:|\n| 1 | 2 |\n\n\
             ```rust title\nfn main() {}\n```\n\n> quoted\n\n---\n",
        );
        assert_eq!(
            document.blocks,
            [
                Block::Heading {
                    level: 1,
                    text: vec![text("Title")]
                },
                Block::Paragraph(vec![
                    text("Some "),
                    Inline::Emphasis(vec![text("soft")]),
                    text(" and "),
                    Inline::Strong(vec![text("hard")]),
                    text(" "),
                    Inline::Code("code".to_owned()),
                    text("."),
                ]),
                Block::List {
                    start: None,
                    items: vec![
                        Item {
                            task: Some(true),
                            blocks: vec![Block::Paragraph(vec![text("done")])],
                        },
                        Item {
                            task: Some(false),
                            blocks: vec![
                                Block::Paragraph(vec![text("open")]),
                                Block::List {
                                    start: Some(1),
                                    items: vec![Item {
                                        task: None,
                                        blocks: vec![Block::Paragraph(vec![text("inner")])],
                                    }],
                                },
                            ],
                        },
                    ],
                },
                Block::Table {
                    align: vec![Align::Left, Align::Right],
                    head: vec![vec![text("a")], vec![text("b")]],
                    rows: vec![vec![vec![text("1")], vec![text("2")]]],
                },
                Block::Code {
                    language: Some("rust".to_owned()),
                    text: "fn main() {}".to_owned(),
                },
                Block::Quote(vec![Block::Paragraph(vec![text("quoted")])]),
                Block::Rule,
            ]
        );
    }

    #[test]
    fn headings_get_github_anchors() {
        let document = Document::parse("# Getting Started!\n\n## Use\n\n## Use\n");
        assert_eq!(document.anchors(), ["getting-started", "use", "use-1"]);
    }

    fn project() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("twyx-markdown-{}", std::process::id()));
        std::fs::create_dir_all(root.join("docs/img")).unwrap();
        std::fs::write(root.join("docs/guide.md"), "").unwrap();
        std::fs::write(root.join("docs/img/a b.png"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        let root = root.canonicalize().unwrap();
        (root.join("docs/guide.md"), root)
    }

    #[test]
    fn local_targets_resolve_inside_the_project_only() {
        let (document, root) = project();
        assert_eq!(
            link("../README.md#install", &document, &root),
            Target::File {
                path: root.join("README.md"),
                anchor: Some("install".to_owned()),
            }
        );
        assert_eq!(
            link("/docs/guide.md", &document, &root),
            Target::File {
                path: document.clone(),
                anchor: None,
            }
        );
        assert_eq!(
            image("img/a%20b.png", &document, &root, false),
            Load::File(root.join("docs/img/a b.png"))
        );
        assert_eq!(
            link("#use", &document, &root),
            Target::Anchor("use".to_owned())
        );
        assert!(matches!(
            link("../../../../etc/hosts", &document, &root),
            Target::Refused(_)
        ));
        assert!(matches!(
            link("file:///etc/hosts", &document, &root),
            Target::Refused(_)
        ));
        assert!(matches!(
            link("missing.md", &document, &root),
            Target::Refused(_)
        ));
        assert!(matches!(
            link("vscode://open", &document, &root),
            Target::Refused(_)
        ));
    }

    #[test]
    fn invariant_18_a_document_runs_no_html_and_fetches_nothing_unallowed() {
        let (document_path, root) = project();
        let document = Document::parse(
            "<script>alert(1)</script>\n\n\
             Text <img src=\"https://example.com/i.png\"> more.\n\n\
             ![remote](https://example.com/r.png) [site](https://example.com)\n",
        );
        // HTML stays its source text: no image, no link, no script.
        assert_eq!(
            document.blocks[0],
            Block::Html("<script>alert(1)</script>".to_owned())
        );
        assert_eq!(
            document.blocks[1],
            Block::Paragraph(vec![
                text("Text "),
                Inline::Html("<img src=\"https://example.com/i.png\">".to_owned()),
                text(" more."),
            ])
        );
        let Block::Paragraph(last) = &document.blocks[2] else {
            panic!("{:?}", document.blocks[2]);
        };
        let Inline::Image { source, .. } = &last[0] else {
            panic!("{last:?}");
        };
        assert!(document.has_remote_images());
        assert_eq!(
            image(source, &document_path, &root, false),
            Load::Ask(source.clone()),
            "a remote image waits for the user"
        );
        assert_eq!(
            image(source, &document_path, &root, true),
            Load::Fetch(source.clone())
        );
        assert_eq!(
            link(&document.links()[0], &document_path, &root),
            Target::Remote("https://example.com".to_owned()),
            "a remote link only names its address; the client asks first"
        );
    }
}
