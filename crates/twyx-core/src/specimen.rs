//! Sample screens for the theme specimens of both clients.

/// Text, an ANSI colour index, and bold.
pub type Run = (&'static str, Option<u8>, bool);

pub const SHELL: &[&[Run]] = &[
    &[
        ("~/twyx", Some(4), false),
        (" main", Some(5), false),
        (" $ just check", None, false),
    ],
    &[
        ("   Compiling", Some(2), true),
        (" twyx-core v0.1.0", None, false),
    ],
    &[
        ("   Compiling", Some(2), true),
        (" twyx-tui v0.1.0", None, false),
    ],
    &[
        ("    Finished", Some(2), true),
        (" `test` profile in 4.12s", None, false),
    ],
    &[
        ("test result: ", None, false),
        ("ok", Some(2), false),
        (". 41 passed; 0 failed", None, false),
    ],
    &[
        ("warning", Some(3), true),
        (": unused import: `Rect`", None, true),
    ],
    &[
        ("~/twyx", Some(4), false),
        (" main", Some(5), false),
        (" $ ", None, false),
    ],
];

pub const EDITOR: &[&[Run]] = &[
    &[
        (" 31 ", Some(8), false),
        ("pub fn ", Some(5), false),
        ("mix", Some(4), false),
        ("(self, other: ", None, false),
        ("Self", Some(3), false),
        (") -> ", None, false),
        ("Self", Some(3), false),
        (" {", None, false),
    ],
    &[
        (" 32 ", Some(8), false),
        ("    let ", Some(5), false),
        ("channel = |a: ", None, false),
        ("u8", Some(3), false),
        (", b: ", None, false),
        ("u8", Some(3), false),
        ("| {", None, false),
    ],
    &[
        (" 33 ", Some(8), false),
        ("        // gamma-space mix, rounded", Some(8), false),
    ],
    &[
        (" 34 ", Some(8), false),
        ("        (a ", None, false),
        ("as ", Some(5), false),
        ("f32", Some(3), false),
        (" * ", None, false),
        ("0.62", Some(6), false),
        (").round()", None, false),
    ],
    &[(" 35 ", Some(8), false), ("    };", None, false)],
    &[
        (" 36 ", Some(8), false),
        ("    ", None, false),
        ("\"#1a1b1e\"", Some(2), false),
    ],
    &[(" 37 ", Some(8), false), ("}", None, false)],
];

pub const AGENT: &[&[Run]] = &[
    &[
        ("> ", Some(8), false),
        ("Write the theme tokens for the TUI", None, false),
    ],
    &[],
    &[
        ("● ", Some(2), false),
        ("Read", None, true),
        (" crates/twyx-core/src/theme.rs", Some(8), false),
    ],
    &[
        ("● ", Some(2), false),
        ("Update", None, true),
        (" theme.rs (+42 -3)", Some(8), false),
    ],
    &[
        ("● ", Some(1), false),
        ("Bash", None, true),
        (" cargo test -p twyx-core", Some(8), false),
    ],
    &[
        ("  └ ", Some(8), false),
        (
            "1 failed: graphite on Light paper: 4.02 < 4.5",
            Some(1),
            false,
        ),
    ],
    &[],
    &[("Do you want to make this edit?", None, true)],
    &[("❯ 1. Yes", Some(4), false)],
    &[(
        "  2. No, and tell Claude what to do differently",
        None,
        false,
    )],
];
