//! Text twyx types into an agent for the user, under Invariant 5: one
//! bracketed paste, only while the pane has bracketed paste on, the agent
//! waits at its prompt by its integration or a prompt mark, and no approval
//! rule matches its screen; never with a trailing Enter. The daemon checks
//! the agent state, the holder checks the pane. When either refuses, the
//! text goes to the client's clipboard and the client says why.

use twyx_core::pane::PaneId;
use twyx_protocol::client::FromDaemon;
use twyx_protocol::holder::ToHolder;

use crate::daemon::Daemon;

/// What the text was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// A task's prompt.
    Prompt,
    /// Review notes, marked sent once delivered.
    Notes(Vec<u64>),
}

#[derive(Debug)]
pub(crate) struct Pending {
    client: u64,
    pane: PaneId,
    text: String,
    purpose: Purpose,
}

impl Daemon {
    pub(crate) fn inject(&mut self, client: u64, pane: PaneId, text: String, purpose: Purpose) {
        let pending = Pending {
            client,
            pane,
            text,
            purpose,
        };
        let link = match self.may_inject(pane) {
            Ok(link) => link,
            Err(reason) => {
                self.injection_done(pending, Err(reason));
                return;
            }
        };
        self.next_injection += 1;
        let id = self.next_injection;
        link.send(ToHolder::Inject {
            id,
            text: pending.text.clone(),
        });
        self.injections.insert(id, pending);
    }

    /// The daemon's half of Invariant 5, and the holder to ask.
    fn may_inject(&self, pane: PaneId) -> Result<crate::holders::HolderLink, String> {
        let entry = self.panes.get(&pane).ok_or("the pane is gone")?;
        let session = entry
            .agent
            .session
            .as_ref()
            .ok_or("no agent runs in the pane")?;
        if !session.trusted_prompt() {
            return Err(format!(
                "{} is not waiting at its prompt by its integration or a prompt mark",
                session.kind
            ));
        }
        let link = entry.link.as_ref().ok_or("the pane has no holder")?;
        if !link.injects() {
            return Err("the pane's holder is too old to paste for twyx".to_owned());
        }
        Ok(link.clone())
    }

    /// The holder's answer to an `Inject`.
    pub(crate) fn injected(&mut self, id: u64, result: Result<(), String>) {
        if let Some(pending) = self.injections.remove(&id) {
            self.injection_done(pending, result);
        }
    }

    fn injection_done(&mut self, pending: Pending, result: Result<(), String>) {
        let delivered = result.is_ok();
        if !delivered {
            let target = self
                .clients
                .get(&pending.client)
                .filter(|client| client.interactive)
                .map(|_| pending.client)
                .or_else(|| self.last_used_client());
            if let Some(client) = target.and_then(|id| self.clients.get(&id)) {
                self.post(
                    &client.out,
                    FromDaemon::Clipboard {
                        pane: pending.pane.raw(),
                        text: pending.text.clone(),
                    },
                );
            }
        }
        if let Purpose::Notes(notes) = &pending.purpose {
            self.notes_sent(notes);
        }
        self.reply(
            pending.client,
            FromDaemon::Delivered {
                pane: pending.pane.raw(),
                result,
            },
        );
    }
}
