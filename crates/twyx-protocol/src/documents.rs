//! Document leaves as both clients show them: each leaf's file and its
//! tree, the chosen link, the reader's permission for remote images, and
//! the keys.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use twyx_core::keys::{Chord, Key};
use twyx_core::layout::LeafId;
use twyx_core::markdown::{self, Document, Target};

use crate::client::{DocumentText, ToDaemon};

/// One document leaf's file as last read.
#[derive(Debug, Clone)]
pub struct Shown {
    pub text: DocumentText,
    pub document: Document,
    /// The chosen link, by its place in reading order.
    pub link: Option<usize>,
    /// The reader allowed this leaf's remote images.
    pub remote_allowed: bool,
    /// A remote address the reader chose; a second enter opens it.
    pub confirm: Option<String>,
}

/// What a key in a document asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum DocKey {
    /// Only the view changed.
    Stay,
    /// Scroll by lines.
    Scroll(i32),
    /// Scroll by pages.
    Page(i32),
    Top,
    Bottom,
    /// Bring the link with this place in reading order into view.
    Link(usize),
    /// Bring the heading with this place into view.
    Heading(usize),
    Send(ToDaemon),
    /// Open this `http` or `https` address in the browser; the reader saw
    /// it and agreed.
    Browse(String),
    Notice(String),
}

#[derive(Debug, Default)]
pub struct Documents {
    pub shown: HashMap<LeafId, Shown>,
    asked: HashMap<LeafId, Instant>,
}

impl Documents {
    /// Requests for the shown document leaves not asked about for a
    /// refresh interval.
    pub fn wanted(&mut self, leaves: &[LeafId]) -> Vec<ToDaemon> {
        let mut requests = Vec::new();
        for leaf in leaves {
            let fresh = self
                .asked
                .get(leaf)
                .is_some_and(|at| at.elapsed() < twyx_core::tuning::DOCUMENT_REFRESH);
            if !fresh {
                self.asked.insert(*leaf, Instant::now());
                requests.push(ToDaemon::Document {
                    leaf: *leaf,
                    since: self.shown.get(leaf).and_then(|shown| shown.text.modified),
                });
            }
        }
        requests
    }

    pub fn answer(&mut self, text: DocumentText) {
        let document = Document::parse(&text.text);
        match self.shown.get_mut(&text.leaf) {
            Some(shown) => {
                let links = document.links().len();
                shown.link = shown.link.filter(|link| *link < links);
                shown.document = document;
                shown.text = text;
            }
            None => {
                self.shown.insert(
                    text.leaf,
                    Shown {
                        text,
                        document,
                        link: None,
                        remote_allowed: false,
                        confirm: None,
                    },
                );
            }
        }
    }

    /// Takes a key for the document leaf `leaf`.
    pub fn key(&mut self, leaf: LeafId, chord: Chord) -> DocKey {
        let Some(shown) = self.shown.get_mut(&leaf) else {
            return DocKey::Stay;
        };
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        if let Some(address) = shown.confirm.take() {
            return match chord.key {
                Key::Enter => DocKey::Browse(address),
                _ => DocKey::Stay,
            };
        }
        let links = shown.document.links();
        match (chord.key, typed) {
            (Key::Down, _) | (_, Some('j')) => DocKey::Scroll(1),
            (Key::Up, _) | (_, Some('k')) => DocKey::Scroll(-1),
            (Key::PageDown, _) | (_, Some(' ')) => DocKey::Page(1),
            (Key::Char('d'), _) if chord.ctrl => DocKey::Page(1),
            (Key::PageUp, _) => DocKey::Page(-1),
            (Key::Char('u'), _) if chord.ctrl => DocKey::Page(-1),
            (Key::Home, _) | (_, Some('g')) => DocKey::Top,
            (Key::End, _) | (_, Some('G')) => DocKey::Bottom,
            (Key::Tab, _) if links.is_empty() => DocKey::Notice("no links".to_owned()),
            (Key::Tab, _) => {
                let next = match (shown.link, chord.shift) {
                    (None, false) => 0,
                    (None, true) => links.len() - 1,
                    (Some(link), false) => (link + 1) % links.len(),
                    (Some(link), true) => (link + links.len() - 1) % links.len(),
                };
                shown.link = Some(next);
                DocKey::Link(next)
            }
            (Key::Escape, _) => {
                shown.link = None;
                DocKey::Stay
            }
            (Key::Enter, _) => {
                let Some(target) = shown.link.and_then(|link| links.get(link)) else {
                    return DocKey::Notice("tab chooses a link".to_owned());
                };
                follow(shown, target)
            }
            (_, Some('e')) => DocKey::Send(ToDaemon::OpenInEditor {
                path: shown.text.path.clone(),
                line: None,
            }),
            (_, Some('a')) if shown.document.has_remote_images() => {
                shown.remote_allowed = !shown.remote_allowed;
                DocKey::Stay
            }
            (_, Some('a')) => DocKey::Notice("no remote images".to_owned()),
            _ => DocKey::Stay,
        }
    }
}

