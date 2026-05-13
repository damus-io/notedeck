mod fetch;
mod snapshot;
mod state;

use hashbrown::HashMap;
use nostrdb::Filter;
use std::time::Instant;
use tokio::sync::oneshot;

use fetch::stage_queued_need_fetches;
use snapshot::{
    full_history_snapshot_from_task, FullHistoryRelayFilter, FullHistorySnapshot, FullHistoryUpsert,
};
#[cfg(test)]
pub(super) use state::{
    FullHistoryFetchRetryState, PendingIngestion, TrackedFullHistorySub,
    FULL_HISTORY_RETRY_BACKOFF_BASE, INGESTION_TIMEOUT,
    MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID, MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER,
    MAX_FULL_HISTORY_ROUNDS,
};
pub(super) use state::{FullHistoryNeed, FullHistoryTracker, FULL_HISTORY_PRESENCE_CHECK_BUDGET};

use super::{stage_unsubscribe_task, OutboxPool, OutboxSession, SessionDelta};
use crate::{
    relay::{
        coordinator::{CoordinationData, NegentropyStartOutcome},
        negentropy::{EventChecker, NegSetProvider},
        subscription::FullHistoryTask,
        FullHistorySubId, NormRelayUrl,
    },
    Wakeup,
};

impl OutboxPool {
    /// Whether full-history upkeep has any pending local or relay work.
    pub(super) fn has_full_history_work(&self) -> bool {
        self.full_history.has_pending_work()
            || self
                .relays
                .values()
                .any(|relay| relay.negentropy_data.has_pending_work())
    }

