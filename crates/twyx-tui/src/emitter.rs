//! Writes the composed grid to the outer terminal.
//!
//! Every run of cells starts with an absolute cursor position, so the outer
//! terminal's width tables never decide where the next cell lands.

use std::io::Write;

use anyhow::{Context, Result};
use std::collections::HashMap;

use twyx_protocol::grid::{Attrs, Cell, Colour, Cursor, CursorShape, Underline};

/// What the outer terminal said it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub synchronized_output: bool,
    pub kitty_keyboard: bool,
    pub truecolor: bool,
}

/// One whole screen for the outer terminal, as the compositor built it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Vec<Cell>>,
    /// Link targets by the ids the cells carry.
    pub links: HashMap<u32, String>,
    /// The cursor, where the focused pane has a visible one on screen.
    pub cursor: Option<Cursor>,
}

impl Grid {
    pub fn blank(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::blank(); usize::from(cols)]; usize::from(rows)],
            links: HashMap::new(),
            cursor: None,
        }
    }

    fn link(&self, cell: &Cell) -> Option<&str> {
        cell.link
            .and_then(|id| self.links.get(&id))
            .map(String::as_str)
    }

    /// Whether a row reads the same on screen in both grids.
    fn same_row(&self, other: &Grid, row: usize) -> bool {
        match (self.cells.get(row), other.cells.get(row)) {
            (Some(a), Some(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(x, y)| x == y && self.link(x) == other.link(y))
            }
            _ => false,
        }
    }
}

pub struct Emitter<W: Write> {
    out: W,
    capabilities: Capabilities,
    current: String,
    /// The grid on screen now; `None` until the first draw and after a
    /// resize, when every row is drawn again.
    shown: Option<Grid>,
}

impl<W: Write> std::fmt::Debug for Emitter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("capabilities", &self.capabilities)
            .field("attributes", &self.current)
            .finish_non_exhaustive()
    }
}

impl<W: Write> Emitter<W> {
    pub fn new(out: W, capabilities: Capabilities) -> Self {
        Self {
            out,
            capabilities,
            current: String::new(),
            shown: None,
        }
    }

    /// The terminal changed size: the next draw starts from a clear screen.
    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    pub fn writer(&self) -> &W {
        &self.out
    }

    pub fn caps(&self) -> Capabilities {
        self.capabilities
    }

    /// Draws the rows that differ from the grid on screen.
    pub fn draw(&mut self, grid: &Grid) -> Result<()> {
        let shown = self.shown.take();
        let redraw = shown
            .as_ref()
            .is_none_or(|shown| (shown.cols, shown.rows) != (grid.cols, grid.rows));
        if redraw {
            // A stale screen from before a resize or a detach can hold anything.
            write!(self.out, "\x1b[0m\x1b[H\x1b[2J").context("clear the screen")?;
            self.current.clear();
        }
        let rows: Vec<usize> = (0..usize::from(grid.rows))
            .filter(|row| {
                redraw
                    || !shown
                        .as_ref()
                        .is_some_and(|shown| grid.same_row(shown, *row))
            })
            .collect();
        let cursor_moved = shown
            .as_ref()
            .is_none_or(|shown| shown.cursor != grid.cursor);
        if rows.is_empty() && !cursor_moved {
            self.shown = Some(grid.clone());
            return self.out.flush().context("flush the frame");
        }

        let sync = self.capabilities.synchronized_output;
        if sync {
            write!(self.out, "\x1b[?2026h").context("begin synchronized output")?;
        }
        for row in rows {
            self.draw_row(grid, row)?;
        }
        self.draw_cursor(grid)?;
        if sync {
            write!(self.out, "\x1b[?2026l").context("end synchronized output")?;
        }
        self.shown = Some(grid.clone());
        self.out.flush().context("flush the frame")
    }

