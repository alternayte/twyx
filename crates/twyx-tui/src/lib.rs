//! The terminal client: compositor, emitter and chrome widgets.
//!
//! Ratatui draws the chrome into a buffer. Pane cells go into the frame grid
//! directly, through the twyx emitter.
