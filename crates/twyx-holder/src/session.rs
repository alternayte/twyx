//! Session detachment and the scrollback budget.

use twyx_core::tuning;

/// Leaves the daemon's session so the pane survives the daemon.
///
/// A process that is already a group leader cannot start a session; that only
/// happens when a person runs a holder by hand, and then isolation is moot.
pub fn detach() {
    let _ = rustix::process::setsid();
}

/// The scrollback line limit that keeps one pane inside its byte budget.
///
/// The estimate is deliberately generous, so a pane under this limit cannot
/// exceed the budget however wide its characters are.
pub fn lines(budget_bytes: usize, cols: u16) -> u32 {
    let per_line = usize::from(cols.max(1)) * tuning::SCROLLBACK_CELL_BYTES;
    let lines = budget_bytes / per_line.max(1);
    u32::try_from(lines).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_17_the_scrollback_budget_bounds_the_lines_a_pane_keeps() {
        for cols in [2_u16, 80, 200, 500] {
            let limit = lines(tuning::PANE_SCROLLBACK_BYTES, cols);
            let worst_case = usize::from(cols) * tuning::SCROLLBACK_CELL_BYTES * limit as usize;
            assert!(worst_case <= tuning::PANE_SCROLLBACK_BYTES, "cols {cols}");
        }
    }
}
