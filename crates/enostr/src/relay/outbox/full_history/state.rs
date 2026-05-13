use hashbrown::{HashMap, HashSet};
use negentropy::NegentropyStorageVector;
use nostrdb::Filter;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

use super::snapshot::{
    full_history_relay_filter_diff, FullHistoryRelayFilter, FullHistorySnapshot, FullHistoryUpsert,
};
use crate::{
    relay::{
        backoff,
        negentropy::{EventChecker, NegSetProvider},
        same_canonical_filter_set, FullHistorySubId, NormRelayUrl,
    },
    NoteId,
};

/// Relay-scoped event id discovered by negentropy reconciliation.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryNeed {
    pub(in crate::relay::outbox) history_id: FullHistorySubId,
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) id: NoteId,
}

/// Queued relay/filter ids waiting for bounded local presence planning.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct QueuedFullHistoryNeeds {
    relay: NormRelayUrl,
    filter: Filter,
    pub(in crate::relay::outbox) ids: VecDeque<NoteId>,
    id_set: HashSet<NoteId>,
}

pub(in crate::relay::outbox) struct FullHistoryNeedBatch {
    pub(in crate::relay::outbox) history_id: FullHistorySubId,
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) ids: Vec<NoteId>,
    pub(in crate::relay::outbox) retries_started: usize,
}

impl QueuedFullHistoryNeeds {
    fn from_need(need: FullHistoryNeed) -> Self {
        let mut ids = VecDeque::new();
        ids.push_back(need.id);
        let mut id_set = HashSet::new();
        id_set.insert(need.id);
        Self {
            relay: need.relay,
            filter: need.filter,
            ids,
            id_set,
        }
    }

    fn matches_relay_filter(&self, relay: &NormRelayUrl, filter: &Filter) -> bool {
        &self.relay == relay && self.filter.same_canonical_attributes(filter)
    }

    fn push_id(&mut self, id: NoteId) {
        if self.id_set.insert(id) {
            self.ids.push_back(id);
        }
    }
}

/// A pending negentropy set waiting for either local-set build completion or a
/// relay/pass becoming available.
pub(in crate::relay::outbox) struct PendingNegSet {
    pub(in crate::relay::outbox) relays: Vec<NormRelayUrl>,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) receiver: Option<oneshot::Receiver<NegentropyStorageVector>>,
    pub(in crate::relay::outbox) storage: Option<NegentropyStorageVector>,
}

impl PendingNegSet {
    /// Start one background local-set build for `filter` and retain all relay
    /// legs that should reuse that result.
    fn new(filter: Filter, relays: Vec<NormRelayUrl>, provider: &dyn NegSetProvider) -> Self {
        let receiver = provider.provide(&filter);
        Self {
            relays,
            filter,
            receiver: Some(receiver),
            storage: None,
        }
    }

    /// Returns the next time the receiver should be polled while a local-set
    /// build is still in flight.
    fn next_poll_deadline(&self, now: Instant) -> Option<Instant> {
        self.receiver
            .is_some()
            .then_some(now + PENDING_NEG_SET_POLL_INTERVAL)
    }
}

pub(in crate::relay::outbox) const MAX_FULL_HISTORY_ROUNDS: usize = 20;
pub(in crate::relay::outbox) const MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER: usize = 3;
pub(in crate::relay::outbox) const MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID: usize = 3;
pub(in crate::relay::outbox) const FULL_HISTORY_RETRY_BACKOFF_BASE: Duration =
    Duration::from_secs(5);
const FULL_HISTORY_RETRY_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
const PENDING_NEG_SET_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(in crate::relay::outbox) const FULL_HISTORY_FETCH_CHUNK: usize = 100;
pub(in crate::relay::outbox) const FULL_HISTORY_PRESENCE_CHECK_BUDGET: usize =
    FULL_HISTORY_FETCH_CHUNK * 4;
/// How long to wait for all fetched events to appear in ndb before treating
/// the stragglers as failed on that relay and moving on.
pub(in crate::relay::outbox) const INGESTION_TIMEOUT: Duration = Duration::from_secs(30);

fn full_history_retry_delay(attempts_started: usize) -> Duration {
    backoff::base_delay_from(
        attempts_started as u32,
        FULL_HISTORY_RETRY_BACKOFF_BASE,
        FULL_HISTORY_RETRY_BACKOFF_MAX,
    )
}

fn next_fetch_retry_at(next_retries_started: usize, now: Instant) -> Instant {
    now + full_history_retry_delay(next_retries_started.saturating_sub(1))
}

