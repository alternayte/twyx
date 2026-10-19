//! Input events. Clients send these, never bytes.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use twyx_core::keys::{Chord, Key};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct Modifiers: u16 {
        const SHIFT = 1 << 0;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
        const HYPER = 1 << 4;
        const META = 1 << 5;
        const CAPS_LOCK = 1 << 6;
        const NUM_LOCK = 1 << 7;
    }
}

/// The key itself, apart from the text it produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCode {
    /// A character key in the current layout, unshifted where the layout has one.
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    /// A numbered function key, F1 to F35.
    F(u8),
    /// A key with no name in this model.
    Unidentified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KeyKind {
    #[default]
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Modifiers,
    pub kind: KeyKind,
    /// The text this key produced in the current layout, if any.
    pub text: Option<String>,
    /// The key this one is on the same physical key as, shifted.
    pub shifted: Option<char>,
    /// The key this one is on the same physical key as, unshifted.
    pub base: Option<char>,
}

impl KeyEvent {
    /// The key as the keymap sees it. A shifted character becomes its own
    /// key: on a US layout, shift with `\` is `|`. Releases have none.
    pub fn chord(&self) -> Option<Chord> {
        if self.kind == KeyKind::Release {
            return None;
        }
        let mut shift = self.mods.contains(Modifiers::SHIFT);
        let key = match self.code {
            KeyCode::Char(ch) => {
                let ch = match (shift, self.shifted) {
                    (true, Some(shifted)) => shifted,
                    (true, None) if ch.is_ascii_lowercase() => ch.to_ascii_uppercase(),
                    _ => ch,
                };
                if ch != ' ' {
                    shift = false;
                }
                Key::Char(ch)
            }
            KeyCode::Enter => Key::Enter,
            KeyCode::Tab => Key::Tab,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Escape => Key::Escape,
            KeyCode::Delete => Key::Delete,
            KeyCode::Insert => Key::Insert,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::F(number) => Key::F(number),
            KeyCode::Unidentified => return None,
        };
        Some(Chord {
            key,
            ctrl: self.mods.contains(Modifiers::CTRL),
            alt: self.mods.contains(Modifiers::ALT),
            shift,
        })
    }

    pub fn press(code: KeyCode, mods: Modifiers) -> Self {
        Self {
            code,
            mods,
            kind: KeyKind::Press,
            text: None,
            shifted: None,
            base: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    Back,
    Forward,
    /// No button, for a bare motion event.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseEvent {
    /// Zero-based cell column inside the pane.
    pub col: u16,
    /// Zero-based cell row inside the pane.
    pub row: u16,
    pub button: MouseButton,
    pub action: MouseAction,
    pub mods: Modifiers,
    pub pixel_col: Option<u16>,
    pub pixel_row: Option<u16>,
}

/// One input event for a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Text pasted as one unit.
    Paste(String),
    Focus(bool),
}
