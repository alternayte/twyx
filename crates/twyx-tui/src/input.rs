//! Turns outer terminal events into the input events a pane receives.

use crossterm::event::{
    Event, KeyCode as CrosstermCode, KeyEvent as CrosstermKey, KeyEventKind as CrosstermKind,
    KeyModifiers as CrosstermModifiers, MouseButton as CrosstermButton,
    MouseEvent as CrosstermMouse, MouseEventKind as CrosstermMouseKind,
};
use twyx_protocol::input::{
    InputEvent, KeyCode, KeyEvent, KeyKind, Modifiers, MouseAction, MouseButton, MouseEvent,
};

/// One outer terminal event, as an input event for the pane.
pub fn to_input(event: Event) -> Option<InputEvent> {
    match event {
        Event::Key(key) => key_event(key).map(InputEvent::Key),
        Event::Mouse(mouse) => mouse_event(mouse).map(InputEvent::Mouse),
        Event::Paste(text) => Some(InputEvent::Paste(text)),
        Event::FocusGained => Some(InputEvent::Focus(true)),
        Event::FocusLost => Some(InputEvent::Focus(false)),
        Event::Resize(_, _) => None,
    }
}

fn key_event(key: CrosstermKey) -> Option<KeyEvent> {
    let code = key_code(key.code)?;
    Some(KeyEvent {
        code,
        mods: modifiers(key.modifiers),
        kind: match key.kind {
            CrosstermKind::Press => KeyKind::Press,
            CrosstermKind::Repeat => KeyKind::Repeat,
            CrosstermKind::Release => KeyKind::Release,
        },
        text: text(key.code, key.modifiers),
        shifted: None,
        base: None,
    })
}

fn key_code(code: CrosstermCode) -> Option<KeyCode> {
    Some(match code {
        CrosstermCode::Char(c) => KeyCode::Char(c),
        CrosstermCode::Enter => KeyCode::Enter,
        CrosstermCode::Tab => KeyCode::Tab,
        CrosstermCode::Backspace => KeyCode::Backspace,
        CrosstermCode::Esc => KeyCode::Escape,
        CrosstermCode::Delete => KeyCode::Delete,
        CrosstermCode::Insert => KeyCode::Insert,
        CrosstermCode::Home => KeyCode::Home,
        CrosstermCode::End => KeyCode::End,
        CrosstermCode::PageUp => KeyCode::PageUp,
        CrosstermCode::PageDown => KeyCode::PageDown,
        CrosstermCode::Up => KeyCode::Up,
        CrosstermCode::Down => KeyCode::Down,
        CrosstermCode::Left => KeyCode::Left,
        CrosstermCode::Right => KeyCode::Right,
        CrosstermCode::F(number) => KeyCode::F(number),
        CrosstermCode::Null
        | CrosstermCode::BackTab
        | CrosstermCode::Menu
        | CrosstermCode::KeypadBegin
        | CrosstermCode::CapsLock
        | CrosstermCode::ScrollLock
        | CrosstermCode::NumLock
        | CrosstermCode::PrintScreen
        | CrosstermCode::Pause
        | CrosstermCode::Media(_)
        | CrosstermCode::Modifier(_) => return None,
    })
}

fn modifiers(modifiers: CrosstermModifiers) -> Modifiers {
    let mut ours = Modifiers::empty();
    ours.set(
        Modifiers::SHIFT,
        modifiers.contains(CrosstermModifiers::SHIFT),
    );
    ours.set(
        Modifiers::CTRL,
        modifiers.contains(CrosstermModifiers::CONTROL),
    );
    ours.set(Modifiers::ALT, modifiers.contains(CrosstermModifiers::ALT));
    ours.set(
        Modifiers::SUPER,
        modifiers.contains(CrosstermModifiers::SUPER),
    );
    ours.set(
        Modifiers::HYPER,
        modifiers.contains(CrosstermModifiers::HYPER),
    );
    ours.set(
        Modifiers::META,
        modifiers.contains(CrosstermModifiers::META),
    );
    ours
}

