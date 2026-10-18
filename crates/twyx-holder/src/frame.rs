//! Turns engine views into the frames a consumer receives.
//!
//! A frame carries the rows that changed since the last one. A consumer that
//! fell behind asks for a snapshot instead.

use std::collections::HashMap;

use twyx_protocol::grid::{Frame, Row, RowUpdate, Screen, Snapshot};

use crate::engine::View;

#[derive(Debug)]
pub struct FrameBuilder {
    seq: u64,
    cols: u16,
    rows: u16,
    last: Vec<Row>,
    published_links: HashMap<u32, String>,
}

impl FrameBuilder {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            seq: 0,
            cols,
            rows,
            last: Vec::new(),
            published_links: HashMap::new(),
        }
    }

    /// A new consumer needs every row and every link again.
    pub fn reset_consumer(&mut self) {
        self.last.clear();
        self.published_links.clear();
    }

    pub fn snapshot(&mut self, view: &View) -> Snapshot {
        self.last.clone_from(&view.rows);
        self.cols = view.cols;
        self.rows = view.rows.len() as u16;
        let links: Vec<(u32, String)> = view
            .links
            .iter()
            .map(|(id, uri)| (*id, uri.clone()))
            .collect();
        self.published_links = links.iter().cloned().collect();

        Snapshot {
            seq: self.seq,
            screen: screen(view),
            rows: view.rows.clone(),
            links,
            scrollback: view.scrollback,
        }
    }

    pub fn frame(&mut self, view: &View) -> Frame {
        self.seq += 1;
        let resized = view.cols != self.cols || view.rows.len() != self.last.len();
        if resized {
            self.cols = view.cols;
            self.rows = view.rows.len() as u16;
        }

        let mut changed = Vec::new();
        for (index, row) in view.rows.iter().enumerate() {
            let differs = match self.last.get(index) {
                Some(previous) => previous != row,
                None => true,
            };
            if differs {
                changed.push(RowUpdate {
                    row: index as u16,
                    cells: row.cells.clone(),
                });
            }
        }
        self.last.clone_from(&view.rows);

        let mut links = Vec::new();
        for (id, uri) in &view.links {
            match self.published_links.get(id) {
                Some(known) if known == uri => {}
                _ => {
                    self.published_links.insert(*id, uri.clone());
                    links.push((*id, uri.clone()));
                }
            }
        }

        Frame {
            seq: self.seq,
            screen: screen(view),
            changed,
            links,
            scrollback: view.scrollback,
        }
    }
}

fn screen(view: &View) -> Screen {
    Screen {
        cols: view.cols,
        rows: view.rows.len() as u16,
        cursor: view.cursor,
        modes: view.modes,
        title: view.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use twyx_protocol::grid::{Cell, Cursor, CursorShape, Modes};

    fn view(rows: &[&str]) -> View {
        View {
            cols: 4,
            rows: rows
                .iter()
                .map(|text| Row {
                    cells: text
                        .chars()
                        .map(|c| Cell {
                            text: c.to_string(),
                            ..Cell::blank()
                        })
                        .collect(),
                })
                .collect(),
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

    #[test]
    fn a_frame_carries_only_the_rows_that_changed() {
        let mut builder = FrameBuilder::new(4, 2);
        let first = builder.frame(&view(&["abcd", "efgh"]));
        assert_eq!(first.changed.len(), 2);
        assert_eq!(first.seq, 1);

        let second = builder.frame(&view(&["abcd", "efXh"]));
        assert_eq!(second.changed.len(), 1);
        assert_eq!(second.changed[0].row, 1);
        assert_eq!(second.seq, 2);
    }

    #[test]
    fn a_snapshot_carries_every_row_and_the_next_frame_only_what_changed_after_it() {
        let mut builder = FrameBuilder::new(4, 2);
        builder.frame(&view(&["abcd", "efgh"]));
        let snapshot = builder.snapshot(&view(&["abcd", "efgh"]));
        assert_eq!(snapshot.rows.len(), 2);

        let unchanged = builder.frame(&view(&["abcd", "efgh"]));
        assert!(unchanged.changed.is_empty());

        let changed = builder.frame(&view(&["abcd", "efXh"]));
        assert_eq!(changed.changed.len(), 1);
    }

    #[test]
    fn a_new_consumer_receives_every_row_of_the_next_frame() {
        let mut builder = FrameBuilder::new(4, 2);
        builder.frame(&view(&["abcd", "efgh"]));
        builder.reset_consumer();
        let frame = builder.frame(&view(&["abcd", "efgh"]));
        assert_eq!(frame.changed.len(), 2);
    }
}
