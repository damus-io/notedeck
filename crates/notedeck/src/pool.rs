use enostr::{OutboxSessionHandler, OutboxSubId, RelayUrlPkgs};
use nostrdb::{Filter, Note};

use crate::{Accounts, EguiWakeup};

// A helpful wrapper for simple legacy-like operations
pub struct RelayPool<'o, 'a> {
    outbox: &'o mut OutboxSessionHandler<'a, EguiWakeup>,
    accounts: &'o Accounts,
}

impl<'o, 'a> RelayPool<'o, 'a> {
    pub fn new(
        outbox: &'o mut OutboxSessionHandler<'a, EguiWakeup>,
        accounts: &'o Accounts,
    ) -> Self {
        Self { outbox, accounts }
    }

    pub fn broadcast_note(&mut self, note: &Note) {
        self.outbox
            .broadcast_note(note, self.accounts.selected_account_write_relays());
    }

    pub fn subscribe(&mut self, filters: Vec<Filter>) -> OutboxSubId {
        self.outbox.subscribe(
            filters,
            RelayUrlPkgs::new(self.accounts.selected_account_read_relays()),
        )
    }

    pub fn oneshot(&mut self, filters: Vec<Filter>) {
        self.outbox.oneshot(
            filters,
            RelayUrlPkgs::new(self.accounts.selected_account_read_relays()),
        );
    }

    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        self.outbox.unsubscribe(id);
    }
}
