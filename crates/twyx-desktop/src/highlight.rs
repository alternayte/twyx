//! Syntax colours for code in the desktop client, from syntect's bundled
//! grammars. Scopes map to the sheet's own colours, so code reads in the
//! terminal's palette on either paper.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::Hsla;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use twyx_core::diff::{Hunk, LineKind};

use crate::paint::{Palette, Tones};

/// Where a token takes its colour from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Ink,
    Graphite,
    /// One of the sheet's terminal colours.
    Ansi(u8),
}

impl Slot {
    pub fn colour(self, tones: Tones, palette: Palette) -> Hsla {
        match self {
            Slot::Ink => tones.ink,
            Slot::Graphite => tones.graphite,
            Slot::Ansi(index) => palette.ansi[usize::from(index) % 16],
        }
    }

    // The theme carries a slot in a colour's red channel; alpha 0 is ink.
    fn of(colour: Color) -> Self {
        match (colour.a, colour.r) {
            (0, _) => Slot::Ink,
            (_, 255) => Slot::Graphite,
            (_, index) => Slot::Ansi(index),
        }
    }
}

/// A line's tokens: each byte range of the line and its slot.
pub type Tokens = Vec<(Range<usize>, Slot)>;

/// Scope selectors and the slot each one takes, most general first.
const SCOPES: [(&str, Slot); 8] = [
    (
        "keyword, storage.type, storage.modifier, keyword.operator.word",
        Slot::Ansi(5),
    ),
    (
        "entity.name.type, support.type, support.class",
        Slot::Ansi(6),
    ),
    (
        "entity.name.function, support.function, meta.function-call variable.function",
        Slot::Ansi(4),
    ),
    (
        "string, string punctuation.definition.string",
        Slot::Ansi(2),
    ),
    (
        "constant, constant.numeric, constant.language, constant.character",
        Slot::Ansi(3),
    ),
    (
        "entity.name.tag, entity.other.attribute-name, meta.attribute",
        Slot::Ansi(1),
    ),
    ("variable.parameter, variable.language", Slot::Ansi(1)),
    ("comment, punctuation.definition.comment", Slot::Graphite),
];

/// Each hunk's tokens, by path and hunk id; each code block's, by its
/// language and a hash of its text.
type Cache = HashMap<(String, u64), Arc<Vec<Tokens>>>;

/// The most hunks whose colours stay cached.
const CACHED: usize = 512;

#[derive(Default)]
pub struct Highlighter {
    loaded: OnceLock<(SyntaxSet, Theme)>,
    hunks: Mutex<Cache>,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter").finish_non_exhaustive()
    }
}

impl Highlighter {
    fn loaded(&self) -> &(SyntaxSet, Theme) {
        self.loaded
            .get_or_init(|| (SyntaxSet::load_defaults_newlines(), theme()))
    }

    fn syntax(&self, path: &str) -> Option<&SyntaxReference> {
        let syntaxes = &self.loaded().0;
        let name = path.rsplit('/').next().unwrap_or(path);
        let extension = name
            .rsplit_once('.')
            .map_or(name, |(_, extension)| extension);
        syntaxes
            .find_syntax_by_extension(extension)
            .or_else(|| syntaxes.find_syntax_by_extension(name))
            .filter(|syntax| syntax.name != "Plain Text")
    }

