//! The one per-user daemon: layouts, workspaces, worktrees, agents, the inbox,
//! review, checks, recipes, trust, the API, SQLite and the Git CLI.
//!
//! This crate is the only place that opens the database or runs Git.

mod agents;
mod api;
mod checks;
mod colours;
pub mod daemon;
mod files;
mod git;
mod holders;
mod inbox;
mod inject;
mod notes;
pub mod peer;
mod recipes;
mod review;
mod setup;
mod ship;
mod store;
mod tabs;
mod tasks;
mod workspaces;
mod worktrees;
