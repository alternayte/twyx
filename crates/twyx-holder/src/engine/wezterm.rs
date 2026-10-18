//! `wezterm-term` behind the engine boundary.

use std::io::Write;
use std::sync::Arc;

use wezterm_term::color::{ColorAttribute, ColorPalette};
use wezterm_term::{
    CellAttributes, Intensity, Terminal, TerminalConfiguration, TerminalSize,
    Underline as WeztermUnderline,
};

use twyx_protocol::grid::{
    Attrs, Cell, Colour, Cursor, CursorShape, Modes, MouseEncoding, MouseMode, Row, Underline,
};

use super::{Engine, EngineSignal, Geometry, Links, View};

pub struct Wezterm {
    term: Terminal,
    size: (u16, u16),
    links: Links,
}

impl std::fmt::Debug for Wezterm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wezterm")
            .field("size", &self.size)
            .field("title", &self.term.get_title())
            .finish_non_exhaustive()
    }
}

impl Wezterm {
    pub fn new(geometry: Geometry, answerback: Box<dyn Write + Send>) -> Self {
        let config = Arc::new(Configuration {
            scrollback: geometry.scrollback_lines as usize,
        });
        let size = TerminalSize {
            rows: usize::from(geometry.rows.max(1)),
            cols: usize::from(geometry.cols.max(2)),
            pixel_width: 0,
            pixel_height: 0,
            dpi: 96,
        };
        let term = Terminal::new(size, config, "twyx", env!("CARGO_PKG_VERSION"), answerback);
        Self {
            term,
            size: (geometry.cols.max(2), geometry.rows.max(1)),
            links: Links::default(),
        }
    }
}

impl Engine for Wezterm {
    fn advance(&mut self, bytes: &[u8]) -> Vec<EngineSignal> {
        self.term.advance_bytes(bytes);
        Vec::new()
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.size = (cols.max(2), rows.max(1));
        self.term.resize(TerminalSize {
            rows: usize::from(self.size.1),
            cols: usize::from(self.size.0),
            pixel_width: 0,
            pixel_height: 0,
            dpi: 96,
        });
    }

    fn view(&mut self) -> View {
        // `wezterm-term` exposes cells one at a time and only through a mutable
        // screen, so a frame is built cell by cell.
        let (cols, rows) = (usize::from(self.size.0), usize::from(self.size.1));
        let mut cells = Vec::with_capacity(rows);
        for row in 0..rows {
            let mut line = Vec::with_capacity(cols);
            for col in 0..cols {
                line.push(self.cell(col, row));
            }
            cells.push(Row { cells: line });
        }

        View {
            cols: self.size.0,
            rows: cells,
            cursor: cursor(&self.term, self.size),
            modes: modes(&self.term, self.size),
            title: self.term.get_title().to_owned(),
            scrollback: self.term.screen().scrollback_rows() as u32,
            links: self.links.take_used(),
        }
    }

    fn modes(&self) -> Modes {
        modes(&self.term, self.size)
    }

    fn line(&mut self, line: i32) -> Row {
        // Only the visible screen; this engine offers no scrollback read-out.
        let cols = usize::from(self.size.0);
        let rows = i32::from(self.size.1);
        if !(0..rows).contains(&line) {
            return Row {
                cells: vec![Cell::blank(); cols],
            };
        }
        let row = line as usize;
        Row {
            cells: (0..cols).map(|col| self.cell(col, row)).collect(),
        }
    }
}

impl Wezterm {
    fn cell(&mut self, col: usize, row: usize) -> Cell {
        let Some(cell) = self.term.screen_mut().get_cell(col, row as i64) else {
            return Cell::blank();
        };
        let attrs = cell.attrs().clone();
        let text = if cell.width() == 0 {
            String::new()
        } else {
            cell.str().to_owned()
        };
        let link = attrs
            .hyperlink()
            .map(|hyperlink| hyperlink.uri().to_owned());
        Cell {
            text,
            fg: colour(attrs.foreground()),
            bg: colour(attrs.background()),
            attrs: flags(&attrs),
            underline: underline(attrs.underline()),
            underline_colour: match attrs.underline_color() {
                ColorAttribute::Default => None,
                other => Some(colour(other)),
            },
            link: link.map(|uri| {
                let id = self.links.intern(&uri);
                self.links.mark(id);
                id
            }),
        }
    }
}

/// Only the methods twyx needs differ from the defaults.
#[derive(Debug)]
struct Configuration {
    scrollback: usize,
}

