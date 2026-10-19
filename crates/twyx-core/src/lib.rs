//! Headless models that both clients share.
//!
//! Commands and keymap, layout tree, inbox, review and diff model, Markdown
//! AST, theme tokens, tuning values. No I/O beyond config parsing.

pub mod config;
pub mod keys;
pub mod launch;
pub mod layout;
pub mod palette;
pub mod pane;
pub mod paths;
pub mod theme;
pub mod tuning;
