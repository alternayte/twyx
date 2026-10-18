//! The cell grid a client draws, and the frames a holder publishes.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// A colour as the pane's screen shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Colour {
    /// The terminal's default for the role the cell uses.
    Default,
    /// One of the 256 palette colours.
    Palette(u8),
    Rgb {
        r: u8,
        g: u8,
        b: u8,
    },
}

bitflags! {
    /// Cell attributes that are a yes or a no.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Attrs: u16 {
        const BOLD = 1 << 0;
        const DIM = 1 << 1;
        const ITALIC = 1 << 2;
        const BLINK = 1 << 3;
        const REVERSE = 1 << 4;
        const HIDDEN = 1 << 5;
        const STRIKE = 1 << 6;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// One cell of the grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// One grapheme cluster. Empty for the spacer after a wide cell.
    pub text: String,
    pub fg: Colour,
    pub bg: Colour,
    pub attrs: Attrs,
    pub underline: Underline,
    pub underline_colour: Option<Colour>,
    /// Index into the sent link table.
    pub link: Option<u32>,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            text: " ".to_owned(),
            fg: Colour::Default,
            bg: Colour::Default,
            attrs: Attrs::empty(),
            underline: Underline::None,
            underline_colour: None,
            link: None,
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MouseMode {
    #[default]
    Off,
    Click,
    Drag,
    Motion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MouseEncoding {
    #[default]
    X10,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

/// The terminal modes that change how input is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modes {
    pub bracketed_paste: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub focus_events: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// Kitty keyboard protocol flags, bits 0 to 4.
    pub kitty_keyboard: u8,
    pub alt_screen: bool,
    pub insert: bool,
    pub reverse_video: bool,
}

/// The cells of one row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
}

/// A row that changed since the previous frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowUpdate {
    pub row: u16,
    pub cells: Vec<Cell>,
}

/// The parts of a pane's screen that a client draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    pub modes: Modes,
    pub title: String,
}

/// What a holder publishes after output settles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub seq: u64,
    pub screen: Screen,
    /// The rows whose cells differ from the previous frame.
    pub changed: Vec<RowUpdate>,
    /// Link targets first named by this frame.
    pub links: Vec<(u32, String)>,
    /// Lines kept above the top of the viewport.
    pub scrollback: u32,
}

/// A complete screen, for a consumer that fell behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub seq: u64,
    pub screen: Screen,
    pub rows: Vec<Row>,
    pub links: Vec<(u32, String)>,
    pub scrollback: u32,
}
