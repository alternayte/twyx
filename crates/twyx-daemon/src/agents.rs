//! Agent sessions: which agent runs in a pane, and its state from four
//! sources of different strength.
//!
//! The process source starts a session when the foreground process names an
//! agent kind, and ends it when that process is gone. Inside a session the
//! strongest source with an observation decides the state, so a weaker source
//! never overrides a stronger one. Nothing here runs on a timer: output
//! silence leaves the state as it is.
//!
//! A key the user sends to the pane removes the integration's observation:
//! the user answered or interrupted what it last reported, and an agent's
//! hooks do not always say so. The screen and terminal decide until the
//! integration reports again.
//!
//! Each session has a database row. The session that runs when its pane
//! ends stays open there, with the agent's own session reference, so the
//! pane can offer Resume after a host restart.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use twyx_core::agent::{AgentState, Source};
use twyx_core::pane::PaneId;
use twyx_protocol::client::{AgentEvent, AgentInfo, PaneState};
use twyx_protocol::holder::{AgentRules, PromptMark, ScreenMatch, ToHolder};

use crate::daemon::Daemon;
use crate::store::SavedAgent;

/// What the daemon knows of a pane's foreground process and agent.
#[derive(Debug, Default)]
pub(crate) struct PaneAgent {
    foreground: Option<u32>,
    pub session: Option<Session>,
    /// The session that ran when the pane ended; Resume continues it.
    pub ended: Option<Session>,
    /// For a resumed pane: the kind and reference its next session of that
    /// kind continues.
    resumed: Option<(String, String)>,
}

#[derive(Debug)]
pub(crate) struct Session {
    pub kind: String,
    /// Its `agent_sessions` row, if the database took it.
    row: Option<i64>,
    /// The process the agent runs as; the session ends when it is gone.
    pid: Option<u32>,
    observed: BTreeMap<Source, (AgentState, u64)>,
    pub reference: Option<String>,
}

impl PaneAgent {
    /// A new pane's agent state, when the pane resumes `kind`'s session
    /// `reference`.
    pub fn resuming(kind: String, reference: String) -> Self {
        Self {
            resumed: Some((kind, reference)),
            ..Self::default()
        }
    }
}

impl Session {
    fn new(kind: String, pid: Option<u32>) -> Self {
        let mut session = Self {
            kind,
            row: None,
            pid,
            observed: BTreeMap::new(),
            reference: None,
        };
        session.observe(Source::Process, AgentState::Unknown);
        session
    }

    /// A session as the last daemon wrote it.
    pub fn saved(saved: SavedAgent) -> Self {
        let mut session = Self::new(saved.kind, saved.pid);
        session.row = Some(saved.row);
        session.reference = saved.reference;
        session
            .observed
            .insert(saved.source, (saved.state, saved.observed_ms));
        session
    }

    /// The row to write, if the session has one.
    pub fn to_saved(&self) -> Option<SavedAgent> {
        let info = self.info();
        Some(SavedAgent {
            row: self.row?,
            kind: self.kind.clone(),
            reference: self.reference.clone(),
            state: info.state,
            source: info.source,
            observed_ms: info.observed_ms,
            pid: self.pid,
        })
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
                self.begin(pane, kind, Some(pid));
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

    /// Starts a session in the pane, ending the one there. A resumed pane's
    /// session continues the agent's session it resumed.
    fn begin(&mut self, pane: PaneId, kind: String, pid: Option<u32>) {
        let row = match self.store.start_agent(pane, &kind) {
            Ok(row) => Some(row),
            Err(error) => {
                eprintln!("twyx daemon: {error:#}");
                None
            }
        };
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        let old = entry.agent.session.take();
        let mut session = Session::new(kind, pid);
        session.row = row;
        if let Some((resumed, reference)) = entry.agent.resumed.take() {
            if resumed == session.kind {
                session.reference = Some(reference);
            } else {
                entry.agent.resumed = Some((resumed, reference));
            }
        }
        entry.agent.session = Some(session);
        if let Some(row) = old.and_then(|old| old.row)
            && let Err(error) = self.store.end_agent(row)
        {
            eprintln!("twyx daemon: {error:#}");
        }
    }

    /// The pane ended: its session stays open for Resume.
    pub(crate) fn pane_ended(&mut self, pane: PaneId) {
        if let Some(entry) = self.panes.get_mut(&pane) {
            entry.agent.ended = entry.agent.session.take();
            entry.agent.foreground = None;
        }
    }

    /// Brings back the sessions the last daemon wrote, once the holders that
    /// outlived it are adopted. A running pane whose agent is gone ends its
    /// session; an ended pane keeps it for Resume.
    pub(crate) fn restore_agents(&mut self, saved: Vec<(PaneId, SavedAgent)>) {
        for (pane, agent) in saved {
            let Some(entry) = self.panes.get_mut(&pane) else {
                continue;
            };
            let row = agent.row;
            let live = agent.pid.is_some_and(alive);
            let session = Session::saved(agent);
            if entry.state != PaneState::Running {
                entry.agent.ended = Some(session);
            } else if live {
                entry.agent.session = Some(session);
                self.send_rules(pane);
            } else if let Err(error) = self.store.end_agent(row) {
                eprintln!("twyx daemon: {error:#}");
            }
        }
    }

    /// The user sent the pane a key.
    pub(crate) fn user_input(&mut self, pane: PaneId) {
        let reported = self
            .panes
            .get(&pane)
            .and_then(|entry| entry.agent.session.as_ref())
            .is_some_and(|session| session.observed.contains_key(&Source::Integration));
        if reported {
            self.observe(pane, Source::Integration, None);
        }
    }

    /// The kind an ended pane can resume, with the argv that resumes it.
    pub(crate) fn resume_argv(&self, pane: PaneId) -> Option<(String, String, Vec<String>)> {
        let entry = self.panes.get(&pane)?;
        if entry.state == PaneState::Running {
            return None;
        }
        let session = entry.agent.ended.as_ref()?;
        let reference = session.reference.clone()?;
        let argv = self.agents.get(&session.kind)?.resume(&reference)?;
        Some((session.kind.clone(), reference, argv))
    }

    /// Ends the pane's agent session, if it has one.
    pub(crate) fn end_agent(&mut self, pane: PaneId) {
        let Some(entry) = self.panes.get_mut(&pane) else {
            return;
        };
        let Some(session) = entry.agent.session.take() else {
            return;
        };
        if let Some(row) = session.row
            && let Err(error) = self.store.end_agent(row)
        {
            eprintln!("twyx daemon: {error:#}");
        }
        let Some(entry) = self.panes.get(&pane) else {
            return;
        };
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
            let foreground = self
                .panes
                .get(&pane)
                .and_then(|entry| entry.agent.foreground);
            self.begin(pane, kind, foreground);
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