fn follow(shown: &mut Shown, target: &str) -> DocKey {
    let path = Path::new(&shown.text.path);
    match markdown::link(target, path, Path::new(&shown.text.root)) {
        Target::File { path, .. } => {
            DocKey::Send(crate::files::open(path.to_string_lossy().into_owned()))
        }
        Target::Anchor(anchor) => match shown
            .document
            .anchors()
            .iter()
            .position(|each| *each == anchor)
        {
            Some(heading) => DocKey::Heading(heading),
            None => DocKey::Notice(format!("no heading #{anchor}")),
        },
        Target::Remote(address) => {
            shown.confirm = Some(address);
            DocKey::Stay
        }
        Target::Refused(reason) => DocKey::Notice(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn documents(text: &str) -> (Documents, LeafId, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("twyx-documents-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("other.md"), "").unwrap();
        std::fs::write(root.join("code.rs"), "").unwrap();
        let root = root.canonicalize().unwrap();
        let mut documents = Documents::default();
        let leaf = LeafId(4);
        documents.answer(DocumentText {
            leaf,
            path: root.join("a.md").to_string_lossy().into_owned(),
            root: root.to_string_lossy().into_owned(),
            text: text.to_owned(),
            modified: Some(1),
            truncated: false,
            error: None,
        });
        (documents, leaf, root)
    }

    #[test]
    fn tab_chooses_links_and_enter_follows_them() {
        let (mut documents, leaf, root) =
            documents("# Top\n\n[a](other.md) [b](code.rs) [c](#top) [d](https://example.com)\n");
        let tab = Chord::plain(Key::Tab);
        let enter = Chord::plain(Key::Enter);
        assert_eq!(documents.key(leaf, tab), DocKey::Link(0));
        assert_eq!(
            documents.key(leaf, enter),
            DocKey::Send(ToDaemon::OpenDocument {
                path: root.join("other.md").to_string_lossy().into_owned(),
            }),
            "a Markdown file opens as a document"
        );
        documents.key(leaf, tab);
        assert_eq!(
            documents.key(leaf, enter),
            DocKey::Send(ToDaemon::OpenInEditor {
                path: root.join("code.rs").to_string_lossy().into_owned(),
                line: None,
            })
        );
        documents.key(leaf, tab);
        assert_eq!(documents.key(leaf, enter), DocKey::Heading(0));
        documents.key(leaf, tab);
        assert_eq!(
            documents.key(leaf, enter),
            DocKey::Stay,
            "a remote link shows its address first"
        );
        assert_eq!(
            documents.shown[&leaf].confirm.as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            documents.key(leaf, enter),
            DocKey::Browse("https://example.com".to_owned())
        );
        documents.key(leaf, enter);
        assert_eq!(
            documents.key(leaf, Chord::plain(Key::Escape)),
            DocKey::Stay,
            "any other key drops the address"
        );
        assert_eq!(documents.shown[&leaf].confirm, None);
    }

    #[test]
    fn the_reader_allows_remote_images_per_leaf() {
        let (mut documents, leaf, _) = documents("![x](https://example.com/x.png)\n");
        assert!(!documents.shown[&leaf].remote_allowed);
        documents.key(leaf, Chord::plain(Key::Char('a')));
        assert!(documents.shown[&leaf].remote_allowed);
        assert_eq!(
            documents.wanted(&[leaf]),
            [ToDaemon::Document {
                leaf,
                since: Some(1)
            }]
        );
        assert_eq!(documents.wanted(&[leaf]), [], "asked a moment ago");
    }
}
