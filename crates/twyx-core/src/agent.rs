//! The agent kinds twyx knows by name, and the command each one starts
//! with.

/// Kind name and command.
pub const KINDS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("pi", "pi"),
    ("opencode", "opencode"),
    ("codex", "codex"),
    ("cursor", "cursor-agent"),
];

/// The command that starts an agent of `kind`.
pub fn command(kind: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, command)| *command)
}
