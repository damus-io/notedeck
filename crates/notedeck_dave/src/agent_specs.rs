//! Agent specs on this host: the account's kind-31993 records, loaded from
//! nostrdb and kept live by a subscription, over the built-ins.
//!
//! A spec is looked up record-first ([`record::lookup`]): the account's
//! record of a slug replaces the built-in of that slug, and removing the
//! record brings the built-in back. Records are account-wide, like backend
//! profiles, so `nd agentium spec set` on any host of the account changes
//! the spec every host runs.

use crate::account_records::{AccountRecord, AccountRecords};
use agentium_core::agent_spec::record::{
    self, agent_spec_record_from_note, LookupError, SpecRecord, AI_AGENT_SPEC_KIND,
};
use agentium_core::agent_spec::AgentSpec;

/// The selected account's spec records, keyed by slug, and the ndb
/// subscription that refreshes them.
pub(crate) type AgentSpecs = AccountRecords<SpecRecord>;

/// A kind-31993 note, `None` for a tombstone. A record whose spec doesn't
/// read is kept (and logged on load), so a lookup can say why it won't run.
impl AccountRecord for SpecRecord {
    const KIND: u32 = AI_AGENT_SPEC_KIND;
    const NAME: &'static str = "agent spec";

    fn from_note(note: &nostrdb::Note) -> Option<Self> {
        agent_spec_record_from_note(note)
    }

    fn unusable(&self) -> Option<&str> {
        self.spec.as_ref().err().map(String::as_str)
    }
}

impl AccountRecords<SpecRecord> {
    /// The spec named `slug`: the account's record, else the built-in.
    pub(crate) fn find(&self, slug: &str) -> Result<AgentSpec, LookupError> {
        record::lookup(slug, self.records())
    }

    /// Every spec this host can run, in slug order: what the backend picker
    /// offers. A record that doesn't read is left out (a session started
    /// under it would fail its first turn with why).
    pub(crate) fn all(&self) -> Vec<AgentSpec> {
        record::slugs(self.records())
            .into_iter()
            .filter_map(|slug| self.find(slug).ok())
            .collect()
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
        ingest_spec_record_as(&test.ndb, &test.sk, built).await;
    }

    /// Ingest `built` PNS-wrapped as `sk` and wait for its inner note,
    /// subscribed before the ingest.
    async fn ingest_spec_record_as(
        ndb: &nostrdb::Ndb,
        sk: &[u8; 32],
        built: agentium_core::session_events::BuiltEvent,
    ) {
        let filter = nostrdb::Filter::new()
            .kinds([AI_AGENT_SPEC_KIND as u64])
            .build();
        let waiter = ndb.subscribe(&[filter]).unwrap();
        ingest_built_event(Ok(built), "agent-spec", ndb, sk);
        ndb.wait_for_all_notes_within(waiter, 1, Duration::from_secs(5))
            .await
            .unwrap();
    }

    /// A record published to the account's PNS (as `nd agentium spec set`
    /// does from any host) replaces the built-in through the poll, and its
    /// tombstone brings the built-in back.
    #[tokio::test]
    async fn a_record_replaces_the_builtin_until_its_tombstone() {
        let mut test = crate::tests::TestDave::new();
        let mut specs = AgentSpecs::default();
        specs.subscribe(&mut test.ndb, test.account);
        specs.load(&test.ndb);
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
        assert!(specs.poll(&test.ndb));
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
        assert!(specs.poll(&test.ndb));
        assert_eq!(
            specs.find("butler").unwrap(),
            builtin::find("butler").unwrap()
        );
    }

    /// Switching accounts moves the specs with it: the second account's
    /// record is what the store finds, and switching back finds the first
    /// account's specs again. The store used to stay on the first account's
    /// subscription, so a switch kept the first account's specs.
    #[tokio::test]
    async fn specs_follow_an_account_switch() {
        use crate::pns_runtime::PnsLocalState;

        let mut test = crate::tests::TestDave::new();
        let other_sk = [7u8; 32];
        assert!(test.ndb.add_key(&other_sk), "ndb must accept the PNS key");
        let other = nostrdb_net::FullKeypair::from_secret_bytes(&other_sk)
            .unwrap()
            .pubkey;
        let as_account = |account| PnsLocalState {
            account,
            has_secret_key: true,
        };
        test.dave.pns_local_state = None;
        test.dave
            .apply_pns_local_state(&mut test.ndb, as_account(test.account));

        let mut butler = builtin::find("butler").unwrap();
        butler.prompt = "The other account's butler.".into();
        ingest_spec_record_as(
            &test.ndb,
            &other_sk,
            build_agent_spec_event_at(&butler, Some(1_770_000_000), &other_sk).unwrap(),
        )
        .await;
        assert!(
            !test.dave.agent_specs.poll(&test.ndb),
            "another account's record is not this one's"
        );

        test.dave
            .apply_pns_local_state(&mut test.ndb, as_account(other));
        assert_eq!(test.dave.agent_specs.find("butler").unwrap(), butler);

        // The live subscription moved too: the other account's next edit
        // reaches the store through the poll.
        butler.prompt = "Edited after the switch.".into();
        ingest_spec_record_as(
            &test.ndb,
            &other_sk,
            build_agent_spec_event_at(&butler, Some(1_770_000_001), &other_sk).unwrap(),
        )
        .await;
        assert!(test.dave.agent_specs.poll(&test.ndb), "the edit is polled");
        assert_eq!(test.dave.agent_specs.find("butler").unwrap(), butler);

        test.dave
            .apply_pns_local_state(&mut test.ndb, as_account(test.account));
        assert_eq!(
            test.dave.agent_specs.find("butler").unwrap(),
            builtin::find("butler").unwrap()
        );
    }
}
