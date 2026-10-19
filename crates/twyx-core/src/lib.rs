//! Headless models that both clients share.
//!
//! Commands and keymap, layout tree, inbox, review and diff model, Markdown
//! AST, theme tokens, tuning values. No I/O beyond config parsing.

pub mod pane;
pub mod paths;
pub mod theme;
pub mod tuning;
