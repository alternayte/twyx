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

/// The longest operating system command the holder reads from pane output.
pub const MAX_OSC_BYTES: usize = 4096;

/// The longest title a pane reports, in characters.
pub const TITLE_CHARS: usize = 512;

/// The longest a single notification field may be, in characters.
pub const NOTIFICATION_CHARS: usize = 512;

/// The share of the tab area a new floating pane takes, on each axis.
pub const FLOATING_SIZE: f32 = 0.6;

/// How far each new floating pane is offset from the last, as a share of the
/// tab area.
pub const FLOATING_CASCADE: f32 = 0.03;

/// The smallest share a split gives one child.
pub const MIN_RATIO: f32 = 0.05;

/// How far one resize step moves a rule, as a share of the parent split.
pub const RESIZE_STEP: f32 = 0.05;

/// How far one move step takes a floating pane, as a share of the tab area.
pub const FLOATING_STEP: f32 = 0.05;