    fn draw_row(&mut self, grid: &Grid, row: usize) -> Result<()> {
        let Some(cells) = grid.cells.get(row) else {
            return Ok(());
        };
        let limit = cells.len();
        let mut col = 0;
        while col < limit {
            let cell = &cells[col];
            // The second half of a wide cell is covered by the first.
            if cell.text.is_empty() {
                col += 1;
                continue;
            }

            let link = grid.link(cell);
            let mut end = col + 1;
            while end < limit
                && !cells[end].text.is_empty()
                && grid.link(&cells[end]) == link
                && same_attributes(cell, &cells[end])
            {
                end += 1;
            }

            write!(self.out, "\x1b[{};{}H", row + 1, col + 1).context("position the cursor")?;

            if let Some(target) = link {
                write!(self.out, "\x1b]8;;{target}\x1b\\").context("open a link")?;
            }

            let sgr = sgr(cell);
            if sgr != self.current {
                self.out
                    .write_all(sgr.as_bytes())
                    .context("set the attributes")?;
                self.current.clone_from(&sgr);
            }

            for (offset, cell) in cells[col..end].iter().enumerate() {
                let at = col + offset;
                // A wide cell whose second half falls past the edge would wrap.
                let cut =
                    at + 1 == limit && cells.get(at + 1).is_some_and(|next| next.text.is_empty());
                if cell.attrs.contains(Attrs::HIDDEN) || cut {
                    self.out.write_all(b" ").context("write a blank cell")?;
                } else {
                    self.out
                        .write_all(cell.text.as_bytes())
                        .context("write a cell")?;
                }
            }

            if link.is_some() {
                self.out
                    .write_all(b"\x1b]8;;\x1b\\")
                    .context("close a link")?;
            }
            col = end;
        }
        Ok(())
    }

    fn draw_cursor(&mut self, grid: &Grid) -> Result<()> {
        let Some(cursor) = grid
            .cursor
            .filter(|cursor| cursor.visible && cursor.row < grid.rows && cursor.col < grid.cols)
        else {
            return self.out.write_all(b"\x1b[?25l").context("hide the cursor");
        };
        let shape = match cursor.shape {
            CursorShape::Block => 2,
            CursorShape::Underline => 4,
            CursorShape::Beam => 6,
        };
        write!(
            self.out,
            "\x1b[{};{}H\x1b[{} q\x1b[?25h",
            cursor.row + 1,
            cursor.col + 1,
            shape
        )
        .context("place the cursor")
    }

    /// Puts the terminal back the way it was found.
    pub fn leave(&mut self) -> Result<()> {
        self.out
            .write_all(b"\x1b[0m\x1b[?25h\x1b[?25h")
            .context("restore the attributes")?;
        self.out.flush().context("flush")
    }
}

fn same_attributes(a: &Cell, b: &Cell) -> bool {
    a.fg == b.fg
        && a.bg == b.bg
        && a.attrs == b.attrs
        && a.underline == b.underline
        && a.underline_colour == b.underline_colour
        && a.attrs.contains(Attrs::HIDDEN) == b.attrs.contains(Attrs::HIDDEN)
}

fn sgr(cell: &Cell) -> String {
    let mut sgr = String::from("\x1b[0");
    if cell.attrs.contains(Attrs::BOLD) {
        sgr.push_str(";1");
    }
    if cell.attrs.contains(Attrs::DIM) {
        sgr.push_str(";2");
    }
    if cell.attrs.contains(Attrs::ITALIC) {
        sgr.push_str(";3");
    }
    match cell.underline {
        Underline::None => {}
        Underline::Single => sgr.push_str(";4"),
        Underline::Double => sgr.push_str(";4:2"),
        Underline::Curly => sgr.push_str(";4:3"),
        Underline::Dotted => sgr.push_str(";4:4"),
        Underline::Dashed => sgr.push_str(";4:5"),
    }
    if cell.attrs.contains(Attrs::BLINK) {
        sgr.push_str(";5");
    }
    if cell.attrs.contains(Attrs::REVERSE) {
        sgr.push_str(";7");
    }
    if cell.attrs.contains(Attrs::STRIKE) {
        sgr.push_str(";9");
    }
    push_colour(&mut sgr, 38, cell.fg, true);
    push_colour(&mut sgr, 48, cell.bg, false);
    if let Some(colour) = cell.underline_colour {
        push_colour(&mut sgr, 58, colour, true);
    }
    sgr.push('m');
    sgr
}

