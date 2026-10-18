//! Asks the outer terminal what it can do.
//!
//! The queries are written once, and the answers are read for a short while.
//! Parsing is separate, so the answers can be tested without a terminal.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::emitter::Capabilities;

/// What the terminal answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answers {
    pub name: Option<String>,
    pub version: Option<String>,
    /// The kitty keyboard flags the terminal reports. `Some` whenever the
    /// terminal has the protocol, even with no flags set.
    pub kitty_keyboard: Option<u8>,
    pub synchronized_output: bool,
    pub truecolor: bool,
}

impl Answers {
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            synchronized_output: self.synchronized_output,
            kitty_keyboard: self.kitty_keyboard.is_some(),
            truecolor: self.truecolor,
        }
    }
}

/// The queries, in one write.
pub const QUERIES: &[u8] = b"\x1b[c\x1b[>0q\x1b[?u\x1b[?2026$p";

/// How long to wait for the answers.
const WINDOW: Duration = Duration::from_millis(200);

/// The answers, for `twyx doctor` and for the tests.
pub fn parse(bytes: &[u8]) -> Answers {
    let text = String::from_utf8_lossy(bytes);
    let mut answers = Answers::default();

    if let Some(start) = text.find("\x1bP>|")
        && let Some(end) = text[start..].find("\x1b\\")
    {
        let payload = &text[start + 4..start + end];
        match payload.split_once('(') {
            Some((name, rest)) => {
                answers.name = Some(name.to_owned());
                answers.version = rest.strip_suffix(')').map(str::to_owned);
            }
            None => match payload.split_once(' ') {
                Some((name, version)) => {
                    answers.name = Some(name.to_owned());
                    answers.version = Some(version.to_owned());
                }
                None => answers.name = Some(payload.to_owned()),
            },
        }
    }

    answers.kitty_keyboard = kitty_flags(&text);
    answers.synchronized_output = synchronized_output(&text);
    answers.truecolor = truecolor_from_env();
    answers
}

/// `CSI ? flags u` reports the kitty keyboard flags.
fn kitty_flags(text: &str) -> Option<u8> {
    let mut rest = text;
    while let Some(start) = rest.find("\x1b[?") {
        let after = &rest[start + 3..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let tail = &after[digits.len()..];
        if tail.starts_with('u')
            && let Ok(flags) = digits.parse::<u16>()
        {
            return u8::try_from(flags).ok();
        }
        rest = after;
    }
    None
}

/// `CSI ? 2026 ; Ps $ y` answers the mode request; 1 is set and 2 is reset.
fn synchronized_output(text: &str) -> bool {
    let Some(start) = text.find("\x1b[?2026;") else {
        return false;
    };
    let after = &text[start + 8..];
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    matches!(digits.parse::<u8>(), Ok(1 | 2))
}

fn truecolor_from_env() -> bool {
    matches!(
        std::env::var("COLORTERM").as_deref(),
        Ok("truecolor") | Ok("24bit")
    )
}

/// Writes the queries and reads the answers for a short while.
///
/// The terminal must already be in raw mode, and the reader is switched to
/// non-blocking so a terminal that answers nothing cannot hold up the client.
pub fn interrogate(tty: &mut (impl Read + Write + std::os::fd::AsFd)) -> Result<Answers> {
    rustix::io::ioctl_fionbio(&*tty, true).context("make the terminal non-blocking")?;
    let result = collect(tty);
    let _ = rustix::io::ioctl_fionbio(&*tty, false);
    result
}

fn collect(tty: &mut (impl Read + Write)) -> Result<Answers> {
    tty.write_all(QUERIES).context("write the queries")?;
    tty.flush().context("flush the queries")?;

    let mut answers = Vec::new();
    let mut buffer = [0_u8; 1024];
    let deadline = Instant::now() + WINDOW;
    while Instant::now() < deadline {
        match tty.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => answers.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("read the answers"),
        }
        // Nothing else is coming once both of the last two answers are in.
        if answers.ends_with(b"$y") && answers.windows(2).any(|window| window == b"u\\") {
            break;
        }
    }
    Ok(parse(&answers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ghostty_like_answer_names_the_terminal_and_its_protocols() {
        let answers = parse(b"\x1b[?62;c\x1bP>|ghostty(1.2.3)\x1b\\\x1b[?1u\x1b[?2026;1$y");
        assert_eq!(answers.name.as_deref(), Some("ghostty"));
        assert_eq!(answers.version.as_deref(), Some("1.2.3"));
        assert_eq!(answers.kitty_keyboard, Some(1));
        assert!(answers.synchronized_output);
    }

    #[test]
    fn a_terminal_that_ignores_the_probe_answers_nothing() {
        let answers = parse(b"");
        assert_eq!(answers.name, None);
        assert_eq!(answers.kitty_keyboard, None);
        assert!(!answers.synchronized_output);
    }

    #[test]
    fn a_mode_reset_still_means_synchronized_output_works() {
        assert!(parse(b"\x1b[?2026;2$y").synchronized_output);
        assert!(!parse(b"\x1b[?2026;0$y").synchronized_output);
    }

    #[test]
    fn kitty_flags_are_not_confused_with_other_private_answers() {
        assert_eq!(parse(b"\x1b[?2026;1$y").kitty_keyboard, None);
        assert_eq!(parse(b"\x1b[?0u").kitty_keyboard, Some(0));
    }
}
