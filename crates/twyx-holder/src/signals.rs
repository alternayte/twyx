//! Signals the holder reads from the pane's output itself.
//!
//! No terminal engine reports notifications, prompt marks, working directories
//! or link targets the same way, so the holder scans the byte stream for them.
//! Titles, bells and clipboard writes come from the engine, which already
//! parses those.

use twyx_core::tuning;
use twyx_protocol::holder::PromptMark;

/// One thing the pane's output told the holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneSignal {
    Notification {
        title: String,
        body: String,
    },
    PromptMark(PromptMark),
    WorkingDirectory(String),
    /// A link target that pane output named. The client decides whether to
    /// offer it; only `http`, `https` and `file` are ever opened.
    Link(String),
}

/// Reads operating system commands from split output.
#[derive(Debug, Default)]
pub struct Scanner {
    /// A sequence that has not ended yet, without its `ESC ]` prefix.
    pending: Vec<u8>,
    in_sequence: bool,
    /// The byte before the current one was an escape.
    escaped: bool,
}

impl Scanner {
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<PaneSignal> {
        let mut signals = Vec::new();
        for &byte in bytes {
            if !self.in_sequence {
                if self.escaped && byte == b']' {
                    self.in_sequence = true;
                    self.pending.clear();
                    self.escaped = false;
                } else {
                    self.escaped = byte == 0x1b;
                }
                continue;
            }

            match byte {
                // BEL ends an operating system command.
                0x07 => {
                    self.finish(&mut signals);
                }
                // ST, as ESC \.
                0x1b => {
                    self.escaped = true;
                }
                b'\\' if self.escaped => {
                    self.escaped = false;
                    self.finish(&mut signals);
                }
                _ => {
                    self.escaped = false;
                    if self.pending.len() < tuning::MAX_OSC_BYTES {
                        self.pending.push(byte);
                    } else {
                        // A sequence this long is not one twyx reads.
                        self.in_sequence = false;
                        self.pending.clear();
                    }
                }
            }
        }
        signals
    }

    fn finish(&mut self, signals: &mut Vec<PaneSignal>) {
        self.in_sequence = false;
        self.escaped = false;
        if let Ok(payload) = std::str::from_utf8(&self.pending)
            && let Some(signal) = parse(payload)
        {
            signals.push(signal);
        }
        self.pending.clear();
    }
}

fn parse(payload: &str) -> Option<PaneSignal> {
    let (number, rest) = payload.split_once(';')?;
    match number {
        "9" => Some(PaneSignal::Notification {
            title: String::new(),
            body: rest.to_owned(),
        }),
        // `777 ; notify ; title ; body`
        "777" => {
            let (kind, rest) = rest.split_once(';')?;
            if kind != "notify" {
                return None;
            }
            Some(match rest.split_once(';') {
                Some((title, body)) => PaneSignal::Notification {
                    title: title.to_owned(),
                    body: body.to_owned(),
                },
                None => PaneSignal::Notification {
                    title: String::new(),
                    body: rest.to_owned(),
                },
            })
        }
        // OSC 99 carries kitty notifications in parts; the body is the text.
        "99" => {
            let mut parts = rest.splitn(3, ';');
            let _metadata = parts.next()?;
            let body = parts.next().unwrap_or_default();
            let body = if body.is_empty() {
                rest.to_owned()
            } else {
                body.to_owned()
            };
            Some(PaneSignal::Notification {
                title: String::new(),
                body,
            })
        }
        // `133 ; A` starts a prompt, `B` a command, `C` its output, `D` its end.
        "133" => {
            let (kind, rest) = match rest.split_once(';') {
                Some((kind, rest)) => (kind, Some(rest)),
                None => (rest, None),
            };
            match kind {
                "A" => Some(PaneSignal::PromptMark(PromptMark::PromptStart)),
                "B" => Some(PaneSignal::PromptMark(PromptMark::CommandStart)),
                "C" => Some(PaneSignal::PromptMark(PromptMark::CommandExecuted(None))),
                "D" => Some(PaneSignal::PromptMark(PromptMark::CommandExecuted(
                    rest.and_then(|code| code.parse().ok()),
                ))),
                _ => None,
            }
        }
        // `7 ; file://host/path`
        "7" => rest
            .starts_with("file://")
            .then(|| PaneSignal::WorkingDirectory(rest.to_owned())),
        // `8 ; params ; uri`, and an empty uri closes the link.
        "8" => {
            let (_params, uri) = rest.split_once(';')?;
            (!uri.is_empty()).then(|| PaneSignal::Link(uri.to_owned()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<PaneSignal> {
        let mut scanner = Scanner::default();
        let mut signals = Vec::new();
        for chunk in chunks {
            signals.extend(scanner.scan(chunk));
        }
        signals
    }

    #[test]
    fn an_osc_9_notification_reaches_the_daemon() {
        assert_eq!(
            scan(&[b"\x1b]9;build finished\x07"]),
            vec![PaneSignal::Notification {
                title: String::new(),
                body: "build finished".to_owned()
            }]
        );
    }

    #[test]
    fn an_osc_777_notification_keeps_its_title() {
        assert_eq!(
            scan(&[b"\x1b]777;notify;agent;needs approval\x1b\\"]),
            vec![PaneSignal::Notification {
                title: "agent".to_owned(),
                body: "needs approval".to_owned()
            }]
        );
    }

    #[test]
    fn prompt_marks_carry_the_exit_code() {
        assert_eq!(
            scan(&[b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;3\x07"]),
            vec![
                PaneSignal::PromptMark(PromptMark::PromptStart),
                PaneSignal::PromptMark(PromptMark::CommandStart),
                PaneSignal::PromptMark(PromptMark::CommandExecuted(None)),
                PaneSignal::PromptMark(PromptMark::CommandExecuted(Some(3))),
            ]
        );
    }

    #[test]
    fn a_working_directory_needs_the_file_scheme() {
        assert_eq!(
            scan(&[b"\x1b]7;file://host/tmp\x07"]),
            vec![PaneSignal::WorkingDirectory("file://host/tmp".to_owned())]
        );
        assert!(scan(&[b"\x1b]7;http://example.com\x07"]).is_empty());
    }

    #[test]
    fn a_link_is_reported_once_with_its_target() {
        assert_eq!(
            scan(&[b"\x1b]8;;https://example.com\x07text\x1b]8;;\x07"]),
            vec![PaneSignal::Link("https://example.com".to_owned())]
        );
    }

    #[test]
    fn a_sequence_split_across_reads_still_arrives() {
        assert_eq!(
            scan(&[b"\x1b]9;hel", b"lo\x07"]),
            vec![PaneSignal::Notification {
                title: String::new(),
                body: "hello".to_owned()
            }]
        );
    }

    #[test]
    fn ordinary_output_and_unknown_sequences_are_ignored() {
        assert!(scan(&[b"plain text \x1b[31mred\x1b[0m"]).is_empty());
        assert!(scan(&[b"\x1b]0;title\x07"]).is_empty());
        assert!(scan(&[b"\x1b]52;c;aGk=\x07"]).is_empty());
    }

    #[test]
    fn a_sequence_longer_than_the_limit_is_dropped() {
        let mut scanner = Scanner::default();
        let mut bytes = b"\x1b]9;".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', tuning::MAX_OSC_BYTES + 10));
        bytes.push(0x07);
        assert!(scanner.scan(&bytes).is_empty());
    }
}