    /// Earliest time-based deadline for full-history maintenance work.
    pub fn next_full_history_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        [
            self.full_history.next_deadline(now),
            self.relays
                .values()
                .filter_map(CoordinationData::next_negentropy_deadline)
                .min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Set the NegSetProvider used to build local sets for background
    /// full-history negentropy reconciliation.
    pub fn set_neg_set_provider(&mut self, provider: Box<dyn NegSetProvider>) {
        self.full_history.neg_set_provider = Some(provider);
    }

    /// Set the EventChecker used to poll ingestion of fetched events.
    pub fn set_event_checker(&mut self, checker: Box<dyn EventChecker>) {
        self.full_history.event_checker = Some(checker);
    }

    /// Snapshot one committed full-history subscription.
    #[cfg(test)]
    pub(super) fn full_history_snapshot(
        &self,
        id: FullHistorySubId,
    ) -> Option<FullHistorySnapshot> {
        self.full_history
            .tracked_subs
            .get(&id)
            .map(|tracked| tracked.snapshot.clone())
    }

    /// Drain relay-scoped ids discovered by background full-history negentropy.
    pub(super) fn drain_full_history_needs(&mut self) -> Vec<FullHistoryNeed> {
        let tracked_subs = &self.full_history.tracked_subs;
        let mut needs = Vec::new();
        for (relay_url, relay) in &mut self.relays {
            for need in relay.negentropy_data.drain_need_ids() {
                let history_id = need.owner_history_id;
                if !tracked_subs.get(&history_id).is_some_and(|tracked| {
                    tracked
                        .snapshot
                        .contains_relay_filter(relay_url, &need.filter)
                }) {
                    continue;
                }
                needs.push(FullHistoryNeed {
                    history_id,
                    relay: relay_url.clone(),
                    filter: need.filter,
                    id: need.id,
                });
            }
        }
        needs
    }

    /// Drain transient retry requests from relay-local negentropy sessions that
    /// ended before one round could complete.
    fn drain_full_history_retries(&mut self) {
        let now = Instant::now();
        let mut drained = Vec::new();
        for (relay_url, relay) in &mut self.relays {
            drained.extend(
                relay
                    .negentropy_data
                    .drain_retry_neg_sets()
                    .into_iter()
                    .map(|retry| (relay_url.clone(), retry)),
            );
        }

        for (relay_url, retry) in drained {
            let Some(tracked) = self
                .full_history
                .tracked_subs
                .get_mut(&retry.owner_history_id)
            else {
                continue;
            };
            tracked.schedule_retry(relay_url, retry.filter, now);
        }
    }

    /// Apply staged full-history tasks and ensure target relays exist before
    /// scheduling local set work.
    pub(super) fn apply_full_history_tasks<W>(
        &mut self,
        tasks: HashMap<FullHistorySubId, FullHistoryTask>,
        wakeup: &W,
    ) where
        W: Wakeup,
    {
        for (id, task) in tasks {
            match task {
                FullHistoryTask::Upsert(task) => {
                    for relay in &task.relays {
                        self.ensure_relay(relay, wakeup);
                    }
                    self.upsert_full_history_snapshot(
                        full_history_snapshot_from_task(id, &task),
                        wakeup,
                    );
                }
                FullHistoryTask::Remove => self.remove_full_history_sub(id, wakeup),
            }
        }
    }

    fn upsert_full_history_snapshot<W>(&mut self, snapshot: FullHistorySnapshot, wakeup: &W)
    where
        W: Wakeup,
    {
        let id = snapshot.id;
        let update = self.full_history.upsert(snapshot);
        match update {
            FullHistoryUpsert::Unchanged => {}
            FullHistoryUpsert::Inserted => {
                self.full_history.schedule_round(id);
            }
            FullHistoryUpsert::Changed {
                added,
                removed,
                filters_changed,
            } => {
                self.cancel_full_history_fetches_matching(id, wakeup, |relay, filter| {
                    removed.iter().any(|removed| {
                        &removed.relay == relay && removed.filter.same_canonical_attributes(filter)
                    })
                });
                self.cancel_full_history_relay_filters(id, &removed);
                if filters_changed {
                    self.full_history.schedule_round(id);
                } else {
                    self.full_history.schedule_relay_filters(id, added);
                }
            }
        }
    }

    fn remove_full_history_sub<W>(&mut self, id: FullHistorySubId, wakeup: &W)
    where
        W: Wakeup,
    {
        self.cancel_full_history_owner(id);
        self.cancel_full_history_fetches(id, wakeup);
        self.full_history.remove(id);
    }

    fn cancel_full_history_fetches<W>(&mut self, id: FullHistorySubId, wakeup: &W)
    where
        W: Wakeup,
    {
        self.cancel_full_history_fetches_matching(id, wakeup, |_, _| true);
    }

    fn cancel_full_history_fetches_matching<W, F>(
        &mut self,
        id: FullHistorySubId,
        wakeup: &W,
        matches: F,
    ) where
        W: Wakeup,
        F: FnMut(&NormRelayUrl, &Filter) -> bool,
    {
        let cancellations = self
            .subs
            .remove_full_history_fetch_relays_matching(id, matches);
        if cancellations.is_empty() {
            return;
        }

        let mut delta = SessionDelta::default();
        for cancellation in cancellations {
            for relay in &cancellation.relays {
                stage_unsubscribe_task(&mut delta, relay, cancellation.id);
            }
            if cancellation.removed_sub {
                delta.removed_subs.insert(cancellation.id);
            }
        }

        self.ingest_session_delta(delta, wakeup);
    }

    /// Cancel relay-local negentropy work still owned by one tracked sub.
    fn cancel_full_history_owner(&mut self, id: FullHistorySubId) {
        for relay in self.relays.values_mut() {
            relay.cancel_negentropy_owner(id);
        }
    }

    /// Cancel relay-local negentropy work for removed relay/filter pairs.
    fn cancel_full_history_relay_filters(
        &mut self,
        id: FullHistorySubId,
        relay_filters: &[FullHistoryRelayFilter],
    ) {
        let mut by_relay: HashMap<NormRelayUrl, Vec<Filter>> = HashMap::new();
        for relay_filter in relay_filters {
            by_relay
                .entry(relay_filter.relay.clone())
                .or_default()
                .push(relay_filter.filter.clone());
        }

        for (relay_url, filters) in by_relay {
            let Some(relay) = self.relays.get_mut(&relay_url) else {
                continue;
            };
            relay.cancel_negentropy_owner_filters(id, &filters);
        }
    }

    /// Poll the full-history backfill pipeline.
    ///
    /// Call once per frame while the live handler session is still open so any
    /// relay fetches discovered here can be staged into that same session.
    #[profiling::function]
    pub(crate) fn poll_full_history(&mut self, session: &mut OutboxSession) {
        if !self.has_full_history_work() {
            return;
        }

        self.reconcile_and_fetch(session);

        let completed_history_subs = self.full_history.completed_ingestion_subs();
        if completed_history_subs.is_empty() {
            return;
        }

        for history_id in completed_history_subs {
            self.restart_full_history_round(history_id);
        }

        self.reconcile_and_fetch(session);
    }

    /// Run one negentropy reconciliation pass: advance the state machine,
    /// drain discovered needs, and auto-fetch missing events.
    fn reconcile_and_fetch(&mut self, session: &mut OutboxSession) {
        self.poll_negentropy_state_machine();
        let needs = self.drain_full_history_needs();
        for history_id in self.stage_need_fetches(needs, session) {
            self.restart_full_history_round(history_id);
        }
    }

    fn restart_full_history_round(&mut self, history_id: FullHistorySubId) {
        self.cancel_full_history_owner(history_id);
        let Some(tracked) = self.full_history.tracked_subs.get_mut(&history_id) else {
            return;
        };
        tracked.progress.clear_round_work();
        self.full_history.schedule_round(history_id);
    }

    /// Advance pending negentropy local-set builds, expire relay timeouts, and
    /// start any sessions that now have both storage and relay capacity.
    pub(super) fn poll_negentropy_state_machine(&mut self) {
        if !self.has_full_history_work() {
            return;
        }

        let now = Instant::now();
        for relay in self.relays.values_mut() {
            relay.poll_negentropy_timeout(now);
        }
        self.drain_full_history_retries();
        self.full_history.promote_due_retries(now);

        let history_ids: Vec<FullHistorySubId> =
            self.full_history.tracked_subs.keys().copied().collect();
        for history_id in history_ids {
            self.poll_pending_neg_sets_for_sub(history_id);
        }
    }

    /// Advance one tracked full-history sub's pending negentropy builds and
    /// start relay sessions once both storage and relay capacity are available.
    fn poll_pending_neg_sets_for_sub(&mut self, history_id: FullHistorySubId) {
        let Some(tracked) = self.full_history.tracked_subs.get_mut(&history_id) else {
            return;
        };
        if tracked.progress.pending_neg_sets.is_empty() {
            return;
        }

        let progress = &mut tracked.progress;
        let mut i = 0;
        while i < progress.pending_neg_sets.len() {
            if let Some(receiver) = progress.pending_neg_sets[i].receiver.as_mut() {
                match receiver.try_recv() {
                    Ok(storage) => {
                        progress.pending_neg_sets[i].receiver = None;
                        progress.pending_neg_sets[i].storage = Some(storage);
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {
                        i += 1;
                        continue;
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        tracing::warn!(
                            history_id = ?history_id,
                            pending_relay_count = progress.pending_neg_sets[i].relays.len(),
                            "full-history local negentropy set build dropped"
                        );
                        progress.pending_neg_sets.swap_remove(i);
                        continue;
                    }
                }
            }

            if progress.pending_neg_sets[i].storage.is_none() {
                i += 1;
                continue;
            }

            let filter = progress.pending_neg_sets[i].filter.clone();
            let mut remaining_relays = Vec::new();

            for relay_url in std::mem::take(&mut progress.pending_neg_sets[i].relays) {
                let Some(relay) = self.relays.get_mut(&relay_url) else {
                    remaining_relays.push(relay_url.clone());
                    continue;
                };
                if relay.negentropy_data.is_filter_blocked(&filter) {
                    continue;
                }
                if relay.has_active_negentropy_for_full_history(history_id, &filter) {
                    continue;
                }

                match relay.try_initiate_negentropy(
                    || {
                        progress.pending_neg_sets[i]
                            .storage
                            .as_ref()
                            .expect("ready storage")
                            .clone()
                    },
                    filter.clone(),
                    history_id,
                ) {
                    NegentropyStartOutcome::Started => {}
                    NegentropyStartOutcome::Drop => {}
                    NegentropyStartOutcome::Retry => {
                        remaining_relays.push(relay_url.clone());
                    }
                }
            }

            if remaining_relays.is_empty() {
                progress.pending_neg_sets.swap_remove(i);
            } else {
                progress.pending_neg_sets[i].relays = remaining_relays;
                i += 1;
            }
        }
    }

    /// Build internal oneshot fetch sessions for negentropy-discovered missing
    /// events and ingest them into the relay coordinators.
    pub(super) fn stage_need_fetches(
        &mut self,
        needs: Vec<FullHistoryNeed>,
        session: &mut OutboxSession,
    ) -> Vec<FullHistorySubId> {
        self.full_history.queue_needs(needs);
        stage_queued_need_fetches(
            &mut self.full_history,
            &mut self.registry,
            session,
            FULL_HISTORY_PRESENCE_CHECK_BUDGET,
            Instant::now(),
        )
    }
}
