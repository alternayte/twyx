//! Tuning values: budgets, caps, timers, grace periods and rate limits.
//!
//! Every tuning value in twyx is read from this module.

use std::time::Duration;

/// How long the holder collects output before it publishes one frame.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// The largest message either side accepts on a socket.
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;

/// The most scrollback one pane may hold, in bytes.
pub const PANE_SCROLLBACK_BYTES: usize = 8 << 20;

/// The bytes one scrollback cell is budgeted at. A generous bound, so a wide
/// cell cannot exceed the pane's budget.
pub const SCROLLBACK_CELL_BYTES: usize = 64;

/// The most scrollback all panes may hold together, in bytes.
pub const SCROLLBACK_BYTES: usize = 512 << 20;

/// How long a holder keeps a finished pane's exit status and screen while it
/// waits for the daemon to acknowledge them.
pub const EXIT_GRACE: Duration = Duration::from_secs(30);

/// The longest title a pane reports, in characters.
pub const TITLE_CHARS: usize = 512;

/// The longest a single notification field may be, in characters.
pub const NOTIFICATION_CHARS: usize = 512;
