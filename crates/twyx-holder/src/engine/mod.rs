//! The terminal engine: libghostty-vt, and the types the holder reads from it.

use std::collections::HashMap;

use twyx_protocol::grid::{Cursor, Modes, Row};

pub mod ghostty;

pub use ghostty::Ghostty as Engine;

/// What an engine reports from the pane's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSignal {
    Title(String),
    Bell,
    /// A request to put text in the user's clipboard.
    ClipboardWrite(String),
}

/// The visible screen, as the engine currently holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub cols: u16,
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub modes: Modes,
    pub title: String,
    /// Lines kept above the top of the viewport.
    pub scrollback: u32,
    /// The link targets the rows above refer to.
    pub links: Vec<(u32, String)>,
}

#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub scrollback_lines: u32,
}

/// Starts an engine; `answerback` carries the replies the terminal sends back
/// to the pane.
pub fn create(
    geometry: Geometry,
    answerback: Box<dyn std::io::Write + Send>,
) -> anyhow::Result<Engine> {
    match Engine::new(geometry, answerback) {
        Some(engine) => Ok(engine),
        None => anyhow::bail!("libghostty-vt refused the terminal options"),
    }
}

/// An engine that starts with a pane's history above a divider, for a pane
/// that runs again. A history that cannot be read gives a plain engine.
pub fn create_after(
    geometry: Geometry,
    answerback: Box<dyn std::io::Write + Send>,
    history: &std::path::Path,
) -> anyhow::Result<Engine> {
    let Ok(Some((checkpoint, log))) = crate::history::read(history) else {
        return create(geometry, answerback);
    };
    let Some(mut engine) = Engine::restore(geometry, answerback, &checkpoint, &log) else {
        return create(geometry, Box::new(std::io::sink()));
    };
    engine.advance(DIVIDER);
    Ok(engine)
}

/// The line between a pane's old history and its new run.
const DIVIDER: &[u8] = b"\r\n\x1b[2m-- ran again --\x1b[0m\r\n";

/// Interns the link targets the grid refers to, so a cell carries a number.
#[derive(Debug, Default)]
pub(crate) struct Links {
    ids: HashMap<String, u32>,
    targets: Vec<String>,
    used: Vec<bool>,
}

impl Links {
    pub(crate) fn intern(&mut self, uri: &str) -> u32 {
        if let Some(id) = self.ids.get(uri) {
            return *id;
        }
        let id = self.targets.len() as u32;
        self.targets.push(uri.to_owned());
        self.ids.insert(uri.to_owned(), id);
        id
    }

    pub(crate) fn mark(&mut self, id: u32) {
        let index = id as usize;
        if index >= self.used.len() {
            self.used.resize(index + 1, false);
        }
        self.used[index] = true;
    }

    pub(crate) fn take_used(&mut self) -> Vec<(u32, String)> {
        let used = std::mem::take(&mut self.used);
        used.iter()
            .enumerate()
            .filter(|(_, used)| **used)
            .filter_map(|(id, _)| {
                self.targets
                    .get(id)
                    .map(|target| (id as u32, target.clone()))
            })
            .collect()
    }
}