/// One in-flight relay-local oneshot fetch for a negentropy-discovered event.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct PendingIngestion {
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) started_at: Instant,
    pub(in crate::relay::outbox) retries_started: usize,
}

impl PendingIngestion {
    /// Returns when this fetch should be treated as timed out.
    fn timeout_deadline(&self) -> Instant {
        self.started_at + INGESTION_TIMEOUT
    }
}

/// Retry policy for one relay-local fetch by id.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryFetchRetryState {
    pub(in crate::relay::outbox) id: NoteId,
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) next_retries_started: usize,
    pub(in crate::relay::outbox) next_retry_at: Instant,
}

impl FullHistoryFetchRetryState {
    fn retry(id: NoteId, pending: PendingIngestion, now: Instant) -> Self {
        let next_retries_started = pending.retries_started + 1;
        Self {
            id,
            relay: pending.relay,
            filter: pending.filter,
            next_retries_started,
            next_retry_at: next_fetch_retry_at(next_retries_started, now),
        }
    }

    fn matches_relay_id(&self, id: &NoteId, relay: &NormRelayUrl) -> bool {
        &self.id == id && &self.relay == relay
    }

    fn belongs_to_snapshot(&self, snapshot: &FullHistorySnapshot) -> bool {
        snapshot.contains_relay_filter(&self.relay, &self.filter)
    }
}

/// Relay/id fetch that exhausted its bounded retry policy for the current round.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryFailedFetch {
    id: NoteId,
    relay: NormRelayUrl,
    filter: Filter,
}

impl FullHistoryFailedFetch {
    fn matches_relay_id(&self, id: &NoteId, relay: &NormRelayUrl) -> bool {
        &self.id == id && &self.relay == relay
    }

    fn belongs_to_snapshot(&self, snapshot: &FullHistorySnapshot) -> bool {
        snapshot.contains_relay_filter(&self.relay, &self.filter)
    }
}

/// Alternate relay that can fetch an id once the active fetch for that id is gone.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryFetchCandidate {
    id: NoteId,
    relay: NormRelayUrl,
    filter: Filter,
    retries_started: usize,
}

impl FullHistoryFetchCandidate {
    fn matches_relay_id(&self, id: &NoteId, relay: &NormRelayUrl) -> bool {
        &self.id == id && &self.relay == relay
    }

    fn belongs_to_snapshot(&self, snapshot: &FullHistorySnapshot) -> bool {
        snapshot.contains_relay_filter(&self.relay, &self.filter)
    }
}

/// Retry policy state for one relay/filter pair in a full-history sub.
pub(in crate::relay::outbox) struct FullHistoryRetryState {
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) attempts_started: usize,
    pub(in crate::relay::outbox) next_retry_at: Option<Instant>,
}

impl FullHistoryRetryState {
    /// Whether this state tracks the same relay and canonical filter.
    fn matches(&self, relay: &NormRelayUrl, filter: &Filter) -> bool {
        &self.relay == relay && self.filter.same_canonical_attributes(filter)
    }

    /// Returns when this retry should be promoted into a new local-set build.
    fn next_deadline(&self) -> Option<Instant> {
        self.next_retry_at
    }
}

/// Per-sub progress state for one full-history reconciliation pipeline.
#[derive(Default)]
pub(in crate::relay::outbox) struct FullHistoryProgress {
    pub(in crate::relay::outbox) pending_neg_sets: Vec<PendingNegSet>,
    pub(in crate::relay::outbox) retry_states: Vec<FullHistoryRetryState>,
    /// Relay-local needs waiting for local presence checks and oneshot planning.
    pub(in crate::relay::outbox) pending_needs: VecDeque<QueuedFullHistoryNeeds>,
    /// IDs currently being fetched plus the relay that was asked to supply
    /// them.
    pub(in crate::relay::outbox) pending_ingestion: HashMap<NoteId, PendingIngestion>,
    /// Timed-out relay-local fetches waiting for bounded retry.
    pub(in crate::relay::outbox) fetch_retry_states: Vec<FullHistoryFetchRetryState>,
    /// Relay/id fetches that exhausted their bounded retry policy this round.
    pub(in crate::relay::outbox) failed_fetches: Vec<FullHistoryFailedFetch>,
    /// Alternate relays waiting behind an active fetch for the same id.
    pub(in crate::relay::outbox) fetch_candidates: Vec<FullHistoryFetchCandidate>,
}

