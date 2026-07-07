use crate::{
    remote_data::RemoteOutboxReadModel,
    scoped_subs::{
        ScopedSubDelta, ScopedSubEffects, ScopedSubOutboxOp, ScopedSubOutboxOps, ScopedSubRuntime,
    },
};
use enostr::{
    FullKeypair, NormRelayUrl, OutboxEvent, OutboxIdRegistry, OutboxServiceOutput, OutboxSubId,
    OutboxSubRelayEose, Pubkey, RelayReqStatus,
};
use enostr_test_support::outbox::{test_outbox_service, TestOutboxService};
use nostrdb::{Filter, Ndb, Note, NoteBuilder, Transaction};
use std::{
    thread,
    time::{Duration, Instant},
};

/// Construct a signed kind `10002` relay-list event at timestamp 1.
pub(crate) fn nip65_note_for_test(
    account: &FullKeypair,
    relays: &[(&str, Option<&str>)],
) -> Note<'static> {
    nip65_note_at_for_test(account, relays, 1)
}

/// Construct a signed kind `10002` relay-list event at `created_at`.
pub(crate) fn nip65_note_at_for_test(
    account: &FullKeypair,
    relays: &[(&str, Option<&str>)],
    created_at: u64,
) -> Note<'static> {
    let mut builder = NoteBuilder::new()
        .kind(10002)
        .content("")
        .created_at(created_at);
    for (url, marker) in relays {
        builder = builder.start_tag().tag_str("r").tag_str(url);
        if let Some(marker) = marker {
            builder = builder.tag_str(marker);
        }
    }

    builder
        .sign(&account.secret_key.secret_bytes())
        .build()
        .expect("nip65 note")
}

/// Construct a signed write-relay kind `10002` event at timestamp 1.
pub(crate) fn nip65_write_relay_note_for_test(
    account: &FullKeypair,
    relays: &[&str],
) -> Note<'static> {
    nip65_write_relay_note_at_for_test(account, relays, 1)
}

/// Construct a signed write-relay kind `10002` event at `created_at`.
pub(crate) fn nip65_write_relay_note_at_for_test(
    account: &FullKeypair,
    relays: &[&str],
    created_at: u64,
) -> Note<'static> {
    let relays = relays
        .iter()
        .map(|relay| (*relay, Some("write")))
        .collect::<Vec<_>>();
    nip65_note_at_for_test(account, &relays, created_at)
}

/// Wait until local NDB imports any kind `10002` event for `pubkey`.
pub(crate) fn wait_for_nip65_for_test(ndb: &Ndb, pubkey: &Pubkey) {
    wait_for_nip65_at_for_test(ndb, pubkey, 1);
}

