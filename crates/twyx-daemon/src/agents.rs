//! Agent sessions: which agent runs in a pane, and its state from four
//! sources of different strength.
//!
//! The process source starts a session when the foreground process names an
//! agent kind, and ends it when that process is gone. Inside a session the
//! strongest source with an observation decides the state, so a weaker source
//! never overrides a stronger one. Nothing here runs on a timer: output
//! silence leaves the state as it is.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use twyx_core::agent::{AgentState, Source};
use twyx_core::pane::PaneId;
use twyx_protocol::client::{AgentEvent, AgentInfo};
use twyx_protocol::holder::{AgentRules, PromptMark, ScreenMatch, ToHolder};

use crate::daemon::Daemon;

/// What the daemon knows of a pane's foreground process and agent.
#[derive(Debug, Default)]
pub(crate) struct PaneAgent {
    foreground: Option<u32>,
    pub session: Option<Session>,
}

#[derive(Debug)]
pub(crate) struct Session {
    pub kind: String,
    /// The process the agent runs as; the session ends when it is gone.
    pid: Option<u32>,
    observed: BTreeMap<Source, (AgentState, u64)>,
    reference: Option<String>,
}

impl Session {
    fn new(kind: String, pid: Option<u32>) -> Self {
        let mut session = Self {
            kind,
            pid,
            observed: BTreeMap::new(),
            reference: None,
        };
        session.observe(Source::Process, AgentState::Unknown);
        session
    }

    fn observe(&mut self, source: Source, state: AgentState) {
        self.observed.insert(source, (state, now_ms()));
    }

