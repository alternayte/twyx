//! The inbox and the status marks: when an agent session or a pane needs
//! the user, and how urgent each pane, workspace and worktree is.

use serde::{Deserialize, Serialize};

use crate::agent::AgentState;
use crate::theme::AgentState as Mark;

/// Why an inbox item appeared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    NeedsApproval,
    TurnFinished,
    /// The pane exited with a non-zero code or a signal.
    Failed,
    /// A check run ended, with any result.
    CheckEnded,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Reason::NeedsApproval => "needs-approval",
            Reason::TurnFinished => "turn-finished",
            Reason::Failed => "failed",
            Reason::CheckEnded => "check-ended",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Reason::NeedsApproval,
            Reason::TurnFinished,
            Reason::Failed,
            Reason::CheckEnded,
        ]
        .into_iter()
        .find(|reason| reason.name() == name)
    }
}

/// Follows one agent session's state and says when it needs the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Turn {
    state: Option<AgentState>,
    /// The session worked since its last turn ended.
    worked: bool,
}

impl Turn {
    /// The session's state is now `state`. A turn ends when the session
    /// reaches its prompt after it worked, whatever it showed in between.
    pub fn moved(&mut self, state: AgentState) -> Option<Reason> {
        let before = self.state.replace(state);
        match state {
            AgentState::Working => {
                self.worked = true;
                None
            }
            AgentState::AwaitingApproval if before != Some(AgentState::AwaitingApproval) => {
                Some(Reason::NeedsApproval)
            }
            AgentState::AwaitingPrompt if self.worked => {
                self.worked = false;
                Some(Reason::TurnFinished)
            }
            _ => None,
        }
    }
}

/// Text from a pane or an agent made fit for a notification: control
/// characters become spaces, and it is cut to `max` characters.
pub fn plain(text: &str, max: usize) -> String {
    let text: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// A pane's mark: `failed` when it exited with an error, else from its agent
/// state. A finished turn shows as done until the user sees the pane.
pub fn mark(failed: bool, agent: Option<AgentState>, unread: bool) -> Option<Mark> {
    if failed {
        return Some(Mark::Failed);
    }
    Some(match agent? {
        AgentState::AwaitingApproval => Mark::NeedsApproval,
        AgentState::Working => Mark::Working,
        AgentState::AwaitingPrompt if unread => Mark::Done,
        AgentState::AwaitingPrompt => Mark::Idle,
        AgentState::None | AgentState::Unknown => Mark::Unknown,
    })
}

/// The most urgent of `marks`.
pub fn most_urgent(marks: impl IntoIterator<Item = Mark>) -> Option<Mark> {
    marks
        .into_iter()
        .min_by_key(|mark| Mark::ALL.iter().position(|each| each == mark))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_ends_at_the_prompt_after_work_even_through_an_unknown_stretch() {
        let mut turn = Turn::default();
        assert_eq!(turn.moved(AgentState::AwaitingPrompt), None, "no work yet");
        assert_eq!(turn.moved(AgentState::Working), None);
        assert_eq!(turn.moved(AgentState::Unknown), None);
        assert_eq!(
            turn.moved(AgentState::AwaitingPrompt),
            Some(Reason::TurnFinished)
        );
        assert_eq!(turn.moved(AgentState::AwaitingPrompt), None);
        assert_eq!(
            turn.moved(AgentState::AwaitingApproval),
            Some(Reason::NeedsApproval)
        );
        assert_eq!(turn.moved(AgentState::AwaitingApproval), None);
    }

    #[test]
    fn marks_roll_up_to_the_most_urgent() {
        assert_eq!(
            mark(false, Some(AgentState::AwaitingPrompt), true),
            Some(Mark::Done)
        );
        assert_eq!(
            mark(false, Some(AgentState::AwaitingPrompt), false),
            Some(Mark::Idle)
        );
        assert_eq!(mark(true, None, false), Some(Mark::Failed));
        assert_eq!(mark(false, None, true), None, "a plain pane has no mark");
        assert_eq!(
            most_urgent([Mark::Idle, Mark::Working, Mark::Failed, Mark::Done]),
            Some(Mark::Failed)
        );
        assert_eq!(most_urgent([]), None);
    }
}