/// Wait until local NDB imports a kind `10002` event at or after `min_created_at`.
pub(crate) fn wait_for_nip65_at_for_test(ndb: &Ndb, pubkey: &Pubkey, min_created_at: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(ndb).expect("txn");
        let query = Filter::new()
            .authors([pubkey.bytes()])
            .kinds([10002])
            .build();
        let latest_created_at = ndb
            .query(&txn, &[query], 64)
            .expect("query")
            .iter()
            .map(|result| result.note.created_at())
            .max()
            .unwrap_or_default();
        if latest_created_at >= min_created_at {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for NIP-65 import"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) struct RemoteOutboxReadModelHarness {
    outbox: TestOutboxService,
    ids: OutboxIdRegistry,
    read_model: RemoteOutboxReadModel,
}

impl Default for RemoteOutboxReadModelHarness {
    fn default() -> Self {
        let outbox = test_outbox_service();
        let ids = outbox.id_registry();
        Self {
            outbox,
            ids,
            read_model: RemoteOutboxReadModel::default(),
        }
    }
}

impl RemoteOutboxReadModelHarness {
    pub(crate) fn scoped_runtime(&self) -> ScopedSubRuntime {
        ScopedSubRuntime::with_ids(self.ids.clone())
    }

    pub(crate) fn with_returned_outbox<R>(
        &mut self,
        f: impl FnOnce(&OutboxIdRegistry) -> R,
    ) -> R::Output
    where
        R: ReturnedScopedOutboxOps,
    {
        let ids = self.ids.clone();
        let (result, outbox_ops) = f(&ids).split();
        self.ingest_outbox_ops(outbox_ops);
        result
    }

    pub(crate) fn with_returned_outbox_ops<R>(
        &mut self,
        f: impl FnOnce(&dyn Fn(OutboxSubId, &NormRelayUrl) -> Option<RelayReqStatus>) -> R,
    ) -> R::Output
    where
        R: ReturnedScopedOutboxOps,
    {
        let read_model = &self.read_model;
        let relay_req_status = |id, relay: &NormRelayUrl| {
            read_model
                .committed_relay_req_statuses(&id)
                .get(relay)
                .copied()
        };
        let (result, outbox_ops) = f(&relay_req_status).split();
        self.ingest_outbox_ops(outbox_ops);
        result
    }

    pub(crate) fn apply_event(&mut self, event: OutboxEvent) {
        self.read_model.apply_event(event);
    }

    pub(crate) fn ingest_scoped_delta(&mut self, delta: ScopedSubDelta) -> ScopedSubEffects {
        let (_output, outbox_ops, effects) = delta.into_parts();
        self.ingest_outbox_ops(outbox_ops);
        effects
    }

    pub(crate) fn outbox_sub_relay_eose(&self, id: &OutboxSubId) -> Option<OutboxSubRelayEose> {
        self.read_model.outbox_sub_relay_eose(id)
    }

    fn apply_service_output(&mut self, output: OutboxServiceOutput) {
        let OutboxServiceOutput::Events(events) = output else {
            return;
        };

        for event in events {
            self.read_model.apply_event(event);
        }
    }

    fn ingest_outbox_ops(&mut self, outbox_ops: ScopedSubOutboxOps) {
        if outbox_ops.is_empty() {
            return;
        }

        self.outbox.begin_effect_turn();
        for op in outbox_ops.into_ops() {
            let output = self.apply_outbox_op(op);
            self.apply_service_output(output);
        }
        let output = self.outbox.end_effect_turn();
        self.apply_service_output(output);
    }

    fn apply_outbox_op(&mut self, op: ScopedSubOutboxOp) -> OutboxServiceOutput {
        match op {
            ScopedSubOutboxOp::SetLive {
                id,
                filters,
                relay_pkgs,
            } => self.outbox.set_live(id, filters, relay_pkgs),
            ScopedSubOutboxOp::StartFetch {
                id,
                filters,
                relay_pkgs,
            } => self.outbox.start_fetch(id, filters, relay_pkgs),
            ScopedSubOutboxOp::UnsubscribeLive { id } => self.outbox.clear_live(id),
            ScopedSubOutboxOp::ClearFetch { id } => self.outbox.clear_fetch(id),
            ScopedSubOutboxOp::SetFullHistoryTargets { id, targets } => {
                self.outbox.set_full_history_targets(id, targets)
            }
            ScopedSubOutboxOp::RemoveFullHistory { id } => self.outbox.clear_full_history(id),
        }
    }
}

pub(crate) trait ReturnedScopedOutboxOps {
    type Output;

    fn split(self) -> (Self::Output, ScopedSubOutboxOps);
}

impl<T> ReturnedScopedOutboxOps for (T, ScopedSubOutboxOps) {
    type Output = T;

    fn split(self) -> (Self::Output, ScopedSubOutboxOps) {
        self
    }
}

impl<T> ReturnedScopedOutboxOps for (T, ScopedSubOutboxOps, ScopedSubEffects) {
    type Output = (T, ScopedSubEffects);

    fn split(self) -> (Self::Output, ScopedSubOutboxOps) {
        let (result, outbox_ops, effects) = self;
        ((result, effects), outbox_ops)
    }
}

impl ReturnedScopedOutboxOps for (ScopedSubOutboxOps, ScopedSubEffects) {
    type Output = ((), ScopedSubEffects);

    fn split(self) -> (Self::Output, ScopedSubOutboxOps) {
        let (outbox_ops, effects) = self;
        (((), effects), outbox_ops)
    }
}

impl ReturnedScopedOutboxOps for ScopedSubOutboxOps {
    type Output = ();

    fn split(self) -> (Self::Output, ScopedSubOutboxOps) {
        ((), self)
    }
}
