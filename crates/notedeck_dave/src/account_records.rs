//! One store for an account's PNS-wrapped replaceable records: the newest
//! revision of each d-tag, loaded from nostrdb and kept live by a
//! subscription.
//!
//! Backend profiles (kind-31992) and agent specs (kind-31993) are both one of
//! these ([`BackendProfiles`], [`AgentSpecs`]); a new account-wide record is
//! an [`AccountRecord`] impl (its kind and how a note reads) plus whatever
//! lookups its callers want on `AccountRecords<ItsType>`.
//!
//! The store remembers whose records it holds. [`subscribe`] to another
//! account drops the old subscription and follows the new one, so a store
//! that outlives an account switch never keeps polling the first account's
//! filter.
//!
//! [`BackendProfiles`]: crate::backend_profiles::BackendProfiles
//! [`AgentSpecs`]: crate::agent_specs::AgentSpecs
//! [`subscribe`]: AccountRecords::subscribe

use agentium_core::session_loader::load_account_records;
use std::collections::BTreeMap;

/// A record kind an account keeps as PNS-wrapped replaceable notes, one per
/// d-tag.
pub(crate) trait AccountRecord: Sized {
    /// The inner (unwrapped) note's kind.
    const KIND: u32;
    /// What the logs call it, e.g. `"backend profile"`.
    const NAME: &'static str;

    /// The record a note spells, or `None` for a tombstone or a note that
    /// doesn't read.
    fn from_note(note: &nostrdb::Note) -> Option<Self>;

    /// Why the record can't be used, when it loaded but won't run. The store
    /// logs it on load; the record stays in the map so a lookup can say why.
    fn unusable(&self) -> Option<&str> {
        None
    }
}

/// An account's records of one kind, keyed by d-tag, and the ndb
/// subscription that refreshes them.
pub(crate) struct AccountRecords<T> {
    records: BTreeMap<String, T>,
    sub: Option<nostrdb::Subscription>,
    /// Whose records these are: the account [`subscribe`](Self::subscribe)
    /// last followed.
    account: Option<nostrdb_net::Pubkey>,
}

/// Empty and following no account. Written out rather than derived, which
/// would demand `T: Default`.
impl<T> Default for AccountRecords<T> {
    fn default() -> Self {
        Self {
            records: BTreeMap::new(),
            sub: None,
            account: None,
        }
    }
}

impl<T: AccountRecord> AccountRecords<T> {
    /// The records, by d-tag.
    pub(crate) fn records(&self) -> &BTreeMap<String, T> {
        &self.records
    }

