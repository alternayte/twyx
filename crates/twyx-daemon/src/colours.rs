//! The paper and ink each pane reports to its programs: those of the
//! client that holds the size lease of the pane's tab, else, for a pane in
//! no tab, those of the client used last.

use twyx_core::pane::PaneId;
use twyx_protocol::holder::{PaneColours, ToHolder};

use crate::daemon::Daemon;

impl Daemon {
    pub(crate) fn set_client_colours(&mut self, client: u64, colours: PaneColours) {
        let Some(entry) = self.clients.get_mut(&client) else {
            return;
        };
        entry.colours = Some(colours);
        let leased: Vec<u64> = self
            .tabs
            .iter()
            .filter(|(_, tab)| tab.lease == Some(client))
            .map(|(id, _)| *id)
            .collect();
        for tab in leased {
            self.send_tab_colours(tab);
        }
    }

    /// Gives every pane of a tab its leaseholder's colours.
    pub(crate) fn send_tab_colours(&self, tab: u64) {
        let Some(entry) = self.tab(tab) else {
            return;
        };
        for pane in entry.layout.panes() {
            self.send_colours(pane);
        }
    }

    /// Gives a pane the colours it reports, when a client has given any.
    pub(crate) fn send_colours(&self, pane: PaneId) {
        let Some(link) = self
            .panes
            .get(&pane)
            .and_then(|entry| entry.link.as_ref())
            .filter(|link| link.takes_colours())
        else {
            return;
        };
        let leaseholder = self
            .leaf_of(pane)
            .and_then(|(tab, _)| self.tab(tab)?.lease)
            .and_then(|client| self.clients.get(&client)?.colours);
        let colours = leaseholder.or_else(|| {
            self.clients
                .values()
                .filter(|client| client.interactive && client.colours.is_some())
                .max_by_key(|client| client.used)
                .and_then(|client| client.colours)
        });
        if let Some(colours) = colours {
            link.send(ToHolder::Colours(colours));
        }
    }
}
