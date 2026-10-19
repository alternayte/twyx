//! The pane's terminal, on `libghostty-vt`.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{Style, StyleColor, Underline as GhosttyUnderline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate};
use libghostty_vt::{RenderState, Terminal};

use twyx_protocol::grid::{
    Attrs, Cell, Colour, Cursor, CursorShape, Modes, MouseEncoding, MouseMode, Row, Underline,
};

use super::{EngineSignal, Geometry, Links, View};

pub struct Ghostty {
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    size: (u16, u16),
    links: Links,
    signals: Rc<RefCell<Vec<EngineSignal>>>,
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
        let mut term = Terminal::new(geometry.cols.max(2), geometry.rows.max(1)).ok()?;
        term.set_scrollback_max_lines(Some(geometry.scrollback_lines as usize))
            .ok()?;
        term.on_pty_write(move |_, reply: &[u8]| {
            let _ = answerback.write_all(reply);
        })
        .ok()?;
        let signals: Rc<RefCell<Vec<EngineSignal>>> = Rc::default();
        let sink = Rc::clone(&signals);
        term.on_bell(move |_| sink.borrow_mut().push(EngineSignal::Bell))
            .ok()?;
        let sink = Rc::clone(&signals);
        term.on_title_changed(move |term| {
            let title = term.title().unwrap_or_default().to_owned();
            sink.borrow_mut().push(EngineSignal::Title(title));
        })
        .ok()?;
        let sink = Rc::clone(&signals);
        term.on_clipboard_write(move |_, write| {
            let text = write
                .contents()
                .find(|content| content.mime.starts_with("text/plain"))
                .map(|content| String::from_utf8_lossy(content.data).into_owned());
            // The daemon applies the clipboard policy; the engine only reports.
            if let Some(text) = text {
                sink.borrow_mut().push(EngineSignal::ClipboardWrite(text));
                write.reply(Ok(()), false);
            }
        })
        .ok()?;
        let render = RenderState::new().ok()?;
        Some(Self {
            term,
            render,
            size: (geometry.cols.max(2), geometry.rows.max(1)),
            links: Links::default(),
            signals,
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

impl Ghostty {
    /// Parses output bytes and returns the signals they carried.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<EngineSignal> {
        self.term.vt_write(bytes);
        std::mem::take(&mut *self.signals.borrow_mut())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = (cols.max(2), rows.max(1));
        let _ = self.term.resize(self.size.0, self.size.1, 0, 0);
    }

    pub fn view(&mut self) -> View {
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
        let mut linked = Vec::new();
        while let Some(row) = row_iteration.next() {
            let mut line = Vec::with_capacity(cols);
            let Ok(mut cell_iteration) = cells.update(row) else {
                break;
            };
            while let Some(cell) = cell_iteration.next() {
                if cell
                    .raw_cell()
                    .and_then(|raw| raw.has_hyperlink())
                    .unwrap_or(false)
                {
                    linked.push((grid.len(), line.len()));
                }
                let style = cell.style().unwrap_or_default();
                let spacer = cell
                    .raw_cell()
                    .and_then(|raw| raw.wide())
                    .is_ok_and(|wide| wide == CellWide::SpacerTail);
                let mut text = cell
                    .graphemes()
                    .map(|graphemes| graphemes.into_iter().collect::<String>())
                    .unwrap_or_default();
                // Only the spacer after a wide cell stays empty; a cell that was
                // never written is a blank.
                if text.is_empty() && !spacer {
                    text.push(' ');
                }
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

        for (row, col) in linked {
            let Some(uri) = hyperlink_at(&self.term, col as u16, row as u32) else {
                continue;
            };
            let id = self.links.intern(&uri);
            self.links.mark(id);
            if let Some(cell) = grid
                .get_mut(row)
                .and_then(|line: &mut Row| line.cells.get_mut(col))
            {
                cell.link = Some(id);
            }
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
            links: self.links.take_used(),
        }
    }

    pub fn modes(&self) -> Modes {
        let on = |mode| self.term.mode(mode).unwrap_or(false);
        let mouse = if on(Mode::ANY_MOUSE) {
            MouseMode::Motion
        } else if on(Mode::BUTTON_MOUSE) {
            MouseMode::Drag
        } else if on(Mode::NORMAL_MOUSE) || on(Mode::X10_MOUSE) {
            MouseMode::Click
        } else {
            MouseMode::Off
        };
        let mouse_encoding = if on(Mode::SGR_PIXELS_MOUSE) {
            MouseEncoding::SgrPixels
        } else if on(Mode::SGR_MOUSE) {
            MouseEncoding::Sgr
        } else if on(Mode::URXVT_MOUSE) {
            MouseEncoding::Urxvt
        } else if on(Mode::UTF8_MOUSE) {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::X10
        };
        Modes {
            bracketed_paste: on(Mode::BRACKETED_PASTE),
            app_cursor: on(Mode::DECCKM),
            app_keypad: on(Mode::KEYPAD_KEYS),
            focus_events: on(Mode::FOCUS_EVENT),
            mouse,
            mouse_encoding,
            kitty_keyboard: self
                .term
                .kitty_keyboard_flags()
                .map(|flags| flags.bits())
                .unwrap_or(0),
            alt_screen: on(Mode::ALT_SCREEN)
                || on(Mode::ALT_SCREEN_SAVE)
                || on(Mode::ALT_SCREEN_LEGACY),
            insert: on(Mode::INSERT),
            reverse_video: on(Mode::REVERSE_COLORS),
        }
    }

    /// One row of the grid, by display line, for copy mode.
    pub fn line(&mut self, line: i32) -> Row {
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

fn hyperlink_at(term: &Terminal<'_, '_>, x: u16, y: u32) -> Option<String> {
    let cell = term
        .grid_ref(Point::Viewport(PointCoordinate { x, y }))
        .ok()?;
    let mut buf = vec![0_u8; 256];
    let len = match cell.hyperlink_uri(&mut buf) {
        Ok(len) => len,
        Err(libghostty_vt::Error::OutOfSpace { required }) if required > buf.len() => {
            buf.resize(required, 0);
            cell.hyperlink_uri(&mut buf).ok()?
        }
        Err(_) => return None,
    };
    buf.truncate(len);
    String::from_utf8(buf).ok()
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