/// The text a key produced, when a terminal would insert it.
fn text(code: CrosstermCode, mods: CrosstermModifiers) -> Option<String> {
    if mods.intersects(CrosstermModifiers::CONTROL | CrosstermModifiers::ALT) {
        return None;
    }
    match code {
        CrosstermCode::Char(c) => Some(c.to_string()),
        _ => None,
    }
}

fn mouse_event(mouse: CrosstermMouse) -> Option<MouseEvent> {
    let (button, action) = match mouse.kind {
        CrosstermMouseKind::Down(button) => (mouse_button(button), MouseAction::Press),
        CrosstermMouseKind::Up(button) => (mouse_button(button), MouseAction::Release),
        CrosstermMouseKind::Drag(button) => (mouse_button(button), MouseAction::Motion),
        CrosstermMouseKind::Moved => (MouseButton::None, MouseAction::Motion),
        CrosstermMouseKind::ScrollUp => (MouseButton::WheelUp, MouseAction::Press),
        CrosstermMouseKind::ScrollDown => (MouseButton::WheelDown, MouseAction::Press),
        CrosstermMouseKind::ScrollLeft => (MouseButton::WheelLeft, MouseAction::Press),
        CrosstermMouseKind::ScrollRight => (MouseButton::WheelRight, MouseAction::Press),
    };
    Some(MouseEvent {
        col: mouse.column,
        row: mouse.row,
        button,
        action,
        mods: modifiers(mouse.modifiers),
        pixel_col: None,
        pixel_row: None,
    })
}

fn mouse_button(button: CrosstermButton) -> MouseButton {
    match button {
        CrosstermButton::Left => MouseButton::Left,
        CrosstermButton::Middle => MouseButton::Middle,
        CrosstermButton::Right => MouseButton::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_key_carries_the_text_a_pane_would_insert() {
        let event = to_input(Event::Key(CrosstermKey::new(
            CrosstermCode::Char('a'),
            CrosstermModifiers::empty(),
        )))
        .expect("a key event");
        let InputEvent::Key(key) = event else {
            panic!("expected a key");
        };
        assert_eq!(key.code, KeyCode::Char('a'));
        assert_eq!(key.text.as_deref(), Some("a"));
        assert_eq!(key.kind, KeyKind::Press);
    }

    #[test]
    fn ctrl_carries_no_text_so_the_encoder_can_send_a_control_byte() {
        let event = to_input(Event::Key(CrosstermKey::new(
            CrosstermCode::Char('c'),
            CrosstermModifiers::CONTROL,
        )))
        .expect("a key event");
        let InputEvent::Key(key) = event else {
            panic!("expected a key");
        };
        assert_eq!(key.text, None);
        assert!(key.mods.contains(Modifiers::CTRL));
    }

    #[test]
    fn a_shifted_tab_keeps_its_shift() {
        let event = to_input(Event::Key(CrosstermKey::new(
            CrosstermCode::Tab,
            CrosstermModifiers::SHIFT,
        )))
        .expect("a key event");
        let InputEvent::Key(key) = event else {
            panic!("expected a key");
        };
        assert_eq!(key.code, KeyCode::Tab);
        assert!(key.mods.contains(Modifiers::SHIFT));
    }

    #[test]
    fn a_scroll_becomes_a_wheel_press_at_the_cell() {
        let event = to_input(Event::Mouse(CrosstermMouse {
            kind: CrosstermMouseKind::ScrollUp,
            column: 3,
            row: 4,
            modifiers: CrosstermModifiers::empty(),
        }))
        .expect("a mouse event");
        let InputEvent::Mouse(mouse) = event else {
            panic!("expected a mouse");
        };
        assert_eq!(mouse.button, MouseButton::WheelUp);
        assert_eq!(mouse.action, MouseAction::Press);
        assert_eq!((mouse.col, mouse.row), (3, 4));
    }

    #[test]
    fn a_paste_and_a_focus_change_pass_straight_through() {
        assert_eq!(
            to_input(Event::Paste("hi".to_owned())),
            Some(InputEvent::Paste("hi".to_owned()))
        );
        assert_eq!(to_input(Event::FocusGained), Some(InputEvent::Focus(true)));
        assert_eq!(to_input(Event::Resize(10, 10)), None);
    }
}
