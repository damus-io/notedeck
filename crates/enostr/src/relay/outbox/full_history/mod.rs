mod fetch;
mod snapshot;
pub(in crate::relay::outbox) mod state;

use negentropy::NegentropyStorageVector;
pub(super) use snapshot::{
    full_history_snapshot_from_task, FullHistorySnapshot, FullHistoryUpsert,
};
pub(super) use state::{
    FullHistoryFetchRequest, FullHistoryNeed, FullHistoryNegentropyStartOutcome, FullHistoryOutput,
    FullHistoryRuntime,
};
pub use state::{
    FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult, FullHistoryLocalSetRequest,
    FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
};
use std::time::Instant;

use crate::relay::FullHistorySubId;

impl FullHistoryRuntime {
    /// Returns whether a retained full-history declaration already has the same
    /// relay/filter projection and relay transport policy.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn full_history_targets_fully_match(
        &self,
        id: FullHistorySubId,
        targets: Vec<crate::relay::FullHistoryTarget>,
    ) -> bool {
        let targets = crate::relay::normalize_full_history_targets(targets);
        self.normalized_full_history_targets_fully_match(id, &targets)
    }

    /// Returns whether a retained full-history declaration already has the same
    /// normalized relay/filter projection and relay transport policy.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn normalized_full_history_targets_fully_match(
        &self,
        id: FullHistorySubId,
        targets: &[crate::relay::FullHistoryRelayFilter],
    ) -> bool {
        self.normalized_targets_fully_match(id, targets)
    }

    /// Whether full-history catchup has no pending work for the committed target set.
    #[cfg(test)]
    pub fn full_history_catchup_complete(
        &self,
        pool: &super::OutboxPool,
        negentropy: &crate::relay::negentropy::NegentropyRuntime,
        id: FullHistorySubId,
    ) -> bool {
        if !self.initial_round_complete(id) {
            return false;
        }

        !pool.relays.iter().any(|(relay_id, relay)| {
            relay.supports_relay_subscription_ids()
                && negentropy
                    .relay(relay_id)
                    .is_some_and(|data| data.has_pending_work_for_owner(id))
        })
    }

    /// Deliver one completed local negentropy set to full-history.
    pub(super) fn apply_full_history_local_set_ready(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
        storage: NegentropyStorageVector,
    ) -> bool {
        self.apply_local_set_ready(history_id, request_id, storage)
    }

    /// Deliver one failed/dropped local negentropy set build to full-history.
    pub(super) fn apply_full_history_local_set_failed(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
    ) -> bool {
        self.apply_local_set_failed(history_id, request_id, Instant::now())
    }

    /// Deliver one completed backend storage-presence check to full-history.
    pub(super) fn apply_full_history_local_presence_result(
        &mut self,
        result: FullHistoryLocalPresenceResult,
    ) -> Option<(FullHistoryOutput, Vec<FullHistorySubId>)> {
        self.apply_local_presence_result(result, Instant::now())
    }

    /// Deliver backend storage presence for relay-fetched events.
    pub(super) fn apply_full_history_pending_ingestion_presence_result(
        &mut self,
        result: FullHistoryPendingIngestionPresenceResult,
    ) -> Vec<FullHistorySubId> {
        self.apply_pending_ingestion_presence_result(result)
    }

    /// Snapshot one committed full-history subscription.
    #[cfg(test)]
    pub(super) fn full_history_snapshot(
        &self,
        id: FullHistorySubId,
    ) -> Option<FullHistorySnapshot> {
        self.snapshot(id)
    }

    /// Build internal oneshot fetch sessions for negentropy-discovered missing
    /// events and ingest them into the relay coordinators.
    pub(super) fn stage_need_fetches(
        &mut self,
        needs: Vec<FullHistoryNeed>,
        now: Instant,
    ) -> (FullHistoryOutput, Vec<FullHistorySubId>) {
        self.queue_needs(needs);
        (self.queue_need_presence_request(now), Vec::new())
    }
}