    /// Follow `account`: subscribe to its `T::KIND` notes (the PNS-unwrapped
    /// inner notes), so [`poll`](Self::poll) sees every revision and
    /// tombstone as it lands. A no-op when already subscribed for `account`;
    /// another account's subscription is dropped first. Call
    /// [`load`](Self::load) after, to read what is already stored.
    pub(crate) fn subscribe(&mut self, ndb: &mut nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        if self.account == Some(account) && self.sub.is_some() {
            return;
        }
        if let Some(old) = self.sub.take() {
            if let Err(e) = ndb.unsubscribe(old) {
                tracing::warn!("failed to drop the old {} subscription: {e:?}", T::NAME);
            }
        }
        self.account = Some(account);
        let filter = nostrdb::Filter::new()
            .kinds([T::KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[filter]) {
            Ok(sub) => self.sub = Some(sub),
            Err(e) => tracing::warn!("failed to subscribe for {} notes: {e:?}", T::NAME),
        }
    }

    /// Replace the records with the followed account's newest live revision
    /// of each, as nostrdb has them now. Nothing to do before the first
    /// [`subscribe`](Self::subscribe).
    pub(crate) fn load(&mut self, ndb: &nostrdb::Ndb) {
        let Some(account) = self.account else {
            return;
        };
        let txn = match nostrdb::Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::warn!("failed to open txn for {} load: {err:?}", T::NAME);
                return;
            }
        };
        self.records = load_account_records(ndb, &txn, &account, T::KIND, T::from_note);
        for (slug, record) in &self.records {
            if let Some(why) = record.unusable() {
                tracing::warn!("{} {slug:?} can't be used: {why}", T::NAME);
            }
        }
        // Slugs and counts only: a profile's env holds its secrets.
        tracing::info!("loaded {} {} records from ndb", self.records.len(), T::NAME);
    }

    /// Reload when a record (an upsert or a tombstone) has landed since the
    /// last poll; answers whether it did. A whole reload rather than a fold
    /// of the new notes: the loader already resolves each d-tag's newest
    /// revision and its tombstones, and an account has a handful of records.
    pub(crate) fn poll(&mut self, ndb: &nostrdb::Ndb) -> bool {
        let Some(sub) = self.sub else {
            return false;
        };
        if ndb.poll_for_notes(sub, 64).is_empty() {
            return false;
        }
        self.load(ndb);
        true
    }

    /// Records for a test, without ndb, keyed by `key`.
    #[cfg(test)]
    pub(crate) fn from_records(
        records: impl IntoIterator<Item = T>,
        key: impl Fn(&T) -> &str,
    ) -> Self {
        Self {
            records: records
                .into_iter()
                .map(|record| (key(&record).to_owned(), record))
                .collect(),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ingest_built_event;
    use agentium_core::config::{BackendProfile, AI_BACKEND_PROFILE_KIND};
    use agentium_core::session_events::build_backend_profile_event_at;
    use std::time::Duration;

    /// Ingest `profile` PNS-wrapped as `sk` at `created_at`, and wait until
    /// its inner note is indexed (subscribed before the ingest).
    async fn ingest_profile(
        ndb: &nostrdb::Ndb,
        sk: &[u8; 32],
        profile: &BackendProfile,
        created_at: u64,
    ) {
        let filter = nostrdb::Filter::new()
            .kinds([AI_BACKEND_PROFILE_KIND as u64])
            .build();
        let waiter = ndb.subscribe(&[filter]).unwrap();
        ingest_built_event(
            build_backend_profile_event_at(profile, Some(created_at), sk),
            "backend-profile",
            ndb,
            sk,
        );
        ndb.wait_for_all_notes_within(waiter, 1, Duration::from_secs(5))
            .await
            .unwrap();
    }

    /// Two revisions of one slug in the same second: the poll's reload keeps
    /// the one a fresh load keeps, `query_replaceable`'s `(created_at, id)`
    /// winner, not whichever landed last.
    #[tokio::test]
    async fn a_same_second_tie_in_poll_resolves_like_load() {
        let mut test = crate::tests::TestDave::new();
        let mut polled = AccountRecords::<BackendProfile>::default();
        polled.subscribe(&mut test.ndb, test.account);
        polled.load(&test.ndb);

        for label in ["First", "Second"] {
            let mut hydra = BackendProfile::new("hydra", label);
            hydra.env.insert("ANTHROPIC_MODEL".into(), label.into());
            ingest_profile(&test.ndb, &test.sk, &hydra, 1_770_000_000).await;
        }
        assert!(polled.poll(&test.ndb), "the revisions reload the store");

        let mut loaded = AccountRecords::<BackendProfile>::default();
        loaded.subscribe(&mut test.ndb, test.account);
        loaded.load(&test.ndb);
        assert_eq!(polled.records(), loaded.records());
        assert_eq!(polled.records().len(), 1, "one record per slug");
    }

    /// Another author's record is no reason to reload: the subscription is
    /// scoped to the followed account.
    #[tokio::test]
    async fn another_authors_record_does_not_reload() {
        let mut test = crate::tests::TestDave::new();
        let mut store = AccountRecords::<BackendProfile>::default();
        store.subscribe(&mut test.ndb, test.account);
        store.load(&test.ndb);

        let stranger = [7u8; 32];
        assert!(test.ndb.add_key(&stranger), "ndb must accept the PNS key");
        ingest_profile(
            &test.ndb,
            &stranger,
            &BackendProfile::new("hydra", "Hydra"),
            1_770_000_000,
        )
        .await;
        assert!(!store.poll(&test.ndb), "the stranger's note isn't polled");
        assert!(store.records().is_empty());
    }
}
