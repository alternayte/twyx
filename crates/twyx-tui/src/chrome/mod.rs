//! Chrome widgets: tab bar, pane frames, status bar, hint bar, palette.
//!
//! Every colour and glyph comes from the theme tokens in `twyx_core::theme`.
//! Nothing here fills a background, except on a sheet twyx paints itself.

pub mod bars;
pub mod frames;
pub mod palette;

use ratatui::style::{Color, Modifier, Style};
use twyx_core::theme::{Face, Rgb, Sheet, Theme, Tone};

/// How many colours the outer terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Truecolor,
    Palette256,
}

/// The theme, resolved to the colours of one outer terminal.
#[derive(Debug, Clone, Copy)]
pub struct Pen {
    pub theme: Theme,
    depth: Depth,
    /// The sheet twyx paints, or `None` to draw on the user's own paper.
    sheet: Option<&'static Sheet>,
}

impl Pen {
    /// Draws on the user's own paper and ink.
    pub fn on_paper(theme: Theme, depth: Depth) -> Self {
        Self {
            theme,
            depth,
            sheet: None,
        }
    }

    /// Paints the reference sheet of the theme's variant.
    pub fn painted(theme: Theme, depth: Depth) -> Self {
        Self {
            theme,
            depth,
            sheet: Some(Sheet::of(theme.variant)),
        }
    }

    pub fn rgb(&self, rgb: Rgb) -> Color {
        match self.depth {
            Depth::Truecolor => Color::Rgb(rgb.r, rgb.g, rgb.b),
            Depth::Palette256 => Color::Indexed(rgb.to_palette()),
        }
    }

    pub fn tone(&self, tone: Tone) -> Color {
        match (tone, self.sheet) {
            (Tone::Ink, None) => Color::Reset,
            (Tone::Ink, Some(sheet)) => self.rgb(sheet.ink),
            (Tone::Rgb(rgb), _) => self.rgb(rgb),
        }
    }

    /// One of the sixteen terminal colours: the user's own, or the sheet's.
    pub fn ansi(&self, index: u8) -> Color {
        match self.sheet {
            None => Color::Indexed(index),
            Some(sheet) => self.rgb(sheet.ansi[usize::from(index & 15)]),
        }
    }

    /// The base style of every cell: the paper, when twyx paints it.
    pub fn paper(&self) -> Style {
        match self.sheet {
            None => Style::new(),
            Some(sheet) => Style::new()
                .bg(self.rgb(sheet.paper))
                .fg(self.rgb(sheet.ink)),
        }
    }

    pub fn face(&self, face: Face) -> Style {
        let style = self.paper().fg(self.tone(face.tone));
        if face.bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }

    pub fn fg(&self, tone: Tone) -> Style {
        self.paper().fg(self.tone(tone))
    }

    pub fn rule(&self) -> Style {
        self.fg(Tone::Rgb(self.theme.rule))
    }

    pub fn accent(&self) -> Style {
        self.fg(Tone::Rgb(self.theme.accent))
    }
}
