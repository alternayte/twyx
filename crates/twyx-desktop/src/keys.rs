//! GPUI keystrokes as the input events a pane receives.

use gpui::{KeyDownEvent, Keystroke};
use twyx_core::keys::Command;
use twyx_protocol::input::{KeyCode, KeyEvent, KeyKind, Modifiers};

/// A key the desktop client keeps for itself, beside the prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Copy the selection.
    Copy,
    /// Paste the clipboard into the pane.
    Paste,
    Run(Command),
}

/// The desktop shortcut a keystroke is, if any. On macOS: Cmd-T new tab,
/// Cmd-D and Cmd-Shift-D split right and down, Cmd-W close the pane,
/// Cmd-Shift-P the palette, Cmd-C and Cmd-V copy and paste. On Linux:
/// Ctrl-Shift-C and Ctrl-Shift-V.
pub fn shortcut(keystroke: &Keystroke) -> Option<Shortcut> {
    let mods = &keystroke.modifiers;
    let key = keystroke.key.to_ascii_lowercase();
    if cfg!(target_os = "macos") {
        if !mods.platform || mods.control || mods.alt {
            return None;
        }
        return Some(match (key.as_str(), mods.shift) {
            ("c", false) => Shortcut::Copy,
            ("v", false) => Shortcut::Paste,
            ("t", false) => Shortcut::Run(Command::NewTab),
            ("d", false) => Shortcut::Run(Command::SplitRight),
            ("d", true) => Shortcut::Run(Command::SplitDown),
            ("w", false) => Shortcut::Run(Command::ClosePane),
            ("p", true) => Shortcut::Run(Command::Palette),
            _ => return None,
        });
    }
    if !(mods.control && mods.shift) || mods.alt || mods.platform {
        return None;
    }
    match key.as_str() {
        "c" => Some(Shortcut::Copy),
        "v" => Some(Shortcut::Paste),
        _ => None,
    }
}

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
