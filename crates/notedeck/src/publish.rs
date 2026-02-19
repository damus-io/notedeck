use enostr::RelayId;
use nostrdb::Note;

use crate::{Accounts, Outbox};

/// Relay target policy for publishing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelayType {
    /// Publish to the selected account's write relay set.
    AccountsWrite,
    /// Publish to an explicit relay target set.
    Explicit(Vec<RelayId>),
}

/// Explicit-relay publishing API that does not depend on account state.
pub struct ExplicitPublishApi<'o, 'a> {
    pool: &'o mut Outbox<'a>,
}

impl<'o, 'a> ExplicitPublishApi<'o, 'a> {
    pub fn new(pool: &'o mut Outbox<'a>) -> Self {
        Self { pool }
    }

    /// Publish a note to an explicit relay target set.
    pub fn publish_note(&mut self, note: &Note, relays: Vec<RelayId>) {
        self.pool.broadcast_note(note, relays);
    }
}

/// Selected-account write-relay publishing API.
pub struct AccountsPublishApi<'o, 'a> {
    pool: &'o mut Outbox<'a>,
    accounts: &'o Accounts,
}

impl<'o, 'a> AccountsPublishApi<'o, 'a> {
    pub fn new(pool: &'o mut Outbox<'a>, accounts: &'o Accounts) -> Self {
        Self { pool, accounts }
    }

    /// Publish a note to the selected account's write relay set.
    pub fn publish_note(&mut self, note: &Note) {
        self.pool
            .broadcast_note(note, self.accounts.selected_account_write_relays());
    }
}

/// Compatibility wrapper over typed publishing APIs.
pub struct PublishApi<'o, 'a> {
    pool: &'o mut Outbox<'a>,
    accounts: &'o Accounts,
}

impl<'o, 'a> PublishApi<'o, 'a> {
    pub fn new(pool: &'o mut Outbox<'a>, accounts: &'o Accounts) -> Self {
        Self { pool, accounts }
    }

    pub fn explicit(&mut self) -> ExplicitPublishApi<'_, 'a> {
        ExplicitPublishApi::new(self.pool)
    }

    pub fn accounts_write(&mut self) -> AccountsPublishApi<'_, 'a> {
        AccountsPublishApi::new(self.pool, self.accounts)
    }

    pub fn publish_note(&mut self, note: &Note, relays: RelayType) {
        match relays {
            RelayType::AccountsWrite => self.accounts_write().publish_note(note),
            RelayType::Explicit(relays) => self.explicit().publish_note(note, relays),
        }
    }
}
