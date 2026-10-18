//! The frame grid a client draws, and how it follows a pane's frames.

use std::collections::HashMap;

use twyx_protocol::grid::{Cell, Cursor, CursorShape, Frame, Modes, Snapshot};

/// What a client knows about the pane's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Vec<Cell>>,
    pub cursor: Cursor,
    pub modes: Modes,
    pub title: String,
    pub scrollback: u32,
    links: HashMap<u32, String>,
    /// Rows whose cells changed since the last draw.
    dirty: Vec<bool>,
    /// The screen was replaced wholesale.
    fresh: bool,
}

impl View {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: blank_rows(cols, rows),
            cursor: Cursor {
                col: 0,
                row: 0,
                shape: CursorShape::Block,
                visible: false,
            },
            modes: Modes::default(),
            title: String::new(),
            scrollback: 0,
            links: HashMap::new(),
            dirty: vec![true; rows as usize],
            fresh: true,
        }
    }

    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.cols = snapshot.screen.cols;
        self.rows = snapshot.screen.rows;
        self.cells = snapshot.rows.into_iter().map(|row| row.cells).collect();
        self.normalise();
        self.cursor = snapshot.screen.cursor;
        self.modes = snapshot.screen.modes;
        self.title = snapshot.screen.title;
        self.scrollback = snapshot.scrollback;
        for (id, uri) in snapshot.links {
            self.links.insert(id, uri);
        }
        self.dirty = vec![true; self.rows as usize];
        self.fresh = true;
    }

    pub fn apply_frame(&mut self, frame: Frame) {
        if frame.screen.cols != self.cols || frame.screen.rows != self.rows {
            self.cols = frame.screen.cols;
            self.rows = frame.screen.rows;
            self.cells = blank_rows(self.cols, self.rows);
            self.dirty = vec![true; self.rows as usize];
        }
        for (id, uri) in frame.links {
            self.links.insert(id, uri);
        }
        for update in frame.changed {
            let row = update.row as usize;
            if row < self.cells.len() {
                self.cells[row] = update.cells;
                self.dirty[row] = true;
            }
        }
        self.normalise();
        self.cursor = frame.screen.cursor;
        self.modes = frame.screen.modes;
        self.title = frame.screen.title;
        self.scrollback = frame.scrollback;
    }

    /// Link target of a cell, if it has one.
    pub fn link(&self, cell: &Cell) -> Option<&str> {
        cell.link
            .and_then(|id| self.links.get(&id))
            .map(String::as_str)
    }

    /// Takes the rows that changed since the last draw.
    pub fn take_dirty(&mut self) -> (bool, Vec<usize>) {
        let fresh = std::mem::replace(&mut self.fresh, false);
        let dirty = self
            .dirty
            .iter()
            .enumerate()
            .filter(|(_, dirty)| **dirty)
            .map(|(row, _)| row)
            .collect();
        if fresh {
            self.dirty.iter_mut().for_each(|dirty| *dirty = false);
        } else {
            self.dirty.iter_mut().for_each(|dirty| *dirty = false);
        }
        (fresh, dirty)
    }

    pub fn resize_grid(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = blank_rows(cols, rows);
        self.dirty = vec![true; rows as usize];
        self.fresh = true;
    }

    /// A pane can report a row shorter than the grid while it resizes.
    fn normalise(&mut self) {
        for row in &mut self.cells {
            row.resize(self.cols as usize, Cell::blank());
        }
    }
}

fn blank_rows(cols: u16, rows: u16) -> Vec<Vec<Cell>> {
    vec![vec![Cell::blank(); cols as usize]; rows as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use twyx_protocol::grid::{Row, RowUpdate, Screen};

    fn screen(cols: u16, rows: u16) -> Screen {
        Screen {
            cols,
            rows,
            cursor: Cursor {
                col: 1,
                row: 1,
                shape: CursorShape::Block,
                visible: true,
            },
            modes: Modes::default(),
            title: "nvim".to_owned(),
        }
    }

    fn row(text: &str) -> Row {
        Row {
            cells: text
                .chars()
                .map(|c| Cell {
                    text: c.to_string(),
                    ..Cell::blank()
                })
                .collect(),
        }
    }

    #[test]
    fn a_snapshot_replaces_the_whole_screen() {
        let mut view = View::new(4, 2);
        view.apply_snapshot(Snapshot {
            seq: 1,
            screen: screen(4, 2),
            rows: vec![row("abcd"), row("efgh")],
            links: vec![(1, "https://example.com".to_owned())],
            scrollback: 3,
        });
        assert_eq!(view.cells[1][0].text, "e");
        assert_eq!(view.title, "nvim");
        assert_eq!(view.scrollback, 3);
        let (fresh, dirty) = view.take_dirty();
        assert!(fresh);
        assert_eq!(dirty, vec![0, 1]);
    }

    #[test]
    fn a_frame_changes_only_the_rows_it_carries() {
        let mut view = View::new(4, 2);
        view.apply_snapshot(Snapshot {
            seq: 1,
            screen: screen(4, 2),
            rows: vec![row("abcd"), row("efgh")],
            links: Vec::new(),
            scrollback: 0,
        });
        view.take_dirty();
        view.apply_frame(Frame {
            seq: 2,
            screen: screen(4, 2),
            changed: vec![RowUpdate {
                row: 1,
                cells: row("eXgh").cells,
            }],
            links: Vec::new(),
            scrollback: 0,
        });
        let (fresh, dirty) = view.take_dirty();
        assert!(!fresh);
        assert_eq!(dirty, vec![1]);
        assert_eq!(view.cells[1][1].text, "X");
    }

    #[test]
    fn a_wider_frame_pads_the_rows_before_the_grid_grows() {
        let mut view = View::new(4, 2);
        view.apply_snapshot(Snapshot {
            seq: 1,
            screen: screen(4, 2),
            rows: vec![row("abc"), row("def")],
            links: Vec::new(),
            scrollback: 0,
        });
        assert_eq!(view.cells[0].len(), 4);
        assert_eq!(view.cells[0][3].text, " ");
    }
}
