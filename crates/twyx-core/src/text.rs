//! Text from repositories and panes, made safe to print to a terminal.

/// `text` with every control character written as an escape, such as
/// `\u{1b}`, so a file name or a commit subject cannot move the cursor,
/// erase a line or set the clipboard of the terminal that prints it.
pub fn visible(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() {
                ch.escape_unicode().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect()
}

/// `text` as `visible` writes it, but with its line breaks and tabs kept:
/// for whole blocks of output.
pub fn printable(text: &str) -> String {
    text.split('\n')
        .map(|line| line.split('\t').map(visible).collect::<Vec<_>>().join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `text` holds a control character other than a line break or a
/// tab. A repository command with one could show the user one thing and
/// run another, so twyx refuses it.
pub fn hides(text: &str) -> bool {
    text.chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
}

/// Whether a link from pane output may reach a client (Invariant 14): only
/// `http`, `https` and `file` targets.
pub fn link_allowed(uri: &str) -> bool {
    let scheme = uri
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https" | "file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_print_as_escapes() {
        assert_eq!(visible("a\x1b[2K\rb\u{9b}"), "a\\u{1b}[2K\\u{d}b\\u{9b}");
        assert_eq!(visible("plain ünïcode"), "plain ünïcode");
        assert!(link_allowed("HTTPS://example.com") && link_allowed("file:///tmp/a"));
        assert!(!link_allowed("vscode://open") && !link_allowed("x-apple.systempreferences:a"));
        assert!(!link_allowed("no scheme"));
        assert_eq!(printable("a\tb\nc\x07"), "a\tb\nc\\u{7}");
    }
}