    /// Each line's tokens in a hunk of the file at `path`: the old side's
    /// lines read on from the hunk's start, the new side's likewise. `None`
    /// when no grammar fits the path.
    pub fn hunk(&self, path: &str, hunk: &Hunk) -> Option<Arc<Vec<Tokens>>> {
        let key = (path.to_owned(), hunk.id());
        if let Some(found) = self.lock().get(&key) {
            return Some(Arc::clone(found));
        }
        let syntax = self.syntax(path)?;
        let (syntaxes, theme) = self.loaded();
        let mut old = HighlightLines::new(syntax, theme);
        let mut new = HighlightLines::new(syntax, theme);
        let read = |side: &mut HighlightLines<'_>, text: &str| -> Tokens {
            let line = format!("{text}\n");
            let mut at = 0;
            let mut tokens = Tokens::new();
            for (style, piece) in side.highlight_line(&line, syntaxes).unwrap_or_default() {
                let end = (at + piece.len()).min(text.len());
                if at < end {
                    tokens.push((at..end, Slot::of(style.foreground)));
                }
                at += piece.len();
            }
            tokens
        };
        let lines: Vec<Tokens> = hunk
            .lines
            .iter()
            .map(|line| match line.kind {
                LineKind::Context => {
                    read(&mut old, &line.text);
                    read(&mut new, &line.text)
                }
                LineKind::Removed => read(&mut old, &line.text),
                LineKind::Added => read(&mut new, &line.text),
                LineKind::NoNewline => Tokens::new(),
            })
            .collect();
        let lines = Arc::new(lines);
        let mut cache = self.lock();
        if cache.len() >= CACHED {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&lines));
        Some(lines)
    }

    /// Each line's tokens in a code block in `language`, a fence's info
    /// word such as `rust` or `sh`. `None` when no grammar knows it.
    pub fn code(&self, language: &str, text: &str) -> Option<Arc<Vec<Tokens>>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let key = (format!("```{language}"), hasher.finish());
        if let Some(found) = self.lock().get(&key) {
            return Some(Arc::clone(found));
        }
        let (syntaxes, theme) = self.loaded();
        let syntax = syntaxes
            .find_syntax_by_token(language)
            .filter(|syntax| syntax.name != "Plain Text")?;
        let mut lines = HighlightLines::new(syntax, theme);
        let tokens: Vec<Tokens> = text
            .lines()
            .map(|text| {
                let line = format!("{text}\n");
                let mut at = 0;
                let mut tokens = Tokens::new();
                for (style, piece) in lines.highlight_line(&line, syntaxes).unwrap_or_default() {
                    let end = (at + piece.len()).min(text.len());
                    if at < end {
                        tokens.push((at..end, Slot::of(style.foreground)));
                    }
                    at += piece.len();
                }
                tokens
            })
            .collect();
        let tokens = Arc::new(tokens);
        let mut cache = self.lock();
        if cache.len() >= CACHED {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&tokens));
        Some(tokens)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.hunks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn theme() -> Theme {
    let colour = |slot: Slot| match slot {
        Slot::Ink => Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        },
        Slot::Graphite => Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        },
        Slot::Ansi(index) => Color {
            r: index,
            g: 0,
            b: 0,
            a: 255,
        },
    };
    Theme {
        name: Some("twyx".to_owned()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(colour(Slot::Ink)),
            ..ThemeSettings::default()
        },
        scopes: SCOPES
            .iter()
            .filter_map(|(scopes, slot)| {
                Some(ThemeItem {
                    scope: scopes.parse::<ScopeSelectors>().ok()?,
                    style: StyleModifier {
                        foreground: Some(colour(*slot)),
                        background: None,
                        font_style: None,
                    },
                })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_strings_and_comments_take_their_slots() {
        let diff = twyx_core::diff::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,2 @@\n fn main() {}\n+let s = \"hi\"; // note\n",
        );
        let highlighter = Highlighter::default();
        let hunk = &diff[0].hunks[0];
        let lines = highlighter.hunk("src/a.rs", hunk).expect("a Rust grammar");
        let text = &hunk.lines[1].text;
        let slot_of = |word: &str| {
            let at = text.find(word).expect("the word");
            lines[1]
                .iter()
                .find(|(range, _)| range.contains(&at))
                .map(|(_, slot)| *slot)
        };
        assert_eq!(slot_of("let"), Some(Slot::Ansi(5)));
        assert_eq!(slot_of("hi"), Some(Slot::Ansi(2)));
        assert_eq!(slot_of("note"), Some(Slot::Graphite));
        assert!(highlighter.hunk("notes.unknownext", hunk).is_none());
    }
}
