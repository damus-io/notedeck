//! Agent specs on this host: the account's kind-31993 records, loaded from
//! nostrdb and kept live by a subscription, over the built-ins.
//!
//! A spec is looked up record-first ([`record::lookup`]): the account's
//! record of a slug replaces the built-in of that slug, and removing the
//! record brings the built-in back. Records are account-wide, like backend
//! profiles, so `nd agentium spec set` on any host of the account changes
//! the spec every host runs.

use std::collections::BTreeMap;

use agentium_core::agent_spec::record::{
    self, load_agent_spec_records_from_ndb, LookupError, SpecRecord, AI_AGENT_SPEC_KIND,
};
use agentium_core::agent_spec::AgentSpec;

/// The selected account's spec records, keyed by slug, and the ndb
/// subscription that refreshes them.
#[derive(Default)]
pub(crate) struct AgentSpecs {
    records: BTreeMap<String, SpecRecord>,
    sub: Option<nostrdb::Subscription>,
}

impl AgentSpecs {
    /// The spec named `slug`: the account's record, else the built-in.
    pub(crate) fn find(&self, slug: &str) -> Result<AgentSpec, LookupError> {
        record::lookup(slug, &self.records)
    }

    /// Every spec this host can run, in slug order: what the backend picker
    /// offers. A record that doesn't read is left out (a session started
    /// under it would fail its first turn with why).
    pub(crate) fn all(&self) -> Vec<AgentSpec> {
        record::slugs(&self.records)
            .into_iter()
            .filter_map(|slug| self.find(slug).ok())
            .collect()
    }

    /// Whether the live kind-31993 subscription is set up.
    pub(crate) fn is_subscribed(&self) -> bool {
        self.sub.is_some()
    }

    /// Subscribe to `account`'s kind-31993 records (the PNS-unwrapped inner
    /// notes), so [`poll`](Self::poll) sees every revision and tombstone.
    pub(crate) fn subscribe(&mut self, ndb: &nostrdb::Ndb, account: nostrdb_net::Pubkey) {
        let filter = nostrdb::Filter::new()
            .kinds([AI_AGENT_SPEC_KIND as u64])
            .authors([account.bytes()])
            .build();
        match ndb.subscribe(&[filter]) {
            Ok(sub) => self.sub = Some(sub),
            Err(e) => tracing::warn!("failed to subscribe for agent spec records: {e:?}"),
        }
    }

    /// Replace the records with `account`'s newest revision of each, as
    /// nostrdb has them now.
    pub(crate) fn load(&mut self, ndb: &nostrdb::Ndb, account: &nostrdb_net::Pubkey) {
        let txn = match nostrdb::Transaction::new(ndb) {
            Ok(txn) => txn,
            Err(err) => {
                tracing::warn!("failed to open txn for agent spec load: {err:?}");
                return;
            }
        };
        self.records = load_agent_spec_records_from_ndb(ndb, &txn, account);
        for record in self.records.values() {
            if let Err(why) = &record.spec {
                tracing::warn!("agent spec record {:?} can't be used: {why}", record.slug);
            }
        }
        tracing::info!("loaded {} agent spec records from ndb", self.records.len());
    }

    /// Reload when a record (an upsert or a tombstone) has landed since the
    /// last poll. A whole reload, as [`BackendProfiles::poll`] does: the
    /// loader already resolves each slug's newest revision and an account
    /// has a handful of specs.
    ///
    /// [`BackendProfiles::poll`]: crate::backend_profiles::BackendProfiles::poll
    pub(crate) fn poll(&mut self, ndb: &nostrdb::Ndb, account: &nostrdb_net::Pubkey) {
        let Some(sub) = self.sub else {
            return;
        };
        if ndb.poll_for_notes(sub, 64).is_empty() {
            return;
        }
        self.load(ndb, account);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::publish::ingest_built_event;
    use agentium_core::agent_spec::builtin;
    use agentium_core::agent_spec::record::{
        build_agent_spec_delete_event_at, build_agent_spec_event_at,
    };
    use std::time::Duration;

    /// Ingest `built` PNS-wrapped as the account and wait for its inner
    /// note, subscribed before the ingest.
    pub(crate) async fn ingest_spec_record(
        test: &crate::tests::TestDave,
        built: agentium_core::session_events::BuiltEvent,
    ) {
        let filter = nostrdb::Filter::new()
            .kinds([AI_AGENT_SPEC_KIND as u64])
            .build();
        let waiter = test.ndb.subscribe(&[filter]).unwrap();
        ingest_built_event(Ok(built), "agent-spec", &test.ndb, &test.sk);
        test.ndb
            .wait_for_all_notes_within(waiter, 1, Duration::from_secs(5))
            .await
            .unwrap();
    }

    /// A record published to the account's PNS (as `nd agentium spec set`
    /// does from any host) replaces the built-in through the poll, and its
    /// tombstone brings the built-in back.
    #[tokio::test]
    async fn a_record_replaces_the_builtin_until_its_tombstone() {
        let test = crate::tests::TestDave::new();
        let mut specs = AgentSpecs::default();
        specs.subscribe(&test.ndb, test.account);
        specs.load(&test.ndb, &test.account);
        assert_eq!(
            specs.find("butler").unwrap(),
            builtin::find("butler").unwrap()
        );

        let mut butler = builtin::find("butler").unwrap();
        butler.model.model = "qwen3".into();
        butler.prompt = "Be brief.".into();
        ingest_spec_record(
            &test,
            build_agent_spec_event_at(&butler, Some(1_770_000_000), &test.sk).unwrap(),
        )
        .await;
        specs.poll(&test.ndb, &test.account);
        assert_eq!(specs.find("butler").unwrap(), butler);
        assert!(
            specs.all().contains(&butler),
            "the picker offers the record"
        );

        ingest_spec_record(
            &test,
            build_agent_spec_delete_event_at("butler", Some(1_770_000_001), &test.sk).unwrap(),
        )
        .await;
        specs.poll(&test.ndb, &test.account);
        assert_eq!(
            specs.find("butler").unwrap(),
            builtin::find("butler").unwrap()
        );
    }
}
