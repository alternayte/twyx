//! `alacritty_terminal` behind the engine boundary.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell as AlacrittyCell, Flags};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AlacrittyColor, CursorShape as AlacrittyCursorShape, NamedColor, Processor,
};

use twyx_protocol::grid::{
    Attrs, Cell, Colour, Cursor, CursorShape, Modes, MouseEncoding, MouseMode, Row, Underline,
};

use super::{Engine, EngineSignal, Geometry, Links, View};

pub struct Alacritty {
    term: Term<Listener>,
    processor: Processor,
    signals: Arc<Mutex<Vec<EngineSignal>>>,
    size: Size,
    title: String,
    links: Links,
}

impl std::fmt::Debug for Alacritty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alacritty")
            .field("size", &self.size)
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

impl Alacritty {
    pub fn new(geometry: Geometry) -> Self {
        let config = Config {
            scrolling_history: geometry.scrollback_lines as usize,
            kitty_keyboard: true,
            // OSC 52 reads are refused; writes reach the clipboard.
            osc52: Osc52::OnlyCopy,
            ..Config::default()
        };

        let signals = Arc::new(Mutex::new(Vec::new()));
        let listener = Listener {
            signals: Arc::clone(&signals),
        };
        let size = Size::new(geometry.cols, geometry.rows);
        let term = Term::new(config, &size, listener);

        Self {
            term,
            processor: Processor::new(),
            signals,
            size,
            title: String::new(),
            links: Links::default(),
        }
    }

    fn take_signals(&self) -> Vec<EngineSignal> {
        let Ok(mut signals) = self.signals.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *signals)
    }
}

impl Engine for Alacritty {
    fn advance(&mut self, bytes: &[u8]) -> Vec<EngineSignal> {
        self.processor.advance(&mut self.term, bytes);
        let signals = self.take_signals();
        for signal in &signals {
            if let EngineSignal::Title(title) = signal {
                self.title.clone_from(title);
            }
        }
        signals
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.size = Size::new(cols, rows);
        self.term.resize(self.size);
    }

    fn view(&mut self) -> View {
        let Self {
            term,
            size,
            title,
            links,
            ..
        } = self;
        let grid = term.grid();
        let offset = grid.display_offset();
        let mut scratch = AlacrittyCell::default();
        let rows = (0..size.screen_lines)
            .map(|row| {
                line_row(
                    grid,
                    Line(row as i32 - offset as i32),
                    size.columns,
                    links,
                    &mut scratch,
                )
            })
            .collect();

        View {
            cols: size.columns as u16,
            rows,
            cursor: cursor(term, *size),
            modes: modes(*term.mode()),
            title: title.clone(),
            scrollback: grid.history_size() as u32,
            links: links.take_used(),
        }
    }

    fn modes(&self) -> Modes {
        modes(*self.term.mode())
    }

    fn line(&mut self, line: i32) -> Row {
        let mut scratch = AlacrittyCell::default();
        line_row(
            self.term.grid(),
            Line(line),
            self.size.columns,
            &mut self.links,
            &mut scratch,
        )
    }
}

