//! GPUI keystrokes as the input events a pane receives.

use gpui::{KeyDownEvent, Keystroke};
use twyx_protocol::input::{KeyCode, KeyEvent, KeyKind, Modifiers};

/// The input event a key press carries, if any.
pub fn key_event(event: &KeyDownEvent) -> Option<KeyEvent> {
    let keystroke = &event.keystroke;
    Some(KeyEvent {
        code: key_code(keystroke)?,
        mods: modifiers(keystroke),
        kind: KeyKind::Press,
        text: text(keystroke),
        shifted: None,
        base: None,
    })
}

fn key_code(keystroke: &Keystroke) -> Option<KeyCode> {
    let key = keystroke.key.as_str();
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(KeyCode::Char(c));
    }
    Some(match key {
        "space" => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "escape" => KeyCode::Escape,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        other => KeyCode::F(other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())?),
    })
}

fn modifiers(keystroke: &Keystroke) -> Modifiers {
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::SHIFT, keystroke.modifiers.shift);
    mods.set(Modifiers::CTRL, keystroke.modifiers.control);
    mods.set(Modifiers::ALT, keystroke.modifiers.alt);
    mods.set(Modifiers::SUPER, keystroke.modifiers.platform);
    mods.set(Modifiers::META, keystroke.modifiers.function);
    mods
}

/// The text the key produced, when the pane would insert it.
fn text(keystroke: &Keystroke) -> Option<String> {
    if keystroke.modifiers.control || keystroke.modifiers.platform {
        return None;
    }
    keystroke.key_char.as_ref().map(|text| text.to_string())
}

/// Whether this keystroke is the platform paste shortcut.
pub fn is_paste(keystroke: &Keystroke) -> bool {
    keystroke.modifiers.platform && keystroke.key == "v"
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers as GpuiModifiers;

    fn key(key: &str, modifiers: GpuiModifiers, text: Option<&str>) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers,
                key: key.to_owned(),
                key_char: text.map(|text| text.into()),
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[test]
    fn a_letter_key_carries_its_text() {
        let event = key_event(&key("a", GpuiModifiers::default(), Some("a"))).expect("a key");
        assert_eq!(event.code, KeyCode::Char('a'));
        assert_eq!(event.text.as_deref(), Some("a"));
    }

    #[test]
    fn control_keys_carry_no_text() {
        let event = key_event(&key(
            "c",
            GpuiModifiers {
                control: true,
                ..GpuiModifiers::default()
            },
            Some("c"),
        ))
        .expect("a key");
        assert_eq!(event.text, None);
        assert!(event.mods.contains(Modifiers::CTRL));
    }

    #[test]
    fn named_keys_become_the_keys_the_encoder_knows() {
        for (name, expected) in [
            ("escape", KeyCode::Escape),
            ("enter", KeyCode::Enter),
            ("tab", KeyCode::Tab),
            ("up", KeyCode::Up),
            ("f5", KeyCode::F(5)),
            ("space", KeyCode::Char(' ')),
        ] {
            let event = key_event(&key(name, GpuiModifiers::default(), None))
                .unwrap_or_else(|| panic!("a key for {name}"));
            assert_eq!(event.code, expected, "{name}");
        }
    }

    #[test]
    fn a_key_with_no_mapping_is_dropped() {
        assert!(key_event(&key("capslock", GpuiModifiers::default(), None)).is_none());
    }
}
