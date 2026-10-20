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

/// How often a client looks for a changed config file.
pub const CONFIG_POLL: Duration = Duration::from_secs(1);

/// How often a holder writes a checkpoint of a pane whose output changed.
pub const HISTORY_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(10);

/// The most a pane's history may take on disk.
pub const HISTORY_DISK_BYTES: usize = 32 << 20;

/// The log a pane writes between checkpoints before it checkpoints early.
pub const HISTORY_LOG_BYTES: usize = 4 << 20;

/// The records the history writer may have queued before output is dropped
/// as a gap.
pub const HISTORY_QUEUE: usize = 1024;

/// How often the daemon looks for a changed HEAD or index in the worktrees
/// that clients show.
pub const GIT_POLL: Duration = Duration::from_secs(1);

/// How often the daemon reads the status of a worktree a client shows when
/// nothing in its Git dir changed, to catch edits to the files.
pub const GIT_STATUS_INTERVAL: Duration = Duration::from_secs(5);

/// The most unpushed commits a removal prompt lists; it counts the rest.
pub const PROMPT_COMMITS: usize = 20;

/// How long a confirmed worktree removal waits for the panes it kills.
pub const REMOVAL_KILL_WAIT: Duration = Duration::from_secs(10);

/// How long a pane's output must pause before the holder reads its screen
/// against the agent's screen rules.
pub const SCREEN_SETTLE: Duration = Duration::from_millis(250);

/// The longest the holder goes without reading a changing screen, so an
/// agent that animates without pause is still read.
pub const SCREEN_READ_MAX: Duration = Duration::from_secs(1);

/// How often the holder looks at the pane's foreground process.
pub const FOREGROUND_POLL: Duration = Duration::from_millis(500);

/// The shortest time between two notifications for one pane; one that
/// comes sooner stays in the inbox only.
pub const NOTIFY_PANE_GAP: Duration = Duration::from_secs(5);

/// The shortest time between any two notifications.
pub const NOTIFY_GAP: Duration = Duration::from_secs(1);

/// The most characters of a notification's text.
pub const NOTIFY_TEXT_MAX: usize = 120;
