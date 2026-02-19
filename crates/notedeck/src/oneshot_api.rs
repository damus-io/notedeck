use enostr::RelayUrlPkgs;
use nostrdb::Filter;

use crate::{Accounts, Outbox};

/// App-facing one-shot relay API.
///
/// This keeps transient read requests (REQ/EOSE) separate from durable
/// scoped subscriptions.
pub struct OneshotApi<'o, 'a> {
    pool: &'o mut Outbox<'a>,
    accounts: &'o Accounts,
}

impl<'o, 'a> OneshotApi<'o, 'a> {
    pub fn new(pool: &'o mut Outbox<'a>, accounts: &'o Accounts) -> Self {
        Self { pool, accounts }
    }

    /// Send a one-shot request to the selected account's read relay set.
    pub fn oneshot(&mut self, filters: Vec<Filter>) {
        self.pool.oneshot(
            filters,
            RelayUrlPkgs::new(self.accounts.selected_account_read_relays()),
        );
    }
}
