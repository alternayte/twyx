//! Turns input events into the bytes a pane receives.
//!
//! One encoder serves both clients. It follows the pane's current modes:
//! legacy keys, the kitty keyboard protocol, every mouse encoding, bracketed
//! paste and focus events.

use twyx_protocol::grid::{Modes, MouseEncoding, MouseMode};
use twyx_protocol::input::{
    InputEvent, KeyCode, KeyEvent, KeyKind, Modifiers, MouseAction, MouseButton, MouseEvent,
};

const KITTY_DISAMBIGUATE: u8 = 1 << 0;
const KITTY_EVENT_TYPES: u8 = 1 << 1;
const KITTY_ALTERNATE_KEYS: u8 = 1 << 2;
const KITTY_ALL_KEYS: u8 = 1 << 3;
const KITTY_TEXT: u8 = 1 << 4;

pub fn encode(event: &InputEvent, modes: &Modes, out: &mut Vec<u8>) {
    match event {
        InputEvent::Key(key) => encode_key(key, modes, out),
        InputEvent::Mouse(mouse) => encode_mouse(mouse, modes, out),
        InputEvent::Paste(text) => encode_paste(text, modes, out),
        InputEvent::Focus(focused) => encode_focus(*focused, modes, out),
    }
}

fn encode_paste(text: &str, modes: &Modes, out: &mut Vec<u8>) {
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
}

fn encode_focus(focused: bool, modes: &Modes, out: &mut Vec<u8>) {
    if modes.focus_events {
        out.extend_from_slice(if focused { b"\x1b[I" } else { b"\x1b[O" });
    }
}

fn encode_key(key: &KeyEvent, modes: &Modes, out: &mut Vec<u8>) {
    let kitty = modes.kitty_keyboard;
    let event_types = kitty & KITTY_EVENT_TYPES != 0;

    if key.kind == KeyKind::Release && !event_types {
        // A legacy pane never sees key releases.
        return;
    }

    if let Some(sequence) = function_key(key, modes) {
        out.extend_from_slice(&sequence);
        return;
    }

    if kitty & KITTY_ALL_KEYS != 0
        || (kitty & KITTY_DISAMBIGUATE != 0 && disambiguation_needed(key))
    {
        encode_kitty(key, modes, out);
        return;
    }

    encode_legacy(key, out);
}

/// Escape, enter, tab and backspace cannot be told apart from control keys in
/// legacy encoding, so the kitty protocol reports them by name.
fn disambiguation_needed(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Escape | KeyCode::Enter | KeyCode::Backspace => true,
        KeyCode::Tab => true,
        KeyCode::Char(_) => key.mods.contains(Modifiers::CTRL),
        _ => false,
    }
}

fn encode_legacy(key: &KeyEvent, out: &mut Vec<u8>) {
    let alt = key.mods.contains(Modifiers::ALT);
    if alt {
        out.push(0x1b);
    }

    if let Some(byte) = control_byte(key) {
        out.push(byte);
        return;
    }

    if key.code == KeyCode::Tab && key.mods.contains(Modifiers::SHIFT) {
        out.extend_from_slice(b"\x1b[Z");
        return;
    }

    if let Some(text) = key_text(key) {
        out.extend_from_slice(text.as_bytes());
    }
}

/// The text a legacy terminal would send for this key.
fn key_text(key: &KeyEvent) -> Option<String> {
    if key
        .mods
        .intersects(Modifiers::CTRL | Modifiers::SUPER | Modifiers::HYPER | Modifiers::META)
    {
        return None;
    }
    match key.code {
        KeyCode::Char(c) => Some(
            key.text
                .clone()
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| c.to_string()),
        ),
        _ => key.text.clone().filter(|text| !text.is_empty()),
    }
}

fn control_byte(key: &KeyEvent) -> Option<u8> {
    let ctrl = key.mods.contains(Modifiers::CTRL);
    match key.code {
        KeyCode::Enter => Some(0x0d),
        KeyCode::Tab if !key.mods.contains(Modifiers::SHIFT) => Some(0x09),
        KeyCode::Backspace => Some(if ctrl { 0x08 } else { 0x7f }),
        KeyCode::Escape => Some(0x1b),
        KeyCode::Char(' ') if ctrl => Some(0x00),
        KeyCode::Char(c) if ctrl => control_for(c),
        _ => None,
    }
}

