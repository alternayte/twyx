//! Headless models that both clients share.
//!
//! Commands and keymap, layout tree, inbox, review and diff model, Markdown
//! AST, theme tokens, tuning values. No I/O beyond config parsing.

pub mod agent;
pub mod config;
pub mod inbox;
pub mod integration;
pub mod keys;
pub mod launch;
pub mod layout;
pub mod palette;
pub mod pane;
pub mod paths;
pub mod recipe;
pub mod setup;
pub mod specimen;
pub mod theme;
pub mod tuning;
