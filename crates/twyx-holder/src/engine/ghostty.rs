//! The pane's terminal, on `libghostty-vt`.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::screen::TrackedGridRef;
use libghostty_vt::search::Search;
use libghostty_vt::selection::{FormatOptions, Selection};
use libghostty_vt::style::{Style, StyleColor, Underline as GhosttyUnderline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport};
use libghostty_vt::{RenderState, Terminal};

use twyx_core::keys::CopyMotion;
use twyx_protocol::grid::{
    Attrs, Cell, Colour, Cursor, CursorShape, Modes, MouseEncoding, MouseMode, Row, Underline,
};
use twyx_protocol::holder::CopyAction;

use super::{EngineSignal, Geometry, Links, View};

pub struct Ghostty {
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    size: (u16, u16),
    links: Links,
    signals: Rc<RefCell<Vec<EngineSignal>>>,
    copy: Option<Copy>,
}

/// Copy mode: a cursor over the screen and scrollback, the selection's
/// other end, and the last search. The engine tracks every point, so they
/// stay on their text while output arrives and scrolls.
struct Copy {
    cursor: TrackedGridRef,
    anchor: Option<TrackedGridRef>,
    search: Option<Search<'static, 'static>>,
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
            copy: None,
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
            let selected = row.selection().ok().flatten();
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
                let mut attrs = attrs(&style);
                let x = line.len() as u16;
                if selected.is_some_and(|range| (range.start_x..=range.end_x).contains(&x)) {
                    attrs ^= Attrs::REVERSE;
                }
                line.push(Cell {
                    text,
                    fg: colour(style.fg_color),
                    bg: colour(style.bg_color),
                    attrs,
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
        let mut cursor = Cursor {
            col: cursor.as_ref().map_or(0, |cursor| cursor.x),
            row: cursor.as_ref().map_or(0, |cursor| cursor.y),
            shape: CursorShape::Block,
            visible: snapshot.cursor_visible().unwrap_or(true),
        };
        // In copy mode the cursor is the copy cursor, wherever it is.
        if let Some(copy) = &self.copy {
            let point = copy.cursor.point(PointSpace::Viewport).ok().flatten();
            cursor = Cursor {
                col: point.map_or(0, |point| point.x),
                row: point.map_or(0, |point| point.y as u16),
                shape: CursorShape::Block,
                visible: point.is_some(),
            };
        }
        View {
            cols: self.size.0,
            rows: grid,
            cursor,
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

    pub fn in_copy_mode(&self) -> bool {
        self.copy.is_some()
    }

    /// Carries out one copy mode action. Returns the copied text on a yank.
    pub fn copy(&mut self, action: CopyAction) -> Option<String> {
        if matches!(action, CopyAction::Exit) {
            self.leave_copy();
            return None;
        }
        if self.copy.is_none() {
            let x = self.term.cursor_x().unwrap_or(0);
            let y = self.term.cursor_y().unwrap_or(0);
            let cursor = self
                .term
                .track_grid_ref(Point::Active(PointCoordinate { x, y: u32::from(y) }))
                .ok()?;
            self.copy = Some(Copy {
                cursor,
                anchor: None,
                search: None,
            });
        }
        match action {
            CopyAction::Enter | CopyAction::Exit => {}
            CopyAction::Move(motion) => {
                let (x, y) = self.copy_point()?;
                let (x, y) = self.motion((x, y), motion);
                self.place_copy_cursor(x, y);
            }
            CopyAction::Scroll(lines) => {
                let (x, y) = self.copy_point()?;
                let bar = self.term.scrollbar().ok()?;
                self.term
                    .scroll_viewport(ScrollViewport::Delta(lines as isize));
                // The cursor keeps its place on screen as the text moves.
                let moved = i64::from(y) + i64::from(lines);
                let last = bar.total.saturating_sub(1) as i64;
                self.place_copy_cursor(x, moved.clamp(0, last) as u32);
            }
            CopyAction::Select => {
                let copy = self.copy.as_mut()?;
                if copy.anchor.take().is_none() {
                    let (x, y) = self.copy_point()?;
                    let anchor = self
                        .term
                        .track_grid_ref(Point::Screen(PointCoordinate { x, y }))
                        .ok()?;
                    self.copy.as_mut()?.anchor = Some(anchor);
                }
                self.show_selection();
            }
            CopyAction::SelectLine => {
                let (_, y) = self.copy_point()?;
                let anchor = self
                    .term
                    .track_grid_ref(Point::Screen(PointCoordinate { x: 0, y }))
                    .ok()?;
                self.copy.as_mut()?.anchor = Some(anchor);
                let end = self.size.0.saturating_sub(1);
                self.place_copy_cursor(end, y);
            }
            CopyAction::Yank => {
                let text = self.selected_text();
                self.leave_copy();
                return text;
            }
            CopyAction::Search(needle) => {
                if needle.is_empty() {
                    return None;
                }
                let mut search = match self.copy.as_mut()?.search.take() {
                    Some(search) => search,
                    None => Search::new(&mut self.term).ok()?,
                };
                let _ = search.set_needle(&mut self.term, needle.as_bytes());
                let _ = search.select_next(&mut self.term);
                self.copy.as_mut()?.search = Some(search);
                self.follow_match();
            }
            CopyAction::Next | CopyAction::Previous => {
                let mut search = self.copy.as_mut()?.search.take()?;
                let _ = if action == CopyAction::Next {
                    search.select_next(&mut self.term)
                } else {
                    search.select_prev(&mut self.term)
                };
                self.copy.as_mut()?.search = Some(search);
                self.follow_match();
            }
        }
        None
    }

    fn leave_copy(&mut self) {
        let _ = self.term.set_selection(None);
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.copy = None;
    }

    /// The copy cursor in screen rows: zero is the oldest scrollback line.
    fn copy_point(&self) -> Option<(u16, u32)> {
        let point = self
            .copy
            .as_ref()?
            .cursor
            .point(PointSpace::Screen)
            .ok()??;
        Some((point.x, point.y))
    }

    fn place_copy_cursor(&mut self, x: u16, y: u32) {
        let Some(copy) = self.copy.as_mut() else {
            return;
        };
        let x = x.min(self.size.0.saturating_sub(1));
        let _ = copy
            .cursor
            .set(&mut self.term, Point::Screen(PointCoordinate { x, y }));
        // The viewport follows the cursor.
        if let Ok(bar) = self.term.scrollbar() {
            let y = u64::from(y);
            if y < bar.offset {
                self.term.scroll_viewport(ScrollViewport::Row(y as usize));
            } else if y >= bar.offset + bar.len {
                let top = y + 1 - bar.len;
                self.term.scroll_viewport(ScrollViewport::Row(top as usize));
            }
        }
        self.show_selection();
    }

    /// Makes the engine's selection run from the anchor to the cursor, so the
    /// render state reports it row by row.
    fn show_selection(&mut self) {
        let Some(copy) = &self.copy else {
            return;
        };
        let Some(anchor) = &copy.anchor else {
            let _ = self.term.set_selection(None);
            return;
        };
        let (Ok(Some(start)), Ok(Some(end))) = (
            anchor.snapshot(&self.term),
            copy.cursor.snapshot(&self.term),
        ) else {
            return;
        };
        let selection = Selection::new(start, end, false);
        let _ = self.term.set_selection(Some(&selection));
    }

    fn selected_text(&self) -> Option<String> {
        let options = || {
            FormatOptions::new()
                .with_emit_format(Format::Plain)
                .with_unwrap(true)
                .with_trim(true)
        };
        let mut buffer = vec![0_u8; 64 * 1024];
        let written = match self.term.format_selection_buf(options(), &mut buffer) {
            Ok(written) => written?,
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                buffer.resize(required, 0);
                self.term
                    .format_selection_buf(options(), &mut buffer)
                    .ok()??
            }
            Err(_) => return None,
        };
        buffer.truncate(written);
        Some(String::from_utf8_lossy(&buffer).into_owned())
    }

    /// Puts the copy cursor on the selected search match, and selects it.
    fn follow_match(&mut self) {
        let Some(mut search) = self.copy.as_mut().and_then(|copy| copy.search.take()) else {
            return;
        };
        let mut found = None;
        if let Ok(snapshot) = search.snapshot(&mut self.term)
            && let Ok(Some(selected)) = snapshot.selected_match()
        {
            let start = snapshot
                .terminal()
                .point_from_grid_ref(&selected.start(), PointSpace::Screen);
            let end = snapshot
                .terminal()
                .point_from_grid_ref(&selected.end(), PointSpace::Screen);
            if let (Ok(Some(start)), Ok(Some(end))) = (start, end) {
                found = Some((start, end));
            }
        }
        if let Some(copy) = self.copy.as_mut() {
            copy.search = Some(search);
        }
        let Some((start, end)) = found else {
            return;
        };
        let anchor = self.term.track_grid_ref(Point::Screen(start)).ok();
        if let Some(copy) = self.copy.as_mut() {
            copy.anchor = anchor;
        }
        self.place_copy_cursor(end.x, end.y);
    }

    /// Where a motion takes the copy cursor.
    fn motion(&self, (x, y): (u16, u32), motion: CopyMotion) -> (u16, u32) {
        let bar = self.term.scrollbar().ok();
        let total = bar.map_or(u64::from(self.size.1), |bar| bar.total).max(1);
        let last = (total - 1) as u32;
        let page = bar.map_or(u64::from(self.size.1), |bar| bar.len).max(1) as u32;
        let right = self.size.0.saturating_sub(1);
        match motion {
            CopyMotion::Left => (x.saturating_sub(1), y),
            CopyMotion::Right => (x.saturating_add(1).min(right), y),
            CopyMotion::Up => (x, y.saturating_sub(1)),
            CopyMotion::Down => (x, (y + 1).min(last)),
            CopyMotion::PageUp => (x, y.saturating_sub(page)),
            CopyMotion::PageDown => (x, (y + page).min(last)),
            CopyMotion::HalfPageUp => (x, y.saturating_sub(page / 2)),
            CopyMotion::HalfPageDown => (x, (y + page / 2).min(last)),
            CopyMotion::Top => (0, 0),
            CopyMotion::Bottom => (0, last),
            CopyMotion::LineStart => (0, y),
            CopyMotion::LineEnd => {
                let end = (0..=right)
                    .rev()
                    .find(|col| !self.blank(*col, y))
                    .unwrap_or(0);
                (end, y)
            }
            CopyMotion::WordForward => {
                let mut at = (x, y);
                // Past this word, then past the blanks after it.
                while !self.blank(at.0, at.1) {
                    match self.step(at, right, last) {
                        Some(next) => at = next,
                        None => return at,
                    }
                }
                while self.blank(at.0, at.1) {
                    match self.step(at, right, last) {
                        Some(next) => at = next,
                        None => return at,
                    }
                }
                at
            }
            CopyMotion::WordBack => {
                let back = |at: (u16, u32)| match at {
                    (0, 0) => None,
                    (0, y) => Some((right, y - 1)),
                    (x, y) => Some((x - 1, y)),
                };
                let mut at = (x, y);
                // Back over blanks, then to the start of the word before.
                loop {
                    match back(at) {
                        Some(previous) if self.blank(previous.0, previous.1) => at = previous,
                        _ => break,
                    }
                }
                loop {
                    match back(at) {
                        Some(previous) if !self.blank(previous.0, previous.1) => at = previous,
                        _ => break,
                    }
                }
                at
            }
        }
    }

    fn step(&self, (x, y): (u16, u32), right: u16, last: u32) -> Option<(u16, u32)> {
        if x < right {
            Some((x + 1, y))
        } else if y < last {
            Some((0, y + 1))
        } else {
            None
        }
    }

    fn blank(&self, x: u16, y: u32) -> bool {
        let mut text = [' '; 8];
        let Ok(grid) = self.term.grid_ref(Point::Screen(PointCoordinate { x, y })) else {
            return true;
        };
        let written = grid.graphemes(&mut text).unwrap_or(0);
        text[..written.min(text.len())]
            .iter()
            .all(|ch| ch.is_whitespace())
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

    /// Twenty numbered lines on a five-row screen, so most are scrollback.
    fn scrolled() -> Ghostty {
        let mut engine = engine();
        for n in 1..=20 {
            engine.advance(format!("line {n:02} word\r\n").as_bytes());
        }
        engine
    }

    #[test]
    fn copy_mode_moves_into_scrollback_and_copies_a_selection() {
        let mut engine = scrolled();
        engine.copy(CopyAction::Enter);
        // The cursor starts on the empty last line; up three lands on line 18.
        for _ in 0..3 {
            engine.copy(CopyAction::Move(CopyMotion::Up));
        }
        engine.copy(CopyAction::Select);
        engine.copy(CopyAction::Move(CopyMotion::LineEnd));
        let view = engine.view();
        assert!(
            view.rows
                .iter()
                .any(|row| row.cells[0].attrs.contains(Attrs::REVERSE))
        );
        assert_eq!(
            engine.copy(CopyAction::Yank).as_deref(),
            Some("line 18 word")
        );
        assert!(!engine.in_copy_mode());
    }

    #[test]
    fn copy_mode_search_puts_the_cursor_on_the_match_in_scrollback() {
        let mut engine = scrolled();
        engine.copy(CopyAction::Search("line 03".to_owned()));
        let view = engine.view();
        assert!(
            view.rows
                .iter()
                .any(|row| text_of(row).starts_with("line 03")),
            "the viewport scrolled to the match"
        );
        assert_eq!(engine.copy(CopyAction::Yank).as_deref(), Some("line 03"));
    }

    #[test]
    fn words_and_line_ends_move_the_copy_cursor() {
        let mut engine = scrolled();
        engine.copy(CopyAction::Enter);
        engine.copy(CopyAction::Move(CopyMotion::Up));
        engine.copy(CopyAction::Select);
        engine.copy(CopyAction::Move(CopyMotion::WordForward));
        engine.copy(CopyAction::Move(CopyMotion::WordForward));
        engine.copy(CopyAction::Move(CopyMotion::Right));
        // "line 20 word": two words on is the "w" of "word", one right is the
        // "o", and the selection holds the cell under the cursor.
        assert_eq!(engine.copy(CopyAction::Yank).as_deref(), Some("line 20 wo"));
    }

    fn text_of(row: &Row) -> String {
        row.cells.iter().map(|cell| cell.text.as_str()).collect()
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