fn control_for(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '8' => Some(0x7f),
        _ => None,
    }
}

fn encode_kitty(key: &KeyEvent, modes: &Modes, out: &mut Vec<u8>) {
    let kitty = modes.kitty_keyboard;
    let code = match key.code {
        KeyCode::Char(c) => u32::from(c),
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::Backspace => 127,
        KeyCode::Escape => 27,
        _ => return encode_legacy(key, out),
    };

    let mut sequence = format!("\x1b[{code}");
    if kitty & KITTY_ALTERNATE_KEYS != 0 {
        let shifted = key.shifted.filter(|c| u32::from(*c) != code);
        let base = key.base.filter(|c| u32::from(*c) != code);
        if shifted.is_some() || base.is_some() {
            sequence.push(':');
            if let Some(shifted) = shifted {
                sequence.push_str(&u32::from(shifted).to_string());
            }
            if let Some(base) = base {
                sequence.push(':');
                sequence.push_str(&u32::from(base).to_string());
            }
        }
    }

    let modifiers = modifier_parameter(key.mods);
    let event = (kitty & KITTY_EVENT_TYPES != 0 && key.kind != KeyKind::Press)
        .then(|| event_type(key.kind));
    let text = (kitty & KITTY_TEXT != 0)
        .then(|| key.text.as_deref().filter(|text| !text.is_empty()))
        .flatten()
        .map(codepoints);

    // Kitty omits the modifier field when it carries nothing.
    if modifiers != "1" || event.is_some() || text.is_some() {
        sequence.push(';');
        sequence.push_str(&modifiers);
        if let Some(event) = event {
            sequence.push(':');
            sequence.push_str(event);
        }
    }

    if let Some(text) = text {
        sequence.push(';');
        sequence.push_str(&text);
    }

    sequence.push('u');
    out.extend_from_slice(sequence.as_bytes());
}

fn codepoints(text: &str) -> String {
    let mut out = String::new();
    for (index, c) in text.chars().enumerate() {
        if index > 0 {
            out.push(':');
        }
        out.push_str(&u32::from(c).to_string());
    }
    out
}

fn event_type(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Press => "1",
        KeyKind::Repeat => "2",
        KeyKind::Release => "3",
    }
}

/// Cursor, editing and function keys. Their bytes are the same in every mode
/// apart from the modifier parameter.
fn function_key(key: &KeyEvent, modes: &Modes) -> Option<Vec<u8>> {
    let (letter, number) = match key.code {
        KeyCode::Up => ("A", None),
        KeyCode::Down => ("B", None),
        KeyCode::Right => ("C", None),
        KeyCode::Left => ("D", None),
        KeyCode::Home => ("H", None),
        KeyCode::End => ("F", None),
        KeyCode::Insert => ("", Some(2)),
        KeyCode::Delete => ("", Some(3)),
        KeyCode::PageUp => ("", Some(5)),
        KeyCode::PageDown => ("", Some(6)),
        KeyCode::F(1) => ("P", None),
        KeyCode::F(2) => ("Q", None),
        KeyCode::F(3) => ("R", None),
        KeyCode::F(4) => ("S", None),
        KeyCode::F(5) => ("", Some(15)),
        KeyCode::F(6) => ("", Some(17)),
        KeyCode::F(7) => ("", Some(18)),
        KeyCode::F(8) => ("", Some(19)),
        KeyCode::F(9) => ("", Some(20)),
        KeyCode::F(10) => ("", Some(21)),
        KeyCode::F(11) => ("", Some(23)),
        KeyCode::F(12) => ("", Some(24)),
        KeyCode::F(13) => ("", Some(25)),
        KeyCode::F(14) => ("", Some(26)),
        KeyCode::F(15) => ("", Some(28)),
        KeyCode::F(16) => ("", Some(29)),
        KeyCode::F(17) => ("", Some(31)),
        KeyCode::F(18) => ("", Some(32)),
        KeyCode::F(19) => ("", Some(33)),
        KeyCode::F(20) => ("", Some(34)),
        _ => return None,
    };

    let modifiers = modifier_parameter(key.mods);
    let types = modes.kitty_keyboard & KITTY_EVENT_TYPES != 0;
    let plain = modifiers == "1" && !(types && key.kind != KeyKind::Press);

    if let Some(number) = number {
        let mut sequence = format!("\x1b[{number}");
        if !plain {
            sequence.push(';');
            sequence.push_str(&modifiers);
            if types && key.kind != KeyKind::Press {
                sequence.push(':');
                sequence.push_str(event_type(key.kind));
            }
        }
        sequence.push('~');
        return Some(sequence.into_bytes());
    }

    if plain {
        // SS3 carries the four function keys that have no numeric form, and
        // every cursor key while the pane asked for application cursor keys.
        if matches!(letter, "P" | "Q" | "R" | "S") || (modes.app_cursor && is_cursor_key(key.code))
        {
            return Some(format!("\x1bO{letter}").into_bytes());
        }
        return Some(format!("\x1b[{letter}").into_bytes());
    }

    let mut sequence = String::from("\x1b[1;");
    sequence.push_str(&modifiers);
    if types && key.kind != KeyKind::Press {
        sequence.push(':');
        sequence.push_str(event_type(key.kind));
    }
    sequence.push_str(letter);
    Some(sequence.into_bytes())
}

