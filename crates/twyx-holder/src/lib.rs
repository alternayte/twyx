//! One holder per pane: the PTY, the terminal engine, the input encoders, the
//! scrollback and the input queue.
//!
//! This crate is the only place that touches a PTY or a terminal engine.

pub mod encode;
pub mod engine;
pub mod frame;
pub mod history;
pub mod pty;
pub mod queue;
pub mod runtime;
pub mod session;
pub mod signals;