impl FullHistoryProgress {
    /// Whether one tracked sub still has local full-history work in flight.
    fn has_pending_work(&self) -> bool {
        !self.pending_neg_sets.is_empty()
            || !self.pending_needs.is_empty()
            || !self.pending_ingestion.is_empty()
            || self
                .fetch_candidates
                .iter()
                .any(|candidate| !self.pending_ingestion.contains_key(&candidate.id))
            || self
                .fetch_retry_states
                .iter()
                .any(|retry| !self.pending_ingestion.contains_key(&retry.id))
            || self
                .retry_states
                .iter()
                .any(|retry| retry.next_retry_at.is_some())
    }

    /// Enqueue one pending local-set build while preserving a single pending
    /// relay leg per canonical filter.
    fn enqueue_pending_neg_set(
        &mut self,
        filter: Filter,
        relays: Vec<NormRelayUrl>,
        provider: &dyn NegSetProvider,
    ) -> bool {
        let mut new_relays = Vec::new();
        for relay in relays {
            if !new_relays.contains(&relay) {
                new_relays.push(relay);
            }
        }

        let mut matching_pending = None;
        for (index, pending) in self.pending_neg_sets.iter().enumerate() {
            if !pending.filter.same_canonical_attributes(&filter) {
                continue;
            }

            matching_pending.get_or_insert(index);
            new_relays.retain(|relay| !pending.relays.contains(relay));
        }
        if new_relays.is_empty() {
            return false;
        }

        if let Some(index) = matching_pending {
            self.pending_neg_sets[index].relays.extend(new_relays);
            return true;
        }

        self.pending_neg_sets
            .push(PendingNegSet::new(filter, new_relays, provider));

        true
    }