fn push_colour(sgr: &mut String, base: u8, colour: Colour, is_foreground: bool) {
    match colour {
        Colour::Default => {
            if is_foreground {
                sgr.push_str(if base == 38 { ";39" } else { ";59" });
            } else {
                sgr.push_str(";49");
            }
        }
        Colour::Palette(index) => {
            sgr.push_str(&format!(";{base};5;{index}"));
        }
        Colour::Rgb { r, g, b } => {
            sgr.push_str(&format!(";{base};2;{r};{g};{b}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&str]) -> Grid {
        let cols = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0) as u16;
        let mut grid = Grid::blank(cols, rows.len() as u16);
        for (row, text) in rows.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                grid.cells[row][col].text = ch.to_string();
            }
        }
        grid.cursor = Some(Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Beam,
            visible: true,
        });
        grid
    }

    fn emit(emitter: &mut Emitter<Vec<u8>>, grid: &Grid) -> String {
        let before = emitter.writer().len();
        emitter.draw(grid).expect("draw");
        String::from_utf8(emitter.writer()[before..].to_vec()).expect("utf8")
    }

    #[test]
    fn a_draw_positions_each_run_absolutely() {
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        let bytes = emit(&mut emitter, &grid(&["ab", "cd"]));
        assert!(bytes.contains("\x1b[H\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("\x1b[1;1H"), "{bytes:?}");
        assert!(bytes.contains("\x1b[2;1H"), "{bytes:?}");
        assert!(bytes.ends_with("\x1b[1;1H\x1b[6 q\x1b[?25h"), "{bytes:?}");
    }

    #[test]
    fn synchronized_output_wraps_the_draw_when_the_terminal_supports_it() {
        let mut emitter = Emitter::new(
            Vec::new(),
            Capabilities {
                synchronized_output: true,
                ..Capabilities::default()
            },
        );
        let bytes = emit(&mut emitter, &grid(&["a"]));
        assert!(bytes.contains("\x1b[?2026h"), "{bytes:?}");
        assert!(bytes.contains("\x1b[?2026l"), "{bytes:?}");
    }

    #[test]
    fn only_rows_that_changed_are_drawn_again() {
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        emit(&mut emitter, &grid(&["ab", "cd"]));
        let bytes = emit(&mut emitter, &grid(&["ab", "cX"]));
        // The first write is the changed row, not the whole screen.
        assert!(bytes.starts_with("\x1b[2;1H"), "{bytes:?}");
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert_eq!(emit(&mut emitter, &grid(&["ab", "cX"])), "");
    }

    #[test]
    fn attributes_and_colours_are_written_into_the_run() {
        let mut grid = grid(&["a"]);
        grid.cells[0][0].fg = Colour::Rgb { r: 1, g: 2, b: 3 };
        grid.cells[0][0].attrs = Attrs::BOLD | Attrs::ITALIC;
        grid.cells[0][0].underline = Underline::Curly;
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        let bytes = emit(&mut emitter, &grid);
        assert!(bytes.contains("\x1b[0;1;3;4:3;38;2;1;2;3;49m"), "{bytes:?}");
    }

    #[test]
    fn a_link_wraps_the_cells_that_share_it() {
        let mut grid = grid(&["ab"]);
        grid.cells[0][0].link = Some(1);
        grid.cells[0][1].link = Some(1);
        grid.links.insert(1, "https://example.com".to_owned());
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        let bytes = emit(&mut emitter, &grid);
        let open = bytes
            .find("\x1b]8;;https://example.com\x1b\\")
            .expect("opened");
        let close = bytes
            .find("ab\x1b]8;;\x1b\\")
            .expect("closed after the text");
        assert!(open < close, "{bytes:?}");
    }

    #[test]
    fn a_new_size_clears_the_screen_and_draws_every_row() {
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        emit(&mut emitter, &grid(&["ab", "cd"]));
        let bytes = emit(&mut emitter, &grid(&["abc"]));
        assert!(bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("abc"), "{bytes:?}");
    }
}