fn is_cursor_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
    )
}

fn modifier_parameter(mods: Modifiers) -> String {
    let mut bits = 1_u32;
    bits += u32::from(mods.contains(Modifiers::SHIFT));
    bits += u32::from(mods.contains(Modifiers::ALT)) << 1;
    bits += u32::from(mods.contains(Modifiers::CTRL)) << 2;
    bits += u32::from(mods.contains(Modifiers::SUPER)) << 3;
    bits += u32::from(mods.contains(Modifiers::HYPER)) << 4;
    bits += u32::from(mods.contains(Modifiers::META)) << 5;
    bits += u32::from(mods.contains(Modifiers::CAPS_LOCK)) << 6;
    bits += u32::from(mods.contains(Modifiers::NUM_LOCK)) << 7;
    bits.to_string()
}

fn encode_mouse(mouse: &MouseEvent, modes: &Modes, out: &mut Vec<u8>) {
    if modes.mouse == MouseMode::Off {
        return;
    }
    if mouse.action == MouseAction::Motion
        && mouse.button == MouseButton::None
        && modes.mouse == MouseMode::Click
    {
        return;
    }

    let released = mouse.action == MouseAction::Release;
    let mut button = match mouse.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::WheelLeft => 66,
        MouseButton::WheelRight => 67,
        MouseButton::Back => 128,
        MouseButton::Forward => 129,
        MouseButton::None => 35,
    };
    if released
        && !matches!(
            modes.mouse_encoding,
            MouseEncoding::Sgr | MouseEncoding::SgrPixels
        )
    {
        button = 3;
    }

    button += u32::from(mouse.mods.contains(Modifiers::SHIFT)) << 2;
    button += u32::from(mouse.mods.contains(Modifiers::ALT)) << 3;
    button += u32::from(mouse.mods.contains(Modifiers::CTRL)) << 4;
    button += u32::from(mouse.action == MouseAction::Motion) << 5;

    let col = u32::from(mouse.col) + 1;
    let row = u32::from(mouse.row) + 1;

    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let end = if released { 'm' } else { 'M' };
            out.extend_from_slice(format!("\x1b[<{button};{col};{row}{end}").as_bytes());
        }
        MouseEncoding::SgrPixels => {
            let x = u32::from(mouse.pixel_col.unwrap_or(mouse.col)) + 1;
            let y = u32::from(mouse.pixel_row.unwrap_or(mouse.row)) + 1;
            let end = if released { 'm' } else { 'M' };
            out.extend_from_slice(format!("\x1b[<{button};{x};{y}{end}").as_bytes());
        }
        MouseEncoding::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            push_wide(out, button + 32);
            push_wide(out, col + 32);
            push_wide(out, row + 32);
        }
        MouseEncoding::Urxvt => {
            out.extend_from_slice(format!("\x1b[{button};{col};{row}M").as_bytes());
        }
        MouseEncoding::X10 => {
            out.extend_from_slice(b"\x1b[M");
            push_byte(out, button);
            push_byte(out, col);
            push_byte(out, row);
        }
    }
}

