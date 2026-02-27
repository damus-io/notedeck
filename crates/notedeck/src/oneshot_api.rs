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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EguiWakeup, UnknownIds, FALLBACK_PUBKEY};
    use enostr::{OutboxPool, OutboxSessionHandler, OutboxSubId};
    use nostrdb::{Config, Ndb, Transaction};
    use tempfile::TempDir;

    fn test_accounts_with_forced_relay(relay: &str) -> (TempDir, crate::Accounts) {
        let tmp = TempDir::new().expect("tmp dir");
        let mut ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let txn = Transaction::new(&ndb).expect("txn");
        let mut unknown_ids = UnknownIds::default();

        let accounts = crate::Accounts::new(
            None,
            vec![relay.to_owned()],
            FALLBACK_PUBKEY(),
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );

        (tmp, accounts)
    }

    /// Verifies oneshot requests are routed to the selected account's read relays
    /// and the expected filters are staged in outbox.
    #[test]
    fn oneshot_uses_selected_account_read_relays() {
        let (_tmp, accounts) = test_accounts_with_forced_relay("wss://relay-read.example.com");
        let expected_relays = accounts.selected_account_read_relays();
        assert!(!expected_relays.is_empty());

        let mut pool = OutboxPool::default();
        let filter = Filter::new().kinds(vec![1]).limit(1).build();

        {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut oneshot = OneshotApi::new(&mut outbox, &accounts);
            oneshot.oneshot(vec![filter.clone()]);
        }

        let request_id = OutboxSubId(0);
        let status = pool.status(&request_id);
        let status_relays: hashbrown::HashSet<enostr::NormRelayUrl> =
            status.keys().map(|url| (*url).clone()).collect();
        assert_eq!(status_relays, expected_relays);

        let stored_filters = pool.filters(&request_id).expect("oneshot filters");
        assert_eq!(stored_filters.len(), 1);
        assert_eq!(
            stored_filters[0].json().expect("filter json"),
            filter.json().expect("filter json")
        );
    }
}
