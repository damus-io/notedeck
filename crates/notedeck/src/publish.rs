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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EguiWakeup, UnknownIds, FALLBACK_PUBKEY};
    use enostr::{FullKeypair, NormRelayUrl, OutboxPool, OutboxSessionHandler};
    use nostrdb::{Config, Ndb, Note, NoteBuilder, Transaction};
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

    fn signed_note() -> Note<'static> {
        let keypair = FullKeypair::generate();
        let seckey = keypair.secret_key.to_secret_bytes();

        NoteBuilder::new()
            .kind(1)
            .content("publish-test")
            .sign(&seckey)
            .build()
            .expect("note")
    }

    /// Verifies explicit relay publishing targets only the provided relay set.
    #[test]
    fn publish_note_explicit_targets_requested_relay() {
        let (_tmp, accounts) = test_accounts_with_forced_relay("wss://relay-write.example.com");
        let note = signed_note();
        let relay = NormRelayUrl::new("wss://relay-explicit.example.com").expect("relay");
        let mut expected = hashbrown::HashSet::new();
        expected.insert(relay.clone());

        let mut pool = OutboxPool::default();
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut publish = PublishApi::new(&mut outbox, &accounts);

            publish.publish_note(
                &note,
                RelayType::Explicit(vec![RelayId::Websocket(relay.clone())]),
            );
        }
        let actual: hashbrown::HashSet<NormRelayUrl> = pool
            .websocket_statuses()
            .keys()
            .map(|url| (*url).clone())
            .collect();
        assert_eq!(actual, expected);
    }

    /// Verifies account-write publishing targets the selected account's write relays.
    #[test]
    fn publish_note_accounts_write_targets_selected_account_relays() {
        let (_tmp, accounts) =
            test_accounts_with_forced_relay("wss://relay-accounts-write.example.com");
        let note = signed_note();
        let expected_relays: hashbrown::HashSet<NormRelayUrl> = accounts
            .selected_account_write_relays()
            .into_iter()
            .filter_map(|relay| match relay {
                RelayId::Websocket(url) => Some(url),
                RelayId::Multicast => None,
            })
            .collect();
        assert!(!expected_relays.is_empty());

        let mut pool = OutboxPool::default();
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut publish = PublishApi::new(&mut outbox, &accounts);

            publish.publish_note(&note, RelayType::AccountsWrite);
        }

        let actual_relays: hashbrown::HashSet<NormRelayUrl> = pool
            .websocket_statuses()
            .keys()
            .map(|url| (*url).clone())
            .collect();
        assert_eq!(actual_relays, expected_relays);
    }
}