fn push_byte(out: &mut Vec<u8>, value: u32) {
    out.push(u8::try_from((value + 32).min(255)).unwrap_or(255));
}

fn push_wide(out: &mut Vec<u8>, value: u32) {
    let character = char::from_u32(value).unwrap_or('\u{fffd}');
    let mut buffer = [0_u8; 4];
    out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> Modes {
        Modes::default()
    }

    fn key(code: KeyCode, mods: Modifiers) -> InputEvent {
        InputEvent::Key(KeyEvent::press(code, mods))
    }

    fn encode_one(event: &InputEvent, modes: &Modes) -> Vec<u8> {
        let mut out = Vec::new();
        encode(event, modes, &mut out);
        out
    }

    fn text(code: KeyCode, mods: Modifiers, text: &str) -> InputEvent {
        let mut event = KeyEvent::press(code, mods);
        event.text = Some(text.to_owned());
        InputEvent::Key(event)
    }

    #[test]
    fn a_plain_character_is_its_utf8_bytes() {
        assert_eq!(
            encode_one(&text(KeyCode::Char('a'), Modifiers::empty(), "a"), &modes()),
            b"a"
        );
        assert_eq!(
            encode_one(&text(KeyCode::Char('é'), Modifiers::empty(), "é"), &modes()),
            "é".as_bytes()
        );
    }

    #[test]
    fn alt_prefixes_escape_and_ctrl_sends_a_control_byte() {
        assert_eq!(
            encode_one(&text(KeyCode::Char('b'), Modifiers::ALT, "b"), &modes()),
            b"\x1bb"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Char('c'), Modifiers::CTRL), &modes()),
            b"\x03"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Char(' '), Modifiers::CTRL), &modes()),
            b"\x00"
        );
    }

    #[test]
    fn cursor_keys_follow_the_application_cursor_mode() {
        let mut modes = modes();
        assert_eq!(
            encode_one(&key(KeyCode::Up, Modifiers::empty()), &modes),
            b"\x1b[A"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Up, Modifiers::SHIFT), &modes),
            b"\x1b[1;2A"
        );
        modes.app_cursor = true;
        assert_eq!(
            encode_one(&key(KeyCode::Up, Modifiers::empty()), &modes),
            b"\x1bOA"
        );
    }

    #[test]
    fn function_and_editing_keys_use_their_sequences() {
        assert_eq!(
            encode_one(&key(KeyCode::F(5), Modifiers::empty()), &modes()),
            b"\x1b[15~"
        );
        assert_eq!(
            encode_one(&key(KeyCode::F(1), Modifiers::empty()), &modes()),
            b"\x1bOP"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Delete, Modifiers::CTRL), &modes()),
            b"\x1b[3;5~"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Tab, Modifiers::SHIFT), &modes()),
            b"\x1b[Z"
        );
    }

    #[test]
    fn kitty_disambiguation_names_the_ambiguous_keys() {
        let mut modes = modes();
        modes.kitty_keyboard = KITTY_DISAMBIGUATE;
        assert_eq!(
            encode_one(&key(KeyCode::Tab, Modifiers::empty()), &modes),
            b"\x1b[9u"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Char('i'), Modifiers::CTRL), &modes),
            b"\x1b[105;5u"
        );
        assert_eq!(
            encode_one(&key(KeyCode::Enter, Modifiers::SHIFT), &modes),
            b"\x1b[13;2u"
        );
        assert_eq!(
            encode_one(
                &key(KeyCode::Char('x'), Modifiers::CTRL | Modifiers::SHIFT),
                &modes
            ),
            b"\x1b[120;6u"
        );
    }

    #[test]
    fn kitty_reports_event_types_and_text_when_asked() {
        let mut modes = modes();
        modes.kitty_keyboard = KITTY_ALL_KEYS | KITTY_EVENT_TYPES;
        let mut release = KeyEvent::press(KeyCode::Char('a'), Modifiers::empty());
        release.kind = KeyKind::Release;
        assert_eq!(
            encode_one(&InputEvent::Key(release), &modes),
            b"\x1b[97;1:3u"
        );

        modes.kitty_keyboard = KITTY_ALL_KEYS | KITTY_TEXT;
        assert_eq!(
            encode_one(&text(KeyCode::Char('a'), Modifiers::empty(), "a"), &modes),
            b"\x1b[97;1;97u"
        );
    }

    #[test]
    fn kitty_alternate_keys_are_reported_when_asked() {
        let mut modes = modes();
        modes.kitty_keyboard = KITTY_ALL_KEYS | KITTY_ALTERNATE_KEYS;
        let mut event = KeyEvent::press(KeyCode::Char('a'), Modifiers::SHIFT);
        event.shifted = Some('A');
        assert_eq!(
            encode_one(&InputEvent::Key(event), &modes),
            b"\x1b[97:65;2u"
        );
    }

    #[test]
    fn a_paste_is_wrapped_only_while_bracketed_paste_is_on() {
        let mut modes = modes();
        let paste = InputEvent::Paste("hi\nthere".to_owned());
        assert_eq!(encode_one(&paste, &modes), b"hi\nthere");
        modes.bracketed_paste = true;
        assert_eq!(encode_one(&paste, &modes), b"\x1b[200~hi\nthere\x1b[201~");
    }

    #[test]
    fn focus_events_are_sent_only_while_the_pane_asked_for_them() {
        let mut modes = modes();
        assert!(encode_one(&InputEvent::Focus(true), &modes).is_empty());
        modes.focus_events = true;
        assert_eq!(encode_one(&InputEvent::Focus(true), &modes), b"\x1b[I");
        assert_eq!(encode_one(&InputEvent::Focus(false), &modes), b"\x1b[O");
    }

    fn mouse(button: MouseButton, action: MouseAction) -> InputEvent {
        InputEvent::Mouse(MouseEvent {
            col: 4,
            row: 2,
            button,
            action,
            mods: Modifiers::empty(),
            pixel_col: None,
            pixel_row: None,
        })
    }

    #[test]
    fn mouse_encodings_put_the_button_and_the_cell_in_the_right_fields() {
        let mut modes = modes();
        modes.mouse = MouseMode::Drag;

        modes.mouse_encoding = MouseEncoding::Sgr;
        assert_eq!(
            encode_one(&mouse(MouseButton::Left, MouseAction::Press), &modes),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            encode_one(&mouse(MouseButton::Left, MouseAction::Release), &modes),
            b"\x1b[<0;5;3m"
        );

        modes.mouse_encoding = MouseEncoding::X10;
        assert_eq!(
            encode_one(&mouse(MouseButton::Left, MouseAction::Press), &modes),
            b"\x1b[M\x20\x25\x23"
        );

        modes.mouse_encoding = MouseEncoding::Utf8;
        assert_eq!(
            encode_one(&mouse(MouseButton::WheelUp, MouseAction::Press), &modes),
            b"\x1b[M\x60\x25\x23"
        );
    }

    #[test]
    fn mouse_events_are_dropped_when_the_pane_does_not_track_mouse() {
        let mut modes = modes();
        modes.mouse = MouseMode::Off;
        assert!(encode_one(&mouse(MouseButton::Left, MouseAction::Press), &modes).is_empty());
    }

    #[test]
    fn releases_are_dropped_when_the_pane_has_no_kitty_event_types() {
        let mut event = KeyEvent::press(KeyCode::Char('a'), Modifiers::empty());
        event.kind = KeyKind::Release;
        assert!(encode_one(&InputEvent::Key(event), &modes()).is_empty());
    }
}