    /// Earliest time-based deadline for this sub's pending full-history work.
    fn next_deadline(
        &self,
        can_retry: bool,
        can_check_ingestion: bool,
        now: Instant,
    ) -> Option<Instant> {
        let local_set_deadline = self
            .pending_neg_sets
            .iter()
            .filter_map(|pending| pending.next_poll_deadline(now))
            .min();
        let retry_deadline = can_retry
            .then(|| {
                self.retry_states
                    .iter()
                    .filter_map(FullHistoryRetryState::next_deadline)
                    .min()
            })
            .flatten();
        let fetch_retry_deadline = can_check_ingestion
            .then(|| {
                self.fetch_retry_states
                    .iter()
                    .filter(|retry| !self.pending_ingestion.contains_key(&retry.id))
                    .map(|retry| retry.next_retry_at)
                    .min()
            })
            .flatten();
        let fetch_candidate_deadline = self
            .fetch_candidates
            .iter()
            .any(|candidate| !self.pending_ingestion.contains_key(&candidate.id))
            .then_some(now);
        let ingestion_deadline = can_check_ingestion
            .then(|| {
                self.pending_ingestion
                    .values()
                    .map(PendingIngestion::timeout_deadline)
                    .min()
            })
            .flatten();
        let needs_deadline = (!self.pending_needs.is_empty()).then_some(now);

        [
            local_set_deadline,
            retry_deadline,
            fetch_retry_deadline,
            fetch_candidate_deadline,
            ingestion_deadline,
            needs_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Drop queued work that no longer belongs to the current relay/filter set.
    fn retain_relay_filters(&mut self, snapshot: &FullHistorySnapshot) {
        self.pending_neg_sets.retain_mut(|pending| {
            pending
                .relays
                .retain(|relay| snapshot.contains_relay_filter(relay, &pending.filter));
            !pending.relays.is_empty()
        });

        self.retry_states
            .retain(|retry| snapshot.contains_relay_filter(&retry.relay, &retry.filter));

        self.pending_needs
            .retain(|needs| snapshot.contains_relay_filter(&needs.relay, &needs.filter));
        self.pending_ingestion
            .retain(|_, pending| snapshot.contains_relay_filter(&pending.relay, &pending.filter));
        self.fetch_retry_states
            .retain(|retry| retry.belongs_to_snapshot(snapshot));
        self.failed_fetches
            .retain(|failed| failed.belongs_to_snapshot(snapshot));
        self.fetch_candidates
            .retain(|candidate| candidate.belongs_to_snapshot(snapshot));
    }

    /// Schedule a delayed retry for one relay/filter pair, subject to this
    /// snapshot's retry budget.
    fn schedule_retry(&mut self, relay: NormRelayUrl, filter: Filter, now: Instant) {
        if let Some(retry) = self
            .retry_states
            .iter_mut()
            .find(|retry| retry.matches(&relay, &filter))
        {
            if retry.attempts_started >= MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER {
                return;
            }
            if retry.next_retry_at.is_none() {
                retry.next_retry_at = Some(now + full_history_retry_delay(retry.attempts_started));
            }
            return;
        }

        self.retry_states.push(FullHistoryRetryState {
            relay,
            filter,
            attempts_started: 0,
            next_retry_at: Some(now + full_history_retry_delay(0)),
        });
    }

    /// Start local-set builds for retry entries whose backoff has elapsed.
    fn promote_due_retries(&mut self, provider: &dyn NegSetProvider, now: Instant) {
        let mut exhausted_retries = Vec::new();
        let mut due_retries = Vec::new();
        for (index, retry) in self.retry_states.iter().enumerate() {
            let Some(next_retry_at) = retry.next_retry_at else {
                continue;
            };
            if next_retry_at > now {
                continue;
            }
            if retry.attempts_started >= MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER {
                exhausted_retries.push(index);
                continue;
            }

            due_retries.push((index, retry.relay.clone(), retry.filter.clone()));
        }

        for index in exhausted_retries {
            self.retry_states[index].next_retry_at = None;
        }
        for (index, relay, filter) in due_retries {
            if self.enqueue_pending_neg_set(filter, vec![relay], provider) {
                self.retry_states[index].attempts_started += 1;
            }
            self.retry_states[index].next_retry_at = None;
        }
    }

    /// Returns true once this sub has no in-flight fetches left to wait on.
    ///
    /// Successful local ingestion clears the pending entry. Timed-out entries
    /// are remembered only for the relay that failed to supply them so later
    /// rounds can still try other relays for the same event id.
    fn ingestion_complete(&mut self, checker: &dyn EventChecker) -> bool {
        if self.pending_ingestion.is_empty() {
            return false;
        }

        let mut missing_ids: HashSet<NoteId> = self.pending_ingestion.keys().copied().collect();
        checker.retain_missing(&mut missing_ids);

        let present_ids: Vec<NoteId> = self
            .pending_ingestion
            .keys()
            .copied()
            .filter(|id| !missing_ids.contains(id))
            .collect();
        if !present_ids.is_empty() {
            self.pending_ingestion
                .retain(|id, _| missing_ids.contains(id));
            for id in present_ids {
                self.clear_fetch_state(&id);
            }
            if self.pending_ingestion.is_empty() {
                return self.pending_needs.is_empty();
            }
        }

        let now = Instant::now();
        let timed_out: Vec<(NoteId, PendingIngestion)> = self
            .pending_ingestion
            .iter()
            .filter(|(_, pending)| now.duration_since(pending.started_at) >= INGESTION_TIMEOUT)
            .map(|(id, pending)| (*id, pending.clone()))
            .collect();
        if timed_out.is_empty() {
            return false;
        }

        for (id, pending) in timed_out {
            self.pending_ingestion.remove(&id);
            self.schedule_fetch_retry(id, pending, now);
        }

        false
    }

    /// Clear work that was based on the previous local negentropy set before
    /// scheduling a fresh verification round.
    pub(in crate::relay::outbox) fn clear_round_work(&mut self) {
        self.pending_neg_sets.clear();
        self.retry_states.clear();
        self.pending_needs.clear();
        self.pending_ingestion.clear();
        self.fetch_retry_states.clear();
        self.failed_fetches.clear();
        self.fetch_candidates.clear();
    }

    /// Whether retry state already owns the next fetch for this relay/id.
    pub(in crate::relay::outbox) fn fetch_retry_waiting(
        &self,
        id: &NoteId,
        relay: &NormRelayUrl,
    ) -> bool {
        self.fetch_retry_states
            .iter()
            .any(|retry| retry.matches_relay_id(id, relay))
    }

    /// Whether this relay/id exhausted its fetch retry policy this round.
    pub(in crate::relay::outbox) fn fetch_failed(&self, id: &NoteId, relay: &NormRelayUrl) -> bool {
        self.failed_fetches
            .iter()
            .any(|failed| failed.matches_relay_id(id, relay))
    }

    /// Whether an alternate relay is waiting for this id.
    pub(in crate::relay::outbox) fn fetch_candidate_waiting(
        &self,
        id: &NoteId,
        relay: &NormRelayUrl,
    ) -> bool {
        self.fetch_candidates
            .iter()
            .any(|candidate| candidate.matches_relay_id(id, relay))
    }

    pub(in crate::relay::outbox) fn fetch_state_suppresses_need(
        &self,
        id: &NoteId,
        relay: &NormRelayUrl,
    ) -> bool {
        self.fetch_retry_waiting(id, relay)
            || self.fetch_candidate_waiting(id, relay)
            || self.fetch_failed(id, relay)
    }

    /// Clear fetch state for an id that is now locally present.
    pub(in crate::relay::outbox) fn clear_fetch_state(&mut self, id: &NoteId) {
        self.fetch_retry_states.retain(|retry| &retry.id != id);
        self.failed_fetches.retain(|failed| &failed.id != id);
        self.fetch_candidates
            .retain(|candidate| &candidate.id != id);
    }

    fn schedule_fetch_retry(&mut self, id: NoteId, pending: PendingIngestion, now: Instant) {
        let next_retries_started = pending.retries_started + 1;
        if next_retries_started > MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID {
            self.record_failed_fetch(id, pending.relay, pending.filter);
            return;
        }

        if let Some(retry) = self
            .fetch_retry_states
            .iter_mut()
            .find(|retry| retry.matches_relay_id(&id, &pending.relay))
        {
            retry.filter = pending.filter;
            retry.next_retries_started = retry.next_retries_started.max(next_retries_started);
            retry.next_retry_at = next_fetch_retry_at(retry.next_retries_started, now);
            return;
        }

        self.fetch_retry_states
            .push(FullHistoryFetchRetryState::retry(id, pending, now));
    }

    fn record_failed_fetch(&mut self, id: NoteId, relay: NormRelayUrl, filter: Filter) {
        self.fetch_retry_states
            .retain(|retry| !retry.matches_relay_id(&id, &relay));
        if let Some(failed) = self
            .failed_fetches
            .iter_mut()
            .find(|failed| failed.matches_relay_id(&id, &relay))
        {
            failed.filter = filter;
            return;
        }

        self.failed_fetches
            .push(FullHistoryFailedFetch { id, relay, filter });
    }

    /// Remember another relay that can be tried if the active fetch for `id`
    /// times out.
    pub(in crate::relay::outbox) fn queue_fetch_candidate(
        &mut self,
        id: NoteId,
        relay: NormRelayUrl,
        filter: Filter,
        retries_started: usize,
    ) {
        if self
            .pending_ingestion
            .get(&id)
            .is_some_and(|pending| pending.relay == relay)
        {
            return;
        }
        if self.fetch_retry_waiting(&id, &relay) {
            return;
        }
        if let Some(candidate) = self
            .fetch_candidates
            .iter_mut()
            .find(|candidate| candidate.matches_relay_id(&id, &relay))
        {
            candidate.filter = filter;
            candidate.retries_started = candidate.retries_started.max(retries_started);
            return;
        }

        self.fetch_candidates.push(FullHistoryFetchCandidate {
            id,
            relay,
            filter,
            retries_started,
        });
    }

    /// Queue one surfaced need for bounded local presence planning.
    fn queue_need(&mut self, need: FullHistoryNeed) {
        if self.pending_ingestion.contains_key(&need.id) {
            self.queue_fetch_candidate(need.id, need.relay, need.filter, 0);
            return;
        }
        if let Some(pending) = self
            .pending_needs
            .iter_mut()
            .find(|pending| pending.matches_relay_filter(&need.relay, &need.filter))
        {
            pending.push_id(need.id);
            return;
        }

        self.pending_needs
            .push_back(QueuedFullHistoryNeeds::from_need(need));
    }

    /// Take up to `limit` queued ids for this sub, preserving relay/filter grouping.
    fn take_queued_need_batches(
        &mut self,
        history_id: FullHistorySubId,
        limit: usize,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        let mut remaining = limit;
        let mut index = 0;

        while remaining > 0 && index < self.pending_needs.len() {
            let mut ids = Vec::new();
            let mut fetch_candidates = Vec::new();
            let relay;
            let filter;
            {
                let pending = &mut self.pending_needs[index];
                relay = pending.relay.clone();
                filter = pending.filter.clone();
                while ids.len() < remaining {
                    let Some(id) = pending.ids.pop_front() else {
                        break;
                    };
                    pending.id_set.remove(&id);
                    if self.pending_ingestion.contains_key(&id) {
                        fetch_candidates.push(id);
                        continue;
                    }
                    ids.push(id);
                }
            }
            for id in fetch_candidates {
                self.queue_fetch_candidate(id, relay.clone(), filter.clone(), 0);
            }

            let pending = &self.pending_needs[index];
            if !ids.is_empty() {
                remaining -= ids.len();
                batches.push(FullHistoryNeedBatch {
                    history_id,
                    relay: pending.relay.clone(),
                    filter: pending.filter.clone(),
                    ids,
                    retries_started: 0,
                });
            }

            if self.pending_needs[index].ids.is_empty() {
                self.pending_needs.remove(index);
                continue;
            }
            index += 1;
        }

        batches
    }

    /// Take alternate relay fetches whose active id fetch is no longer present.
    fn take_fetch_candidate_batches(
        &mut self,
        history_id: FullHistorySubId,
        limit: usize,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        let mut remaining = limit;
        let mut index = 0;

        while remaining > 0 && index < self.fetch_candidates.len() {
            if self
                .pending_ingestion
                .contains_key(&self.fetch_candidates[index].id)
            {
                index += 1;
                continue;
            }

            let candidate = self.fetch_candidates.swap_remove(index);
            push_need_batch(
                &mut batches,
                FullHistoryNeedBatch {
                    history_id,
                    relay: candidate.relay,
                    filter: candidate.filter,
                    ids: vec![candidate.id],
                    retries_started: candidate.retries_started,
                },
            );
            remaining -= 1;
        }

        batches
    }

    /// Take due relay-local fetch retries, preserving relay/filter grouping.
    fn take_due_fetch_retry_batches(
        &mut self,
        history_id: FullHistorySubId,
        limit: usize,
        now: Instant,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        let mut remaining = limit;
        let mut index = 0;

        while remaining > 0 && index < self.fetch_retry_states.len() {
            if self
                .pending_ingestion
                .contains_key(&self.fetch_retry_states[index].id)
            {
                index += 1;
                continue;
            }

            let next_retry_at = self.fetch_retry_states[index].next_retry_at;
            if next_retry_at > now {
                index += 1;
                continue;
            }

            let retry = self.fetch_retry_states.swap_remove(index);
            if retry.next_retries_started > MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID {
                self.record_failed_fetch(retry.id, retry.relay, retry.filter);
                continue;
            }

            push_need_batch(
                &mut batches,
                FullHistoryNeedBatch {
                    history_id,
                    relay: retry.relay,
                    filter: retry.filter,
                    ids: vec![retry.id],
                    retries_started: retry.next_retries_started,
                },
            );
            remaining -= 1;
        }

        batches
    }
}

fn push_need_batch(batches: &mut Vec<FullHistoryNeedBatch>, batch: FullHistoryNeedBatch) {
    if let Some(existing) = batches.iter_mut().find(|existing| {
        existing.retries_started == batch.retries_started
            && existing.relay == batch.relay
            && existing.filter.same_canonical_attributes(&batch.filter)
    }) {
        existing.ids.extend(batch.ids);
        return;
    }

    batches.push(batch);
}

/// Stable snapshot plus owned progress state for one full-history sub.
pub(in crate::relay::outbox) struct TrackedFullHistorySub {
    pub(in crate::relay::outbox) snapshot: FullHistorySnapshot,
    pub(in crate::relay::outbox) rounds_started: usize,
    pub(in crate::relay::outbox) progress: FullHistoryProgress,
}

impl TrackedFullHistorySub {
    /// Build a fresh tracked sub state from one current snapshot.
    fn new(snapshot: FullHistorySnapshot) -> Self {
        Self {
            snapshot,
            rounds_started: 0,
            progress: FullHistoryProgress::default(),
        }
    }

    /// Replace the snapshot and drop progress for removed relay/filter pairs.
    fn replace_snapshot(&mut self, snapshot: FullHistorySnapshot, reset_rounds: bool) {
        self.snapshot = snapshot;
        if reset_rounds {
            self.rounds_started = 0;
        }
        self.progress.retain_relay_filters(&self.snapshot);
    }

    /// Schedule one bounded negentropy round for this tracked sub.
    fn schedule_round(&mut self, provider: &dyn NegSetProvider) {
        if self.rounds_started >= MAX_FULL_HISTORY_ROUNDS {
            return;
        }

        if !self.enqueue_round(provider) {
            return;
        }

        self.rounds_started += 1;
    }

    /// Schedule a delayed retry for one relay/filter pair from this snapshot.
    pub(in crate::relay::outbox) fn schedule_retry(
        &mut self,
        relay: NormRelayUrl,
        filter: Filter,
        now: Instant,
    ) {
        if !self
            .snapshot
            .relays
            .iter()
            .any(|snapshot_relay| snapshot_relay == &relay)
        {
            return;
        }
        if !self
            .snapshot
            .filters
            .iter()
            .any(|snapshot_filter| snapshot_filter.same_canonical_attributes(&filter))
        {
            return;
        }

        self.progress.schedule_retry(relay, filter, now);
    }

    /// Start any retry local-set builds whose backoff has elapsed.
    fn promote_due_retries(&mut self, provider: &dyn NegSetProvider, now: Instant) {
        self.progress.promote_due_retries(provider, now);
    }

    fn enqueue_round(&mut self, provider: &dyn NegSetProvider) -> bool {
        let mut queued = false;
        for filter in &self.snapshot.filters {
            queued |= self.progress.enqueue_pending_neg_set(
                filter.clone(),
                self.snapshot.relays.clone(),
                provider,
            );
        }

        queued
    }

    /// Schedule local-set builds for newly added relay/filter pairs.
    fn schedule_relay_filters(
        &mut self,
        relay_filters: Vec<FullHistoryRelayFilter>,
        provider: &dyn NegSetProvider,
    ) {
        if relay_filters.is_empty() {
            return;
        }

        let mut grouped: Vec<(Filter, Vec<NormRelayUrl>)> = Vec::new();
        for relay_filter in relay_filters {
            if let Some((_, relays)) = grouped
                .iter_mut()
                .find(|(filter, _)| filter.same_canonical_attributes(&relay_filter.filter))
            {
                relays.push(relay_filter.relay);
                continue;
            }
            grouped.push((relay_filter.filter, vec![relay_filter.relay]));
        }

        for (filter, relays) in grouped {
            self.progress
                .enqueue_pending_neg_set(filter, relays, provider);
        }
    }

    /// Whether this tracked full-history sub still needs upkeep work.
    fn has_pending_work(&self) -> bool {
        self.progress.has_pending_work()
    }

    /// Earliest time-based deadline for this tracked sub.
    fn next_deadline(
        &self,
        can_retry: bool,
        can_check_ingestion: bool,
        now: Instant,
    ) -> Option<Instant> {
        self.progress
            .next_deadline(can_retry, can_check_ingestion, now)
    }
}

/// Internal state tracking per-sub full-history reconciliation pipelines.
#[derive(Default)]
pub(in crate::relay::outbox) struct FullHistoryTracker {
    pub(in crate::relay::outbox) tracked_subs: HashMap<FullHistorySubId, TrackedFullHistorySub>,
    pub(in crate::relay::outbox) neg_set_provider: Option<Box<dyn NegSetProvider>>,
    pub(in crate::relay::outbox) event_checker: Option<Box<dyn EventChecker>>,
}

impl FullHistoryTracker {
    /// Upsert one full-history sub snapshot and report relay/filter changes.
    pub(in crate::relay::outbox) fn upsert(
        &mut self,
        snapshot: FullHistorySnapshot,
    ) -> FullHistoryUpsert {
        let id = snapshot.id;
        match self.tracked_subs.get_mut(&id) {
            Some(tracked) => {
                if tracked.snapshot.semantically_matches(&snapshot) {
                    FullHistoryUpsert::Unchanged
                } else {
                    let previous_relays = tracked.snapshot.relay_filters();
                    let next_relays = snapshot.relay_filters();
                    let added = full_history_relay_filter_diff(&next_relays, &previous_relays);
                    let removed = full_history_relay_filter_diff(&previous_relays, &next_relays);
                    let filters_changed =
                        !same_canonical_filter_set(&tracked.snapshot.filters, &snapshot.filters);
                    tracked.replace_snapshot(snapshot, filters_changed);
                    FullHistoryUpsert::Changed {
                        added,
                        removed,
                        filters_changed,
                    }
                }
            }
            None => {
                self.tracked_subs
                    .insert(id, TrackedFullHistorySub::new(snapshot));
                FullHistoryUpsert::Inserted
            }
        }
    }

    /// Drop one tracked full-history sub and all owned progress state tied to it.
    pub(in crate::relay::outbox) fn remove(&mut self, id: FullHistorySubId) {
        self.tracked_subs.remove(&id);
    }

    /// Schedule one bounded negentropy round for a tracked sub.
    pub(in crate::relay::outbox) fn schedule_round(&mut self, id: FullHistorySubId) {
        let Some(provider) = self.neg_set_provider.as_deref() else {
            return;
        };
        if let Some(tracked) = self.tracked_subs.get_mut(&id) {
            tracked.schedule_round(provider);
        }
    }

    /// Schedule local-set builds for newly added relay/filter pairs.
    pub(in crate::relay::outbox) fn schedule_relay_filters(
        &mut self,
        id: FullHistorySubId,
        relay_filters: Vec<FullHistoryRelayFilter>,
    ) {
        let Some(provider) = self.neg_set_provider.as_deref() else {
            return;
        };
        if let Some(tracked) = self.tracked_subs.get_mut(&id) {
            tracked.schedule_relay_filters(relay_filters, provider);
        }
    }

    /// Start due retry work for all tracked subs.
    pub(in crate::relay::outbox) fn promote_due_retries(&mut self, now: Instant) {
        let Some(provider) = self.neg_set_provider.as_deref() else {
            return;
        };
        for tracked in self.tracked_subs.values_mut() {
            tracked.promote_due_retries(provider, now);
        }
    }

    /// Queue relay-surfaced needs under the owning tracked sub.
    pub(in crate::relay::outbox) fn queue_needs(&mut self, needs: Vec<FullHistoryNeed>) {
        for need in needs {
            let Some(tracked) = self.tracked_subs.get_mut(&need.history_id) else {
                continue;
            };
            if !tracked
                .snapshot
                .contains_relay_filter(&need.relay, &need.filter)
            {
                continue;
            }
            tracked.progress.queue_need(need);
        }
    }

    /// Take a bounded batch of relay-local fetch ids across tracked subs.
    pub(in crate::relay::outbox) fn take_need_batches(
        &mut self,
        limit: usize,
        now: Instant,
    ) -> Vec<FullHistoryNeedBatch> {
        if limit == 0 {
            return Vec::new();
        }

        let pending_subs = self
            .tracked_subs
            .values()
            .filter(|tracked| {
                !tracked.progress.pending_needs.is_empty()
                    || tracked.progress.fetch_candidates.iter().any(|candidate| {
                        !tracked
                            .progress
                            .pending_ingestion
                            .contains_key(&candidate.id)
                    })
                    || tracked.progress.fetch_retry_states.iter().any(|retry| {
                        retry.next_retry_at <= now
                            && !tracked.progress.pending_ingestion.contains_key(&retry.id)
                    })
            })
            .count();
        if pending_subs == 0 {
            return Vec::new();
        }
        let per_sub_limit = (limit / pending_subs).max(1);

        let mut batches = Vec::new();
        let mut remaining = limit;
        for (&history_id, tracked) in &mut self.tracked_subs {
            if remaining == 0 {
                break;
            }
            let sub_limit = remaining.min(per_sub_limit);
            let mut taken = tracked
                .progress
                .take_fetch_candidate_batches(history_id, sub_limit);
            let mut taken_count = taken.iter().map(|batch| batch.ids.len()).sum::<usize>();
            let remaining_for_sub = sub_limit.saturating_sub(taken_count);
            if remaining_for_sub > 0 {
                let mut queued = tracked
                    .progress
                    .take_queued_need_batches(history_id, remaining_for_sub);
                taken_count += queued.iter().map(|batch| batch.ids.len()).sum::<usize>();
                taken.append(&mut queued);
            }
            let remaining_for_sub = sub_limit.saturating_sub(taken_count);
            if remaining_for_sub > 0 {
                taken.extend(tracked.progress.take_due_fetch_retry_batches(
                    history_id,
                    remaining_for_sub,
                    now,
                ));
            }
            remaining =
                remaining.saturating_sub(taken.iter().map(|batch| batch.ids.len()).sum::<usize>());
            batches.extend(taken);
        }
        batches
    }

    /// Earliest time-based deadline among tracked full-history subs.
    pub(in crate::relay::outbox) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let can_retry = self.neg_set_provider.is_some();
        let can_check_ingestion = self.event_checker.is_some();

        self.tracked_subs
            .values()
            .filter_map(|tracked| tracked.next_deadline(can_retry, can_check_ingestion, now))
            .min()
    }

    /// Return the tracked full-history ids whose current fetch batch is complete.
    pub(in crate::relay::outbox) fn completed_ingestion_subs(&mut self) -> Vec<FullHistorySubId> {
        let Some(checker) = self.event_checker.as_deref() else {
            return Vec::new();
        };

        let completed: Vec<FullHistorySubId> = self
            .tracked_subs
            .iter_mut()
            .filter_map(|(&history_id, tracked)| {
                tracked
                    .progress
                    .ingestion_complete(checker)
                    .then_some(history_id)
            })
            .collect();
        completed
    }

    /// Whether any tracked sub still has local full-history work in flight.
    pub(in crate::relay::outbox) fn has_pending_work(&self) -> bool {
        self.tracked_subs
            .values()
            .any(TrackedFullHistorySub::has_pending_work)
    }
}