    pub fn info(&self) -> AgentInfo {
        // The process source is always there, so a session always has one.
        let (source, (state, observed_ms)) = self.observed.iter().next().map_or(
            (Source::Process, (AgentState::Unknown, 0)),
            |(source, seen)| (*source, *seen),
        );
        AgentInfo {
            kind: self.kind.clone(),
            state,
            source,
            observed_ms,
            session: self.reference.clone(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

fn alive(pid: u32) -> bool {
    rustix::process::Pid::from_raw(pid.cast_signed())
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

impl Daemon {
    pub(crate) fn foreground(&mut self, pane: PaneId, pid: u32, name: &str) {
        let kind = self.agents.by_process(name).map(|kind| kind.name.clone());
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        entry.agent.foreground = Some(pid);
        let start = match (&mut entry.agent.session, kind) {
            (Some(session), Some(kind)) if session.kind == kind => {
                session.pid = Some(pid);
                false
            }
            (_, Some(kind)) => {
                entry.agent.session = Some(Session::new(kind, Some(pid)));
                true
            }
            // The agent ran a program in the foreground, or it ended.
            (Some(session), None) => {
                if session.pid.is_none_or(|agent| !alive(agent)) {
                    self.end_agent(pane);
                }
                return;
            }
            (None, None) => return,
        };
        if start {
            self.send_rules(pane);
        }
        self.broadcast_pane(pane);
    }

    /// Ends the pane's agent session, if it has one.
    pub(crate) fn end_agent(&mut self, pane: PaneId) {
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        if entry.agent.session.take().is_none() {
            return;
        }
        if let Some(link) = &entry.link
            && link.reads_screens()
        {
            link.send(ToHolder::Agent(None));
        }
        self.broadcast_pane(pane);
    }

    fn send_rules(&self, pane: PaneId) {
        let Some(entry) = self.panes.get(&pane) else {
            return;
        };
        let (Some(link), Some(session)) = (&entry.link, &entry.agent.session) else {
            return;
        };
        if let Some(kind) = self.agents.get(&session.kind)
            && link.reads_screens()
        {
            link.send(ToHolder::Agent(Some(AgentRules {
                kind: kind.name.clone(),
                rules: kind.source.clone(),
            })));
        }
    }

    fn observe(&mut self, pane: PaneId, source: Source, state: Option<AgentState>) {
        let Some(session) = self
            .panes
            .get_mut(&pane)
            .and_then(|entry| entry.agent.session.as_mut())
        else {
            return;
        };
        let before = session.info();
        match state {
            Some(state) => session.observe(source, state),
            None => {
                session.observed.remove(&source);
            }
        }
        let after = session.info();
        if (after.state, after.source) != (before.state, before.source) {
            self.broadcast_pane(pane);
        }
    }

    pub(crate) fn screen(&mut self, pane: PaneId, found: Option<ScreenMatch>) {
        self.observe(pane, Source::Screen, found.map(|found| found.state));
    }

    /// A notification or the bell means the agent stopped for the user; a
    /// prompt mark says whether a command runs.
    pub(crate) fn terminal(&mut self, pane: PaneId, signal: Terminal) {
        let state = match signal {
            Terminal::Attention => AgentState::AwaitingPrompt,
            Terminal::Mark(PromptMark::PromptStart | PromptMark::CommandExecuted(Some(_))) => {
                AgentState::AwaitingPrompt
            }
            Terminal::Mark(PromptMark::CommandStart | PromptMark::CommandExecuted(None)) => {
                AgentState::Working
            }
        };
        self.observe(pane, Source::Terminal, Some(state));
    }

    /// An integration's event. It starts a session when the pane has none and
    /// the event names a known agent kind.
    pub(crate) fn notify(
        &mut self,
        pane: PaneId,
        agent: Option<String>,
        event: AgentEvent,
        reference: Option<String>,
    ) -> Result<(), String> {
        let Some(entry) = self.panes.get_mut(&pane) else {
            return Err(format!("no pane {pane}"));
        };
        let current = entry
            .agent
            .session
            .as_ref()
            .map(|session| session.kind.clone());
        let start = match (current, agent) {
            (Some(kind), Some(named)) if kind == named => None,
            (Some(_), None) => None,
            (_, Some(named)) => Some(named),
            (None, None) => {
                return Err(format!(
                    "pane {pane} has no agent session; name the agent with --agent"
                ));
            }
        };
        if let Some(kind) = start {
            if self.agents.get(&kind).is_none() {
                return Err(format!("{kind:?} is not an agent kind"));
            }
            let entry = self.panes.get_mut(&pane).ok_or("the pane is gone")?;
            entry.agent.session = Some(Session::new(kind, entry.agent.foreground));
            self.send_rules(pane);
        }
        let Some(session) = self
            .panes
            .get_mut(&pane)
            .and_then(|entry| entry.agent.session.as_mut())
        else {
            return Err("the pane is gone".to_owned());
        };
        if reference.is_some() {
            session.reference = reference;
        }
        let state = match event {
            AgentEvent::Working => Some(AgentState::Working),
            AgentEvent::AwaitingApproval => Some(AgentState::AwaitingApproval),
            AgentEvent::AwaitingPrompt | AgentEvent::TurnFinished | AgentEvent::Error => {
                Some(AgentState::AwaitingPrompt)
            }
            AgentEvent::Session => None,
        };
        if let Some(state) = state {
            session.observe(Source::Integration, state);
        }
        self.broadcast_pane(pane);
        Ok(())
    }
}

/// A terminal signal that bears on an agent's state.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Terminal {
    /// A notification or the bell.
    Attention,
    Mark(PromptMark),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strongest_source_with_an_observation_decides() {
        let mut session = Session::new("claude".to_owned(), None);
        assert_eq!(session.info().state, AgentState::Unknown);
        assert_eq!(session.info().source, Source::Process);

        session.observe(Source::Terminal, AgentState::AwaitingPrompt);
        session.observe(Source::Integration, AgentState::Working);
        assert_eq!(session.info().state, AgentState::Working);

        session.observe(Source::Screen, AgentState::AwaitingApproval);
        assert_eq!(
            (session.info().state, session.info().source),
            (AgentState::Working, Source::Integration),
            "a weaker source never overrides a stronger one"
        );

        session.observed.remove(&Source::Integration);
        assert_eq!(session.info().state, AgentState::AwaitingApproval);
    }
}
