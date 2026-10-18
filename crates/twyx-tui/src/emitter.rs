//! Writes the frame grid to the outer terminal.
//!
//! Every run of cells starts with an absolute cursor position, so the outer
//! terminal's width tables never decide where the next cell lands.

use std::io::Write;

use anyhow::{Context, Result};
use twyx_protocol::grid::{Attrs, Cell, Colour, CursorShape, Underline};

use twyx_protocol::view::View;

/// What the outer terminal said it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub synchronized_output: bool,
    pub kitty_keyboard: bool,
    pub truecolor: bool,
}

pub struct Emitter<W: Write> {
    out: W,
    capabilities: Capabilities,
    current: String,
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
        }
    }

    pub fn writer(&self) -> &W {
        &self.out
    }

    pub fn caps(&self) -> Capabilities {
        self.capabilities
    }

    /// Draws the rows that changed since the last call.
    pub fn draw(&mut self, view: &mut View) -> Result<()> {
        let (fresh, dirty) = view.take_dirty();
        if fresh {
            // A stale grid from before a resize or a detach can hold anything.
            write!(self.out, "\x1b[0m\x1b[H\x1b[2J").context("clear the screen")?;
            self.current.clear();
        }
        if dirty.is_empty() {
            return Ok(());
        }

        let sync = self.capabilities.synchronized_output;
        if sync {
            write!(self.out, "\x1b[?2026h").context("begin synchronized output")?;
        }

        for row in dirty {
            self.draw_row(view, row)?;
        }
        self.draw_cursor(view)?;

        if sync {
            write!(self.out, "\x1b[?2026l").context("end synchronized output")?;
        }
        self.out.flush().context("flush the frame")
    }

    fn draw_row(&mut self, view: &View, row: usize) -> Result<()> {
        let Some(cells) = view.cells.get(row) else {
            return Ok(());
        };
        let mut col = 0;
        while col < cells.len() {
            let cell = &cells[col];
            // The second half of a wide cell is covered by the first.
            if cell.text.is_empty() {
                col += 1;
                continue;
            }

            let link = view.link(cell);
            let mut end = col + 1;
            while end < cells.len()
                && !cells[end].text.is_empty()
                && view.link(&cells[end]) == link
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

            for cell in &cells[col..end] {
                if cell.attrs.contains(Attrs::HIDDEN) {
                    self.out.write_all(b" ").context("write a hidden cell")?;
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

    fn draw_cursor(&mut self, view: &View) -> Result<()> {
        let cursor = view.cursor;
        if !cursor.visible || cursor.row >= view.rows || cursor.col >= view.cols {
            return self.out.write_all(b"\x1b[?25l").context("hide the cursor");
        }
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
    use twyx_protocol::grid::{Cursor, Modes, Row, RowUpdate, Screen, Snapshot};

    fn view(rows: &[&str]) -> View {
        let cols = rows.iter().map(|row| row.len()).max().unwrap_or(0) as u16;
        let cells: Vec<Row> = rows
            .iter()
            .map(|row| {
                let mut cells: Vec<Cell> = row
                    .chars()
                    .map(|c| Cell {
                        text: c.to_string(),
                        ..Cell::blank()
                    })
                    .collect();
                cells.resize(usize::from(cols), Cell::blank());
                Row { cells }
            })
            .collect();
        let mut view = View::new(cols, rows.len() as u16);
        view.apply_snapshot(Snapshot {
            seq: 1,
            screen: Screen {
                cols,
                rows: rows.len() as u16,
                cursor: Cursor {
                    col: 0,
                    row: 0,
                    shape: CursorShape::Beam,
                    visible: true,
                },
                modes: Modes::default(),
                title: String::new(),
            },
            rows: cells,
            links: Vec::new(),
            scrollback: 0,
        });
        view
    }

    fn emit(view: &mut View, caps: Capabilities) -> String {
        let mut emitter = Emitter::new(Vec::new(), caps);
        emitter.draw(view).expect("draw");
        String::from_utf8(emitter.writer().clone()).expect("utf8")
    }

    #[test]
    fn a_draw_positions_each_run_absolutely() {
        let mut view = view(&["ab", "cd"]);
        let bytes = emit(
            &mut view,
            Capabilities {
                synchronized_output: false,
                ..Capabilities::default()
            },
        );
        assert!(bytes.contains("\x1b[H\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("\x1b[1;1H"), "{bytes:?}");
        assert!(bytes.contains("\x1b[2;1H"), "{bytes:?}");
        assert!(bytes.ends_with("\x1b[1;1H\x1b[6 q\x1b[?25h"), "{bytes:?}");
    }

    #[test]
    fn synchronized_output_wraps_the_draw_when_the_terminal_supports_it() {
        let mut view = view(&["a"]);
        let bytes = emit(
            &mut view,
            Capabilities {
                synchronized_output: true,
                ..Capabilities::default()
            },
        );
        assert!(bytes.contains("\x1b[?2026h"), "{bytes:?}");
        assert!(bytes.contains("\x1b[?2026l"), "{bytes:?}");
    }

    #[test]
    fn only_rows_that_changed_are_drawn_again() {
        let mut view = view(&["ab", "cd"]);
        emit(&mut view, Capabilities::default());
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        view.apply_frame(twyx_protocol::grid::Frame {
            seq: 2,
            screen: Screen {
                cols: 2,
                rows: 2,
                cursor: Cursor {
                    col: 0,
                    row: 0,
                    shape: CursorShape::Beam,
                    visible: true,
                },
                modes: Modes::default(),
                title: String::new(),
            },
            changed: vec![RowUpdate {
                row: 1,
                cells: vec![
                    Cell {
                        text: "c".to_owned(),
                        ..Cell::blank()
                    },
                    Cell {
                        text: "X".to_owned(),
                        ..Cell::blank()
                    },
                ],
            }],
            links: Vec::new(),
            scrollback: 0,
        });
        emitter.draw(&mut view).expect("draw");
        let bytes = String::from_utf8(emitter.writer().clone()).expect("utf8");
        // The first write is the changed row, not the whole screen.
        assert!(bytes.starts_with("\x1b[2;1H"), "{bytes:?}");
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
    }

    #[test]
    fn attributes_and_colours_are_written_into_the_run() {
        let mut view = view(&["a"]);
        view.cells[0][0].fg = Colour::Rgb { r: 1, g: 2, b: 3 };
        view.cells[0][0].attrs = Attrs::BOLD | Attrs::ITALIC;
        view.cells[0][0].underline = Underline::Curly;
        let bytes = emit(&mut view, Capabilities::default());
        assert!(bytes.contains("\x1b[0;1;3;4:3;38;2;1;2;3;49m"), "{bytes:?}");
    }

    #[test]
    fn a_link_wraps_the_cells_that_share_it() {
        let mut view = view(&["ab"]);
        view.apply_frame(twyx_protocol::grid::Frame {
            seq: 2,
            screen: Screen {
                cols: 2,
                rows: 1,
                cursor: Cursor {
                    col: 0,
                    row: 0,
                    shape: CursorShape::Beam,
                    visible: true,
                },
                modes: Modes::default(),
                title: String::new(),
            },
            changed: vec![RowUpdate {
                row: 0,
                cells: vec![
                    Cell {
                        text: "a".to_owned(),
                        link: Some(1),
                        ..Cell::blank()
                    },
                    Cell {
                        text: "b".to_owned(),
                        link: Some(1),
                        ..Cell::blank()
                    },
                ],
            }],
            links: vec![(1, "https://example.com".to_owned())],
            scrollback: 0,
        });
        let mut emitter = Emitter::new(Vec::new(), Capabilities::default());
        emitter.draw(&mut view).expect("draw");
        let bytes = String::from_utf8(emitter.writer().clone()).expect("utf8");
        assert!(
            bytes.contains("\x1b]8;;https://example.com\x1b\\"),
            "{bytes:?}"
        );
        assert!(bytes.contains("\x1b]8;;\x1b\\"), "{bytes:?}");
    }
}