impl TerminalConfiguration for Configuration {
    fn scrollback_size(&self) -> usize {
        self.scrollback
    }

    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }

    fn enable_kitty_keyboard(&self) -> bool {
        true
    }

    fn enable_csi_u_key_encoding(&self) -> bool {
        true
    }

    fn enable_title_reporting(&self) -> bool {
        true
    }

    fn unicode_version(&self) -> wezterm_term::UnicodeVersion {
        wezterm_term::UnicodeVersion::new(14)
    }
}

fn colour(colour: ColorAttribute) -> Colour {
    match colour {
        ColorAttribute::Default => Colour::Default,
        ColorAttribute::PaletteIndex(index) => Colour::Palette(u8::from(index)),
        ColorAttribute::TrueColorWithPaletteFallback(rgb, _)
        | ColorAttribute::TrueColorWithDefaultFallback(rgb) => Colour::Rgb {
            r: to_byte(rgb.0),
            g: to_byte(rgb.1),
            b: to_byte(rgb.2),
        },
    }
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn flags(attrs: &CellAttributes) -> Attrs {
    let mut flags = Attrs::empty();
    flags.set(Attrs::BOLD, attrs.intensity() == Intensity::Bold);
    flags.set(Attrs::DIM, attrs.intensity() == Intensity::Half);
    flags.set(Attrs::ITALIC, attrs.italic());
    flags.set(Attrs::REVERSE, attrs.reverse());
    flags.set(Attrs::HIDDEN, attrs.invisible());
    flags.set(Attrs::STRIKE, attrs.strikethrough());
    flags
}

fn underline(underline: WeztermUnderline) -> Underline {
    match underline {
        WeztermUnderline::None => Underline::None,
        WeztermUnderline::Single => Underline::Single,
        WeztermUnderline::Double => Underline::Double,
        WeztermUnderline::Curly => Underline::Curly,
        WeztermUnderline::Dotted => Underline::Dotted,
        WeztermUnderline::Dashed => Underline::Dashed,
    }
}

fn cursor(term: &Terminal, size: (u16, u16)) -> Cursor {
    let position = term.cursor_pos();
    Cursor {
        col: u16::try_from(position.x)
            .unwrap_or(0)
            .min(size.0.saturating_sub(1)),
        row: u16::try_from(position.y.max(0))
            .unwrap_or(0)
            .min(size.1.saturating_sub(1)),
        // The crate does not expose the cursor shape or its visibility through
        // its public API, so a wezterm pane always reports a visible block.
        shape: CursorShape::Block,
        visible: true,
    }
}

fn modes(term: &Terminal, _size: (u16, u16)) -> Modes {
    let mouse = if term.mouse_tracking_enabled() {
        MouseMode::Drag
    } else {
        MouseMode::Off
    };
    // `get_mouse_encoding` and `get_keyboard_encoding` return types from the
    // `termwiz` crate, which this crate does not re-export, so a holder on
    // wezterm-term cannot report the mouse encoding or the kitty keyboard
    // flags without depending on termwiz as well.
    let mouse_encoding = MouseEncoding::X10;
    let kitty_keyboard = 0;
    Modes {
        bracketed_paste: term.bracketed_paste_enabled(),
        app_cursor: term.application_cursor_keys_enabled(),
        app_keypad: false,
        focus_events: term.focus_tracking_enabled(),
        mouse,
        mouse_encoding,
        kitty_keyboard,
        alt_screen: term.is_alt_screen_active(),
        insert: term.insert_mode_enabled(),
        reverse_video: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Wezterm {
        Wezterm::new(
            Geometry {
                cols: 20,
                rows: 5,
                scrollback_lines: 100,
            },
            Box::new(std::io::sink()),
        )
    }

    fn text(view: &View, row: usize) -> String {
        view.rows[row]
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn plain_text_lands_in_the_grid() {
        let mut engine = engine();
        engine.advance(b"hello");
        let view = engine.view();
        assert_eq!(text(&view, 0).trim_end(), "hello");
        assert_eq!(view.cursor.col, 5);
    }

    #[test]
    fn sgr_attributes_and_truecolor_reach_the_cells() {
        let mut engine = engine();
        engine.advance(b"\x1b[1;4;38;2;10;20;30ma\x1b[0m");
        let view = engine.view();
        let cell = &view.rows[0].cells[0];
        assert_eq!(cell.text, "a");
        assert!(cell.attrs.contains(Attrs::BOLD));
        assert_eq!(cell.underline, Underline::Single);
        assert_eq!(
            cell.fg,
            Colour::Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
    }

    #[test]
    fn bracketed_paste_and_alt_screen_are_reported() {
        let mut engine = engine();
        engine.advance(b"\x1b[?2004h\x1b[?1049h");
        let modes = engine.view().modes;
        assert!(modes.bracketed_paste);
        assert!(modes.alt_screen);
    }
}
