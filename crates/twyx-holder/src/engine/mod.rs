//! The engine boundary. One terminal engine sits behind it until GATE 1 picks
//! one, then the others and this trait are deleted.

use twyx_protocol::grid::{Cursor, Modes, Row};

#[cfg(feature = "engine-alacritty")]
pub mod alacritty;
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
    #[cfg(feature = "engine-wezterm")]
    {
        Ok(Box::new(wezterm::Wezterm::new(geometry, answerback)))
    }
    #[cfg(all(not(feature = "engine-wezterm"), feature = "engine-alacritty"))]
    {
        let _ = answerback;
        Ok(Box::new(alacritty::Alacritty::new(geometry)))
    }
    #[cfg(all(not(feature = "engine-wezterm"), not(feature = "engine-alacritty")))]
    {
        let _ = (geometry, answerback);
        anyhow::bail!("no terminal engine is compiled in")
    }
}

pub trait Engine: Send {
    /// Parses output bytes and returns the signals they carried.
    fn advance(&mut self, bytes: &[u8]) -> Vec<EngineSignal>;

    fn resize(&mut self, cols: u16, rows: u16);

    fn view(&mut self) -> View;

    fn modes(&self) -> Modes;

    /// One cell of the grid, by display line and column, for copy mode.
    fn line(&mut self, line: i32) -> Row;
}
