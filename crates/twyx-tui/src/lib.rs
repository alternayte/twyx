//! The terminal client: compositor, emitter and chrome widgets.
//!
//! Ratatui draws the chrome into a buffer. Pane cells go into the frame grid
//! directly, through the twyx emitter.

pub mod app;
pub mod chrome;
pub mod compositor;
pub mod emitter;
pub mod input;
pub mod mouse;
pub mod probe;
pub mod specimen;
