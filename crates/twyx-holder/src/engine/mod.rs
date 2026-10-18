//! The engine boundary. One terminal engine sits behind it until GATE 1 picks
//! one, then the others and this trait are deleted.

use std::collections::HashMap;

use twyx_protocol::grid::{Cursor, Modes, Row};

#[cfg(feature = "engine-alacritty")]
pub mod alacritty;
#[cfg(feature = "engine-ghostty")]
pub mod ghostty;
#[cfg(feature = "engine-wezterm")]
pub mod wezterm;

/// What an engine reports from the pane's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSignal {
    Title(String),
    Bell,
    /// A request to put text in the user's clipboard.
    ClipboardWrite(String),
    /// Bytes the terminal itself must send back to the pane.
    WriteToPane(Vec<u8>),
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

/// The engine this build was compiled with.
pub fn create(
    geometry: Geometry,
    answerback: Box<dyn std::io::Write + Send>,
) -> anyhow::Result<Box<dyn Engine>> {
    #[cfg(feature = "engine-ghostty")]
    {
        match ghostty::Ghostty::new(geometry, answerback) {
            Some(engine) => Ok(Box::new(engine)),
            None => anyhow::bail!("libghostty-vt refused the terminal options"),
        }
    }
    #[cfg(all(not(feature = "engine-ghostty"), feature = "engine-wezterm"))]
    {
        Ok(Box::new(wezterm::Wezterm::new(geometry, answerback)))
    }
    #[cfg(all(
        not(feature = "engine-ghostty"),
        not(feature = "engine-wezterm"),
        feature = "engine-alacritty"
    ))]
    {
        let _ = answerback;
        Ok(Box::new(alacritty::Alacritty::new(geometry)))
    }
    #[cfg(all(
        not(feature = "engine-ghostty"),
        not(feature = "engine-wezterm"),
        not(feature = "engine-alacritty")
    ))]
    {
        let _ = (geometry, answerback);
        anyhow::bail!("no terminal engine is compiled in")
    }
}

/// One terminal engine, owned by the holder's core thread.
///
/// Not `Send`: `libghostty-vt` handles are single-threaded, and the boundary
/// never moves an engine between threads.
pub trait Engine {
    /// Parses output bytes and returns the signals they carried.
    fn advance(&mut self, bytes: &[u8]) -> Vec<EngineSignal>;

    fn resize(&mut self, cols: u16, rows: u16);

    fn view(&mut self) -> View;

    fn modes(&self) -> Modes;

    /// One cell of the grid, by display line and column, for copy mode.
    fn line(&mut self, line: i32) -> Row;
}

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