#[derive(Debug)]
struct Listener {
    signals: Arc<Mutex<Vec<EngineSignal>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: AlacrittyEvent) {
        let signal = match event {
            AlacrittyEvent::Title(title) => EngineSignal::Title(title),
            AlacrittyEvent::ResetTitle => EngineSignal::Title(String::new()),
            AlacrittyEvent::Bell => EngineSignal::Bell,
            AlacrittyEvent::ClipboardStore(_, text) => EngineSignal::ClipboardWrite(text),
            AlacrittyEvent::PtyWrite(text) => EngineSignal::WriteToPane(text.into_bytes()),
            _ => return,
        };
        if let Ok(mut signals) = self.signals.lock() {
            signals.push(signal);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Size {
    columns: usize,
    screen_lines: usize,
}

impl Size {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            columns: usize::from(cols).max(2),
            screen_lines: usize::from(rows).max(1),
        }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

fn line_row(
    grid: &alacritty_terminal::grid::Grid<AlacrittyCell>,
    line: Line,
    cols: usize,
    links: &mut Links,
    scratch: &mut AlacrittyCell,
) -> Row {
    let cells = (0..cols)
        .map(|col| {
            *scratch = grid[Point::new(line, Column(col))].clone();
            cell(scratch, links)
        })
        .collect();
    Row { cells }
}

fn cell(cell: &AlacrittyCell, links: &mut Links) -> Cell {
    let text = if cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
    {
        String::new()
    } else {
        let mut text = String::new();
        text.push(cell.c);
        if let Some(zerowidth) = cell.zerowidth() {
            text.extend(zerowidth);
        }
        text
    };

    Cell {
        text,
        fg: colour(cell.fg),
        bg: colour(cell.bg),
        attrs: attrs(cell.flags),
        underline: underline(cell.flags),
        underline_colour: cell.underline_color().map(colour),
        link: cell.hyperlink().map(|hyperlink| {
            let id = links.intern(hyperlink.uri());
            links.mark(id);
            id
        }),
    }
}

fn colour(colour: AlacrittyColor) -> Colour {
    match colour {
        AlacrittyColor::Spec(rgb) => Colour::Rgb {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        },
        AlacrittyColor::Indexed(index) => Colour::Palette(index),
        AlacrittyColor::Named(named) => match named as u16 {
            index if index < 16 => Colour::Palette(index as u8),
            _ => Colour::Default,
        },
    }
}

fn attrs(flags: Flags) -> Attrs {
    let mut attrs = Attrs::empty();
    attrs.set(Attrs::BOLD, flags.contains(Flags::BOLD));
    attrs.set(Attrs::DIM, flags.contains(Flags::DIM));
    attrs.set(Attrs::ITALIC, flags.contains(Flags::ITALIC));
    attrs.set(Attrs::REVERSE, flags.contains(Flags::INVERSE));
    attrs.set(Attrs::HIDDEN, flags.contains(Flags::HIDDEN));
    attrs.set(Attrs::STRIKE, flags.contains(Flags::STRIKEOUT));
    attrs
}

fn underline(flags: Flags) -> Underline {
    if flags.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if flags.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    }
}

fn cursor(term: &Term<Listener>, size: Size) -> Cursor {
    let content = term.renderable_content();
    let point = content.cursor.point;
    // A scrolled-back viewport keeps the cursor on the row it was moved to.
    let offset = content.display_offset as i32;
    let row = (point.line.0 + offset).clamp(0, size.screen_lines as i32 - 1) as u16;
    Cursor {
        col: point.column.0 as u16,
        row,
        shape: match content.cursor.shape {
            AlacrittyCursorShape::Block | AlacrittyCursorShape::HollowBlock => CursorShape::Block,
            AlacrittyCursorShape::Beam => CursorShape::Beam,
            AlacrittyCursorShape::Underline => CursorShape::Underline,
            AlacrittyCursorShape::Hidden => CursorShape::Block,
        },
        visible: content.mode.contains(TermMode::SHOW_CURSOR)
            && content.cursor.shape != AlacrittyCursorShape::Hidden,
    }
}

fn modes(mode: TermMode) -> Modes {
    let mouse = if mode.contains(TermMode::MOUSE_DRAG) {
        MouseMode::Drag
    } else if mode.contains(TermMode::MOUSE_MOTION) {
        MouseMode::Motion
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        MouseMode::Click
    } else {
        MouseMode::Off
    };

    let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
        MouseEncoding::Sgr
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        MouseEncoding::Utf8
    } else {
        MouseEncoding::X10
    };

    let mut kitty_keyboard = 0_u8;
    kitty_keyboard |= u8::from(mode.contains(TermMode::DISAMBIGUATE_ESC_CODES));
    kitty_keyboard |= u8::from(mode.contains(TermMode::REPORT_EVENT_TYPES)) << 1;
    kitty_keyboard |= u8::from(mode.contains(TermMode::REPORT_ALTERNATE_KEYS)) << 2;
    kitty_keyboard |= u8::from(mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)) << 3;
    kitty_keyboard |= u8::from(mode.contains(TermMode::REPORT_ASSOCIATED_TEXT)) << 4;

    Modes {
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        app_cursor: mode.contains(TermMode::APP_CURSOR),
        app_keypad: mode.contains(TermMode::APP_KEYPAD),
        focus_events: mode.contains(TermMode::FOCUS_IN_OUT),
        mouse,
        mouse_encoding,
        kitty_keyboard,
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
        insert: mode.contains(TermMode::INSERT),
        reverse_video: false,
    }
}

/// Named colours that are roles rather than palette entries.
#[allow(dead_code, reason = "the mapping is exhaustive over the engine's enum")]
fn is_role(named: NamedColor) -> bool {
    named as u16 >= 256
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Alacritty {
        Alacritty::new(Geometry {
            cols: 20,
            rows: 5,
            scrollback_lines: 100,
        })
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
        assert_eq!(view.cursor.row, 0);
    }

    #[test]
    fn sgr_attributes_reach_the_cells() {
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
    fn a_wide_grapheme_takes_two_cells() {
        let mut engine = engine();
        engine.advance("字".as_bytes());
        let view = engine.view();
        assert_eq!(view.rows[0].cells[0].text, "字");
        assert_eq!(view.rows[0].cells[1].text, "");
        assert_eq!(view.cursor.col, 2);
    }

    #[test]
    fn bracketed_paste_and_mouse_modes_are_reported() {
        let mut engine = engine();
        engine.advance(b"\x1b[?2004h\x1b[?1002h\x1b[?1006h");
        let modes = engine.view().modes;
        assert!(modes.bracketed_paste);
        assert_eq!(modes.mouse, MouseMode::Drag);
        assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
    }

    #[test]
    fn the_title_reaches_the_view() {
        let mut engine = engine();
        engine.advance(b"\x1b]2;nvim\x07");
        assert_eq!(engine.view().title, "nvim");
    }

    #[test]
    fn output_beyond_the_screen_fills_the_scrollback() {
        let mut engine = engine();
        engine.advance(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n");
        let view = engine.view();
        // Eight lines and the empty line after the last break, in five rows.
        assert_eq!(view.scrollback, 4);
        assert_eq!(text(&view, 0).trim_end(), "5");
        assert_eq!(text(&view, 3).trim_end(), "8");
        assert_eq!(text(&view, 4).trim_end(), "");
    }
}
