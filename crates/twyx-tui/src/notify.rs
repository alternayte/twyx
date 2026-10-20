//! Notifications to the outer terminal: OSC 9 or OSC 777, as the terminal
//! that answered the probe takes them.
//!
//! tmux drops both from its panes. It passes OSC 9 through to its own
//! terminal when wrapped in its DCS passthrough and `allow-passthrough` is
//! on; with it off, tmux drops the wrapper too.

/// How this terminal takes a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// `OSC 9 ; body`: Ghostty, iTerm2, WezTerm, kitty.
    Osc9,
    /// `OSC 777 ; notify ; title ; body`: foot, urxvt.
    Osc777,
    /// OSC 9 in tmux's passthrough.
    Tmux,
}

impl Form {
    /// The form for the terminal the probe named, else for what the
    /// environment says.
    pub fn of(name: Option<&str>, term_program: Option<&str>, in_tmux: bool) -> Option<Self> {
        if in_tmux {
            return Some(Self::Tmux);
        }
        let name = name.or(term_program)?.to_ascii_lowercase();
        let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));
        if starts(&["tmux"]) {
            Some(Self::Tmux)
        } else if starts(&["ghostty", "iterm", "wezterm", "kitty"]) {
            Some(Self::Osc9)
        } else if starts(&["foot", "rxvt", "urxvt"]) {
            Some(Self::Osc777)
        } else {
            None
        }
    }

    /// The bytes that show `title` and `body`. Their control characters,
    /// and `;` in the title of OSC 777, cannot end the sequence early.
    pub fn encode(self, title: &str, body: &str) -> String {
        let clean = |text: &str| -> String {
            text.chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect()
        };
        let body = clean(body);
        match self {
            // A body that starts `4;` would be a progress report to Ghostty.
            Self::Osc9 => format!("\x1b]9;twyx: {body}\x1b\\"),
            Self::Osc777 => format!(
                "\x1b]777;notify;{};{body}\x1b\\",
                clean(title).replace(';', ",")
            ),
            Self::Tmux => format!("\x1bPtmux;\x1b\x1b]9;twyx: {body}\x07\x1b\\"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_follows_the_terminal_and_text_cannot_end_the_sequence() {
        assert_eq!(Form::of(Some("ghostty"), None, false), Some(Form::Osc9));
        assert_eq!(Form::of(None, Some("iTerm.app"), false), Some(Form::Osc9));
        assert_eq!(Form::of(Some("foot"), None, false), Some(Form::Osc777));
        assert_eq!(Form::of(Some("ghostty"), None, true), Some(Form::Tmux));
        assert_eq!(Form::of(Some("xterm"), None, false), None);
        assert_eq!(
            Form::Osc9.encode("twyx", "a\x1b]0;x\x07b"),
            "\x1b]9;twyx: a ]0;x b\x1b\\"
        );
        assert_eq!(
            Form::Osc777.encode("t;w", "done"),
            "\x1b]777;notify;t,w;done\x1b\\"
        );
        assert_eq!(
            Form::Tmux.encode("twyx", "done"),
            "\x1bPtmux;\x1b\x1b]9;twyx: done\x07\x1b\\"
        );
    }
}
