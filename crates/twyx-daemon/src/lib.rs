//! The one per-user daemon: layouts, workspaces, worktrees, agents, the inbox,
//! review, checks, recipes, trust, the API, SQLite and the Git CLI.
//!
//! This crate is the only place that opens the database or runs Git.

pub mod daemon;
mod holders;
pub mod peer;
mod tabs;
