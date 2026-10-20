//! The one per-user daemon: layouts, workspaces, worktrees, agents, the inbox,
//! review, checks, recipes, trust, the API, SQLite and the Git CLI.
//!
//! This crate is the only place that opens the database or runs Git.

mod agents;
pub mod daemon;
mod git;
mod holders;
pub mod peer;
mod recipes;
mod setup;
mod store;
mod tabs;
mod workspaces;
mod worktrees;
