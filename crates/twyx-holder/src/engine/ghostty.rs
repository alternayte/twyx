//! `libghostty-vt` behind the engine boundary.

use std::io::Write;

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::style::{Style, StyleColor, Underline as GhosttyUnderline};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};

use twyx_protocol::grid::{
    Attrs, Cell, Colour, Cursor, CursorShape, Modes, MouseEncoding, MouseMode, Row, Underline,
};

use super::{Engine, EngineSignal, Geometry, View};

pub struct Ghostty {
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    size: (u16, u16),
}

impl std::fmt::Debug for Ghostty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ghostty")
            .field("size", &self.size)
            .field("title", &self.term.title().unwrap_or_default())
            .finish_non_exhaustive()
    }
}

impl Ghostty {
    pub fn new(geometry: Geometry, mut answerback: Box<dyn Write + Send>) -> Option<Self> {
        let mut term = Terminal::new(TerminalOptions {
            cols: geometry.cols.max(2),
            rows: geometry.rows.max(1),
            max_scrollback: geometry.scrollback_lines as usize,
        })
        .ok()?;
        term.on_pty_write(move |_, reply: &[u8]| {
            let _ = answerback.write_all(reply);
        })
        .ok()?;
        let render = RenderState::new().ok()?;
        Some(Self {
            term,
            render,
            size: (geometry.cols.max(2), geometry.rows.max(1)),
        })
    }

    fn blank_view(&self) -> View {
        let cols = usize::from(self.size.0);
        View {
            cols: self.size.0,
            rows: vec![
                Row {
                    cells: vec![Cell::blank(); cols],
                };
                usize::from(self.size.1)
            ],
            cursor: Cursor {
                col: 0,
                row: 0,
                shape: CursorShape::Block,
                visible: true,
            },
            modes: Modes::default(),
            title: String::new(),
            scrollback: 0,
            links: Vec::new(),
        }
    }
}

impl Engine for Ghostty {
    fn advance(&mut self, bytes: &[u8]) -> Vec<EngineSignal> {
        self.term.vt_write(bytes);
        Vec::new()
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.size = (cols.max(2), rows.max(1));
        let _ = self.term.resize(self.size.0, self.size.1, 0, 0);
    }

    fn view(&mut self) -> View {
        let Ok(snapshot) = self.render.update(&self.term) else {
            return self.blank_view();
        };
        let Ok(mut rows) = RowIterator::new() else {
            return self.blank_view();
        };
        let Ok(mut row_iteration) = rows.update(&snapshot) else {
            return self.blank_view();
        };
        let Ok(mut cells) = CellIterator::new() else {
            return self.blank_view();
        };

        let cols = usize::from(self.size.0);
        let mut grid = Vec::with_capacity(usize::from(self.size.1));
        while let Some(row) = row_iteration.next() {
            let mut line = Vec::with_capacity(cols);
            let Ok(mut cell_iteration) = cells.update(row) else {
                break;
            };
            while let Some(cell) = cell_iteration.next() {
                let style = cell.style().unwrap_or_default();
                // A spacer after a wide cell has no graphemes of its own.
                let text = cell
                    .graphemes()
                    .map(|graphemes| graphemes.into_iter().collect::<String>())
                    .unwrap_or_default();
                line.push(Cell {
                    text,
                    fg: colour(style.fg_color),
                    bg: colour(style.bg_color),
                    attrs: attrs(&style),
                    underline: underline(style.underline),
                    underline_colour: match style.underline_color {
                        StyleColor::None => None,
                        other => Some(colour(other)),
                    },
                    link: None,
                });
            }
            grid.push(Row { cells: line });
        }

        let cursor = snapshot.cursor_viewport().ok().flatten();
        View {
            cols: self.size.0,
            rows: grid,
            cursor: Cursor {
                col: cursor.as_ref().map_or(0, |cursor| cursor.x),
                row: cursor.as_ref().map_or(0, |cursor| cursor.y),
                shape: CursorShape::Block,
                visible: snapshot.cursor_visible().unwrap_or(true),
            },
            modes: self.modes(),
            title: self.term.title().unwrap_or_default().to_owned(),
            scrollback: self.term.scrollback_rows().unwrap_or(0) as u32,
            links: Vec::new(),
        }
    }

    fn modes(&self) -> Modes {
        Modes {
            mouse: if self.term.is_mouse_tracking().unwrap_or(false) {
                MouseMode::Drag
            } else {
                MouseMode::Off
            },
            // `Terminal::mode` and `get_mouse_encoding` cannot be used here:
            // see the note in the gate notes. Every mode that comes from a
            // `Mode` query therefore reports its default.
            mouse_encoding: MouseEncoding::Sgr,
            kitty_keyboard: self
                .term
                .kitty_keyboard_flags()
                .map(|flags| flags.bits())
                .unwrap_or(0),
            ..Modes::default()
        }
    }

    fn line(&mut self, line: i32) -> Row {
        // Only the visible screen; scrollback read-out is not wired yet.
        let cols = usize::from(self.size.0);
        let view = self.view();
        match view.rows.get(line.max(0) as usize) {
            Some(row) => Row {
                cells: row.cells.clone(),
            },
            None => Row {
                cells: vec![Cell::blank(); cols],
            },
        }
    }
}

fn colour(colour: StyleColor) -> Colour {
    match colour {
        StyleColor::None => Colour::Default,
        StyleColor::Palette(index) => Colour::Palette(index.0),
        StyleColor::Rgb(rgb) => Colour::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        },
    }
}

fn attrs(style: &Style) -> Attrs {
    let mut attrs = Attrs::empty();
    attrs.set(Attrs::BOLD, style.bold);
    attrs.set(Attrs::DIM, style.faint);
    attrs.set(Attrs::ITALIC, style.italic);
    attrs.set(Attrs::BLINK, style.blink);
    attrs.set(Attrs::REVERSE, style.inverse);
    attrs.set(Attrs::HIDDEN, style.invisible);
    attrs.set(Attrs::STRIKE, style.strikethrough);
    attrs
}

fn underline(underline: GhosttyUnderline) -> Underline {
    match underline {
        GhosttyUnderline::None => Underline::None,
        GhosttyUnderline::Single => Underline::Single,
        GhosttyUnderline::Double => Underline::Double,
        GhosttyUnderline::Curly => Underline::Curly,
        GhosttyUnderline::Dotted => Underline::Dotted,
        GhosttyUnderline::Dashed => Underline::Dashed,
        _ => Underline::Single,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Ghostty {
        Ghostty::new(
            Geometry {
                cols: 20,
                rows: 5,
                scrollback_lines: 100,
            },
            Box::new(std::io::sink()),
        )
        .expect("the engine starts")
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
    fn mouse_tracking_and_kitty_flags_are_reported() {
        let mut engine = engine();
        engine.advance(b"\x1b[?1002h\x1b[>1u");
        let modes = engine.modes();
        assert_eq!(modes.mouse, MouseMode::Drag);
        assert!(modes.kitty_keyboard & 1 != 0);
    }

    #[test]
    fn a_wide_grapheme_takes_two_cells() {
        let mut engine = engine();
        engine.advance("字".as_bytes());
        let view = engine.view();
        assert_eq!(view.rows[0].cells[0].text, "字");
        assert_eq!(view.rows[0].cells[1].text, "");
        assert_eq!(view.cursor.col, 2);
    }
}
