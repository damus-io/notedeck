use hashbrown::{HashMap, HashSet};
use negentropy::NegentropyStorageVector;
use nostrdb::Filter;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use super::super::RelayTransportDemand;
use super::snapshot::{full_history_relay_filter_diff, FullHistorySnapshot, FullHistoryUpsert};
use crate::{
    relay::{
        backoff, same_canonical_filter_set, subscription::SubscribeTask, FullHistoryRelayFilter,
        FullHistorySubId, NormRelayUrl, RelayConnectionPriority, RelayUrlPolicy, RelayUrlSource,
    },
    NoteId,
};

/// Relay-scoped event id discovered by negentropy reconciliation.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryNeed {
    pub(in crate::relay::outbox) history_id: FullHistorySubId,
    pub(in crate::relay::outbox) target: FullHistoryRelayFilter,
    pub(in crate::relay::outbox) id: NoteId,
}

/// Queued relay/filter ids waiting for bounded local presence planning.
#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct QueuedFullHistoryNeeds {
    target: FullHistoryRelayFilter,
    pub(in crate::relay::outbox) ids: VecDeque<NoteId>,
    id_set: HashSet<NoteId>,
}

#[derive(Clone, Debug)]
pub(in crate::relay::outbox) struct FullHistoryNeedBatch {
    pub(in crate::relay::outbox) history_id: FullHistorySubId,
    pub(in crate::relay::outbox) target: FullHistoryRelayFilter,
    pub(in crate::relay::outbox) ids: Vec<NoteId>,
    pub(in crate::relay::outbox) retries_started: usize,
}

/// Backend-owned local negentropy-set request emitted by full-history.
#[derive(Debug)]
pub struct FullHistoryLocalSetRequest {
    pub history_id: FullHistorySubId,
    pub request_id: u64,
    pub filter: Filter,
}

/// Backend-owned storage-presence request for relay-discovered event ids.
#[derive(Debug)]
pub struct FullHistoryLocalPresenceRequest {
    pub request_id: u64,
    pub candidate_ids: HashSet<NoteId>,
}

/// Backend storage-presence result for relay-discovered event ids.
#[derive(Debug)]
pub struct FullHistoryLocalPresenceResult {
    pub request_id: u64,
    pub missing_ids: HashSet<NoteId>,
    pub already_local_ids: HashSet<NoteId>,
}

/// Backend-owned storage-presence request for ids already queued for relay
/// fetch ingestion.
#[derive(Debug)]
pub struct FullHistoryPendingIngestionPresenceRequest {
    pub candidate_ids: HashSet<NoteId>,
    pub deadline: Instant,
}

/// Backend storage-presence result for ids already queued for relay fetch
/// ingestion.
#[derive(Debug)]
pub struct FullHistoryPendingIngestionPresenceResult {
    pub stored_ids: HashSet<NoteId>,
}

/// Relay fetch request emitted by full-history before the service allocates a
/// concrete outbox subscription id.
pub(in crate::relay::outbox) struct FullHistoryFetchRequest {
    pub(in crate::relay::outbox) owner: FullHistorySubId,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) subscribe: SubscribeTask,
}

/// Exact work emitted by one full-history runtime transition.
#[derive(Default)]
pub(in crate::relay::outbox) struct FullHistoryOutput {
    pub(in crate::relay::outbox) local_set_requests: Vec<FullHistoryLocalSetRequest>,
    pub(in crate::relay::outbox) local_presence_requests: Vec<FullHistoryLocalPresenceRequest>,
    pub(in crate::relay::outbox) pending_ingestion_presence_requests:
        Vec<FullHistoryPendingIngestionPresenceRequest>,
    pub(in crate::relay::outbox) fetch_requests: Vec<FullHistoryFetchRequest>,
    pub(in crate::relay::outbox) relay_demand_changes:
        HashMap<NormRelayUrl, Option<RelayTransportDemand>>,
}

impl FullHistoryOutput {
    pub(in crate::relay::outbox) fn extend(&mut self, next: FullHistoryOutput) {
        self.local_set_requests.extend(next.local_set_requests);
        self.local_presence_requests
            .extend(next.local_presence_requests);
        self.pending_ingestion_presence_requests
            .extend(next.pending_ingestion_presence_requests);
        self.fetch_requests.extend(next.fetch_requests);
        self.relay_demand_changes.extend(next.relay_demand_changes);
    }
}

/// One relay-local negentropy session that has a completed local set and is
/// ready to be attempted on a coordinator.
pub(in crate::relay::outbox) struct FullHistoryNegentropyStart<'a> {
    pub(in crate::relay::outbox) history_id: FullHistorySubId,
    pub(in crate::relay::outbox) relay: NormRelayUrl,
    pub(in crate::relay::outbox) filter: &'a Filter,
    pub(in crate::relay::outbox) relay_policy: RelayUrlPolicy,
    pub(in crate::relay::outbox) storage: &'a NegentropyStorageVector,
}

/// Result of trying to materialize one ready full-history negentropy leg.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::relay::outbox) enum FullHistoryNegentropyStartOutcome {
    Started,
    Drop,
    Retry,
}

impl QueuedFullHistoryNeeds {
    fn from_need(need: FullHistoryNeed) -> Self {
        let mut ids = VecDeque::new();
        ids.push_back(need.id);
        let mut id_set = HashSet::new();
        id_set.insert(need.id);
        Self {
            target: need.target,
            ids,
            id_set,
        }
    }

    fn matches_relay_filter(&self, target: &FullHistoryRelayFilter) -> bool {
        self.target.semantically_matches(target)
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
    pub(in crate::relay::outbox) request_id: u64,
    pub(in crate::relay::outbox) relays: Vec<NormRelayUrl>,
    /// Relay package policy keyed by relay for this pending local-set build.
    pub(in crate::relay::outbox) relay_policy_by_relay: HashMap<NormRelayUrl, RelayUrlPolicy>,
    pub(in crate::relay::outbox) filter: Filter,
    pub(in crate::relay::outbox) storage: Option<NegentropyStorageVector>,
}

impl PendingNegSet {
    /// Start one background local-set build for `filter` and retain all relay
    /// legs that should reuse that result.
    fn new(request_id: u64, filter: Filter, relay_filters: Vec<FullHistoryRelayFilter>) -> Self {
        let mut pending = Self {
            request_id,
            relays: Vec::new(),
            relay_policy_by_relay: HashMap::new(),
            filter,
            storage: None,
        };
        pending.add_relays(relay_filters);
        pending
    }

    /// Add relay legs while preserving each leg's relay package policy.
    fn add_relays(&mut self, relay_filters: Vec<FullHistoryRelayFilter>) {
        for relay_filter in relay_filters {
            if !self.relays.contains(&relay_filter.relay) {
                self.relays.push(relay_filter.relay.clone());
            }
            self.relay_policy_by_relay
                .insert(relay_filter.relay, relay_filter.relay_policy);
        }
    }

    /// Return the relay package policy for a retained relay leg.
    pub(super) fn relay_policy_for_relay(&self, relay: &NormRelayUrl) -> Option<RelayUrlPolicy> {
        self.relay_policy_by_relay.get(relay).copied()
    }

    /// Return the relay/filter targets represented by this pending local-set build.
    pub(super) fn relay_filters(&self) -> Vec<FullHistoryRelayFilter> {
        self.relays
            .iter()
            .filter_map(|relay| {
                Some(FullHistoryRelayFilter {
                    relay: relay.clone(),
                    relay_policy: self.relay_policy_for_relay(relay)?,
                    filter: self.filter.clone(),
                })
            })
            .collect()
    }

    /// Keep only relay legs still present in `snapshot`, refreshing copied
    /// relay package policy from the current snapshot.
    fn retain_relay_filters(&mut self, snapshot: &FullHistorySnapshot) -> bool {
        let mut retained = Vec::new();
        for relay in &self.relays {
            let Some(target) = snapshot.target_for_relay_filter(relay, &self.filter) else {
                continue;
            };
            self.relay_policy_by_relay
                .insert(relay.clone(), target.relay_policy);
            retained.push(relay.clone());
        }
        self.relays = retained;
        self.relay_policy_by_relay
            .retain(|relay, _| self.relays.contains(relay));
        !self.relays.is_empty()
    }
}

pub(in crate::relay::outbox) const MAX_FULL_HISTORY_ROUNDS: usize = 20;
pub(in crate::relay::outbox) const MAX_FULL_HISTORY_RETRIES_PER_RELAY_FILTER: usize = 3;
pub(in crate::relay::outbox) const MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID: usize = 3;
pub(in crate::relay::outbox) const FULL_HISTORY_RETRY_BACKOFF_BASE: Duration =
    Duration::from_secs(5);
const FULL_HISTORY_RETRY_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
pub(in crate::relay::outbox) const FULL_HISTORY_FETCH_CHUNK: usize = 100;
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
    pub(in crate::relay::outbox) target: FullHistoryRelayFilter,
    pub(in crate::relay::outbox) started_at: Instant,
    pub(in crate::relay::outbox) retries_started: usize,
}

impl PendingIngestion {
    /// Returns when this fetch should be treated as timed out.
    pub(in crate::relay::outbox) fn timeout_deadline(&self) -> Instant {
        self.started_at + INGESTION_TIMEOUT
    }
}

/// Retry policy state for one relay-local fetch by id.
#[derive(Clone, Debug)]
struct FullHistoryFetchRetryState {
    target: FullHistoryRelayFilter,
    next_retries_started: usize,
    next_retry_at: Instant,
}

/// Relay/id fetch that exhausted its bounded retry policy for the current round.
#[derive(Clone, Debug)]
struct FullHistoryFailedFetch {
    target: FullHistoryRelayFilter,
}

/// Alternate relay that can fetch an id once the active fetch for that id is gone.
#[derive(Clone, Debug)]
struct FullHistoryFetchCandidate {
    target: FullHistoryRelayFilter,
    retries_started: usize,
}

#[derive(Clone, Debug)]
enum FullHistoryRelayFetchState {
    Candidate(FullHistoryFetchCandidate),
    Retry(FullHistoryFetchRetryState),
    Failed(FullHistoryFailedFetch),
}

impl FullHistoryRelayFetchState {
    fn target(&self) -> &FullHistoryRelayFilter {
        match self {
            Self::Candidate(candidate) => &candidate.target,
            Self::Retry(retry) => &retry.target,
            Self::Failed(failed) => &failed.target,
        }
    }

    fn target_mut(&mut self) -> &mut FullHistoryRelayFilter {
        match self {
            Self::Candidate(candidate) => &mut candidate.target,
            Self::Retry(retry) => &mut retry.target,
            Self::Failed(failed) => &mut failed.target,
        }
    }

    fn refresh_target_policy(&mut self, snapshot: &FullHistorySnapshot) -> bool {
        refresh_target_policy(self.target_mut(), snapshot)
    }

    fn has_pending_work(&self) -> bool {
        matches!(self, Self::Candidate(_) | Self::Retry(_))
    }
}

#[derive(Clone, Debug)]
struct FullHistoryRelayFetch {
    id: NoteId,
    state: FullHistoryRelayFetchState,
}

impl FullHistoryRelayFetch {
    fn matches_relay_id(&self, id: &NoteId, relay: &NormRelayUrl) -> bool {
        &self.id == id && &self.state.target().relay == relay
    }
}

/// Active ingestion plus relay-local follow-up fetch state for negentropy
/// missing ids.
#[derive(Clone, Debug, Default)]
struct FullHistoryFetches {
    active: HashMap<NoteId, PendingIngestion>,
    relay_states: Vec<FullHistoryRelayFetch>,
}

impl FullHistoryFetches {
    #[cfg(test)]
    fn has_pending_work(&self) -> bool {
        !self.active.is_empty()
            || self
                .relay_states
                .iter()
                .any(|fetch| fetch.state.has_pending_work())
    }

    fn has_pending_work_for_snapshot(&self, snapshot: &FullHistorySnapshot) -> bool {
        self.active
            .values()
            .any(|pending| snapshot.contains_relay_filter_target(&pending.target))
            || self.relay_states.iter().any(|fetch| {
                fetch.state.has_pending_work()
                    && snapshot.contains_relay_filter_target(fetch.state.target())
            })
    }

    fn retain_relay_filters(&mut self, snapshot: &FullHistorySnapshot) {
        self.active
            .retain(|_, pending| refresh_target_policy(&mut pending.target, snapshot));
        self.relay_states
            .retain_mut(|fetch| fetch.state.refresh_target_policy(snapshot));
    }

    fn for_each_transport_demand(&self, mut visit: impl FnMut(&FullHistoryRelayFilter)) {
        for fetch in &self.relay_states {
            if self.active.contains_key(&fetch.id) {
                continue;
            }
            match &fetch.state {
                FullHistoryRelayFetchState::Candidate(candidate) => visit(&candidate.target),
                FullHistoryRelayFetchState::Retry(retry) => visit(&retry.target),
                FullHistoryRelayFetchState::Failed(_) => {}
            }
        }
    }

    fn next_retry_deadline(&self) -> Option<Instant> {
        self.relay_states
            .iter()
            .filter(|fetch| !self.active.contains_key(&fetch.id))
            .filter_map(|fetch| match &fetch.state {
                FullHistoryRelayFetchState::Retry(retry) => Some(retry.next_retry_at),
                FullHistoryRelayFetchState::Candidate(_)
                | FullHistoryRelayFetchState::Failed(_) => None,
            })
            .min()
    }

    fn has_ready_candidate(&self) -> bool {
        self.relay_states.iter().any(|fetch| {
            !self.active.contains_key(&fetch.id)
                && matches!(fetch.state, FullHistoryRelayFetchState::Candidate(_))
        })
    }

    fn ingestion_deadline(&self) -> Option<Instant> {
        self.active
            .values()
            .map(PendingIngestion::timeout_deadline)
            .min()
    }

    fn start_pending_ingestion(&mut self, id: NoteId, pending: PendingIngestion) {
        let relay = pending.target.relay.clone();
        self.relay_states
            .retain(|fetch| !fetch.matches_relay_id(&id, &relay));
        self.active.insert(id, pending);
    }

    fn pending_ingestion(&self, id: &NoteId) -> Option<&PendingIngestion> {
        self.active.get(id)
    }

    fn pending_ingestions(&self) -> impl Iterator<Item = (&NoteId, &PendingIngestion)> {
        self.active.iter()
    }

    fn active_is_empty(&self) -> bool {
        self.active.is_empty()
    }

    fn take_stored(&mut self, id: &NoteId) -> bool {
        if self.active.remove(id).is_none() {
            return false;
        }
        self.clear_id(id);
        true
    }

    fn take_timed_out(&mut self, now: Instant) -> Vec<(NoteId, PendingIngestion)> {
        let timed_out = self
            .active
            .iter()
            .filter(|(_, pending)| now.duration_since(pending.started_at) >= INGESTION_TIMEOUT)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();

        let mut pending = Vec::with_capacity(timed_out.len());
        for id in timed_out {
            if let Some(active) = self.active.remove(&id) {
                pending.push((id, active));
            }
        }
        pending
    }

    fn upsert_retry(
        &mut self,
        id: NoteId,
        target: FullHistoryRelayFilter,
        next_retries_started: usize,
        next_retry_at: Instant,
    ) {
        let relay = target.relay.clone();
        if let Some(fetch) = self
            .relay_states
            .iter_mut()
            .find(|fetch| fetch.matches_relay_id(&id, &relay))
        {
            match &mut fetch.state {
                FullHistoryRelayFetchState::Retry(retry) => {
                    retry.target = target;
                    retry.next_retries_started =
                        retry.next_retries_started.max(next_retries_started);
                    retry.next_retry_at = next_retry_at;
                    return;
                }
                FullHistoryRelayFetchState::Candidate(_)
                | FullHistoryRelayFetchState::Failed(_) => {}
            }
            fetch.state = FullHistoryRelayFetchState::Retry(FullHistoryFetchRetryState {
                target,
                next_retries_started,
                next_retry_at,
            });
            return;
        }

        self.relay_states.push(FullHistoryRelayFetch {
            id,
            state: FullHistoryRelayFetchState::Retry(FullHistoryFetchRetryState {
                target,
                next_retries_started,
                next_retry_at,
            }),
        });
    }

    fn state_matches(
        &self,
        id: &NoteId,
        relay: &NormRelayUrl,
        matches: impl Fn(&FullHistoryRelayFetchState) -> bool,
    ) -> bool {
        self.relay_states
            .iter()
            .find(|fetch| fetch.matches_relay_id(id, relay))
            .is_some_and(|fetch| matches(&fetch.state))
    }

    fn clear_id(&mut self, id: &NoteId) {
        self.active.remove(id);
        self.relay_states.retain(|fetch| &fetch.id != id);
    }

    fn record_failed(&mut self, id: NoteId, target: FullHistoryRelayFilter) {
        let relay = target.relay.clone();
        if let Some(fetch) = self
            .relay_states
            .iter_mut()
            .find(|fetch| fetch.matches_relay_id(&id, &relay))
        {
            fetch.state = FullHistoryRelayFetchState::Failed(FullHistoryFailedFetch { target });
            return;
        }

        self.relay_states.push(FullHistoryRelayFetch {
            id,
            state: FullHistoryRelayFetchState::Failed(FullHistoryFailedFetch { target }),
        });
    }

    fn queue_candidate(
        &mut self,
        id: NoteId,
        target: FullHistoryRelayFilter,
        retries_started: usize,
    ) {
        if self
            .active
            .get(&id)
            .is_some_and(|pending| pending.target.relay == target.relay)
        {
            return;
        }

        let relay = target.relay.clone();
        if let Some(fetch) = self
            .relay_states
            .iter_mut()
            .find(|fetch| fetch.matches_relay_id(&id, &relay))
        {
            match &mut fetch.state {
                FullHistoryRelayFetchState::Retry(_) => return,
                FullHistoryRelayFetchState::Candidate(candidate) => {
                    candidate.target = target;
                    candidate.retries_started = candidate.retries_started.max(retries_started);
                    return;
                }
                FullHistoryRelayFetchState::Failed(_) => {
                    fetch.state =
                        FullHistoryRelayFetchState::Candidate(FullHistoryFetchCandidate {
                            target,
                            retries_started,
                        });
                    return;
                }
            };
        }

        self.relay_states.push(FullHistoryRelayFetch {
            id,
            state: FullHistoryRelayFetchState::Candidate(FullHistoryFetchCandidate {
                target,
                retries_started,
            }),
        });
    }

    fn take_candidate_batches(
        &mut self,
        history_id: FullHistorySubId,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        let mut retained = Vec::with_capacity(self.relay_states.len());

        for fetch in self.relay_states.drain(..) {
            if self.active.contains_key(&fetch.id) {
                retained.push(fetch);
                continue;
            }

            match fetch.state {
                FullHistoryRelayFetchState::Candidate(candidate) => {
                    push_need_batch(
                        &mut batches,
                        FullHistoryNeedBatch {
                            history_id,
                            target: candidate.target,
                            ids: vec![fetch.id],
                            retries_started: candidate.retries_started,
                        },
                    );
                }
                FullHistoryRelayFetchState::Retry(_) | FullHistoryRelayFetchState::Failed(_) => {
                    retained.push(fetch);
                }
            }
        }

        self.relay_states = retained;
        batches
    }

    fn take_due_retry_batches(
        &mut self,
        history_id: FullHistorySubId,
        now: Instant,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        let mut retained = Vec::with_capacity(self.relay_states.len());

        for fetch in self.relay_states.drain(..) {
            if self.active.contains_key(&fetch.id) {
                retained.push(fetch);
                continue;
            }

            match fetch.state {
                FullHistoryRelayFetchState::Retry(retry) if retry.next_retry_at <= now => {
                    if retry.next_retries_started > MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID {
                        retained.push(FullHistoryRelayFetch {
                            id: fetch.id,
                            state: FullHistoryRelayFetchState::Failed(FullHistoryFailedFetch {
                                target: retry.target,
                            }),
                        });
                        continue;
                    }

                    push_need_batch(
                        &mut batches,
                        FullHistoryNeedBatch {
                            history_id,
                            target: retry.target,
                            ids: vec![fetch.id],
                            retries_started: retry.next_retries_started,
                        },
                    );
                }
                FullHistoryRelayFetchState::Candidate(_)
                | FullHistoryRelayFetchState::Retry(_)
                | FullHistoryRelayFetchState::Failed(_) => retained.push(fetch),
            }
        }

        self.relay_states = retained;
        batches
    }

    fn clear(&mut self) {
        self.active.clear();
        self.relay_states.clear();
    }
}

/// Retry policy state for one relay/filter pair in a full-history sub.
pub(in crate::relay::outbox) struct FullHistoryRetryState {
    pub(in crate::relay::outbox) target: FullHistoryRelayFilter,
    pub(in crate::relay::outbox) attempts_started: usize,
    pub(in crate::relay::outbox) next_retry_at: Option<Instant>,
}

impl FullHistoryRetryState {
    /// Whether this state tracks the same relay and canonical filter.
    fn matches(&self, target: &FullHistoryRelayFilter) -> bool {
        self.target.semantically_matches(target)
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
    /// Active and follow-up fetch state for negentropy-discovered missing ids.
    fetches: FullHistoryFetches,
}

impl FullHistoryProgress {
    /// Whether this progress has a completed local set ready to start against
    /// `relay`.
    fn has_ready_pending_neg_set_for_relay(&self, relay: &NormRelayUrl) -> bool {
        self.pending_neg_sets.iter().any(|pending| {
            pending.storage.is_some()
                && pending
                    .relays
                    .iter()
                    .any(|pending_relay| pending_relay == relay)
        })
    }

    /// Visit aggregate relay transport demand contributed by pending
    /// full-history work.
    fn for_each_relay_transport_demand(
        &self,
        mut visit: impl FnMut(&NormRelayUrl, RelayConnectionPriority, RelayUrlSource, u32),
    ) {
        for pending in &self.pending_neg_sets {
            for relay in &pending.relays {
                let Some(relay_policy) = pending.relay_policy_for_relay(relay) else {
                    continue;
                };
                let count = pending
                    .relays
                    .iter()
                    .filter(|pending_relay| *pending_relay == relay)
                    .count();
                let Some(priority) =
                    RelayConnectionPriority::from_demand(relay_policy.demand_priority(), count)
                else {
                    continue;
                };
                visit(
                    relay,
                    priority,
                    relay_policy.source(),
                    relay_policy.connection_weight(),
                );
            }
        }

        for retry in &self.retry_states {
            if retry.next_retry_at.is_none() {
                continue;
            }
            let Some(priority) =
                RelayConnectionPriority::from_demand(retry.target.demand_priority(), 1)
            else {
                continue;
            };
            visit(
                &retry.target.relay,
                priority,
                retry.target.relay_policy.source(),
                retry.target.relay_policy.connection_weight(),
            );
        }

        for pending in &self.pending_needs {
            let Some(priority) = RelayConnectionPriority::from_demand(
                pending.target.demand_priority(),
                pending.ids.len(),
            ) else {
                continue;
            };
            visit(
                &pending.target.relay,
                priority,
                pending.target.relay_policy.source(),
                pending.target.relay_policy.connection_weight(),
            );
        }

        self.fetches.for_each_transport_demand(|target| {
            let Some(priority) = RelayConnectionPriority::from_demand(target.demand_priority(), 1)
            else {
                return;
            };
            visit(
                &target.relay,
                priority,
                target.relay_policy.source(),
                target.relay_policy.connection_weight(),
            );
        });
    }

    /// Whether one tracked sub still has local full-history work in flight.
    #[cfg(test)]
    fn has_pending_work(&self) -> bool {
        !self.pending_neg_sets.is_empty()
            || !self.pending_needs.is_empty()
            || self.fetches.has_pending_work()
            || self
                .retry_states
                .iter()
                .any(|retry| retry.next_retry_at.is_some())
    }

    /// Whether this progress has work that still belongs to `snapshot`.
    fn has_pending_work_for_snapshot(&self, snapshot: &FullHistorySnapshot) -> bool {
        self.pending_neg_sets.iter().any(|pending| {
            pending
                .relay_filters()
                .iter()
                .any(|target| snapshot.contains_relay_filter_target(target))
        }) || self
            .pending_needs
            .iter()
            .any(|needs| snapshot.contains_relay_filter_target(&needs.target))
            || self.fetches.has_pending_work_for_snapshot(snapshot)
            || self.retry_states.iter().any(|retry| {
                retry.next_retry_at.is_some()
                    && snapshot.contains_relay_filter_target(&retry.target)
            })
    }

    /// Enqueue one pending local-set build while preserving a single pending
    /// relay leg per canonical filter.
    fn enqueue_pending_neg_set(
        &mut self,
        history_id: FullHistorySubId,
        next_request_id: &mut u64,
        local_set_requests: &mut Vec<FullHistoryLocalSetRequest>,
        filter: Filter,
        relay_filters: Vec<FullHistoryRelayFilter>,
    ) -> bool {
        let mut new_relay_filters: Vec<FullHistoryRelayFilter> = Vec::new();
        for relay_filter in relay_filters {
            if let Some(existing) = new_relay_filters
                .iter_mut()
                .find(|existing| existing.has_same_relay_filter(&relay_filter))
            {
                existing.merge_policy_from(&relay_filter);
                continue;
            }
            new_relay_filters.push(relay_filter);
        }

        let mut matching_pending = None;
        for (index, pending) in self.pending_neg_sets.iter().enumerate() {
            if !pending.filter.same_canonical_attributes(&filter) {
                continue;
            }

            matching_pending.get_or_insert(index);
            new_relay_filters.retain(|relay_filter| !pending.relays.contains(&relay_filter.relay));
        }
        if new_relay_filters.is_empty() {
            return false;
        }

        if let Some(index) = matching_pending {
            self.pending_neg_sets[index].add_relays(new_relay_filters);
            return true;
        }

        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        local_set_requests.push(FullHistoryLocalSetRequest {
            history_id,
            request_id,
            filter: filter.clone(),
        });
        self.pending_neg_sets
            .push(PendingNegSet::new(request_id, filter, new_relay_filters));

        true
    }

    /// Earliest time-based deadline for this sub's pending full-history work.
    fn next_deadline(
        &self,
        can_retry: bool,
        can_check_ingestion: bool,
        now: Instant,
    ) -> Option<Instant> {
        let retry_deadline = can_retry
            .then(|| {
                self.retry_states
                    .iter()
                    .filter_map(FullHistoryRetryState::next_deadline)
                    .min()
            })
            .flatten();
        let fetch_retry_deadline = can_check_ingestion
            .then(|| self.fetches.next_retry_deadline())
            .flatten();
        let fetch_candidate_deadline = self.fetches.has_ready_candidate().then_some(now);
        let ingestion_deadline = can_check_ingestion
            .then(|| self.fetches.ingestion_deadline())
            .flatten();
        let needs_deadline = (!self.pending_needs.is_empty()).then_some(now);

        [
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

    /// Drop queued work that no longer belongs to the current relay/filter set,
    /// and refresh retained relay package policy from the current snapshot.
    fn retain_relay_filters(&mut self, snapshot: &FullHistorySnapshot) {
        self.pending_neg_sets
            .retain_mut(|pending| pending.retain_relay_filters(snapshot));

        self.retry_states
            .retain_mut(|retry| refresh_target_policy(&mut retry.target, snapshot));

        self.pending_needs
            .retain_mut(|needs| refresh_target_policy(&mut needs.target, snapshot));
        self.fetches.retain_relay_filters(snapshot);
    }

    /// Schedule a delayed retry for one relay/filter pair, subject to this
    /// snapshot's retry budget.
    pub(in crate::relay::outbox) fn schedule_retry(
        &mut self,
        target: FullHistoryRelayFilter,
        now: Instant,
    ) {
        if let Some(retry) = self
            .retry_states
            .iter_mut()
            .find(|retry| retry.matches(&target))
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
            target,
            attempts_started: 0,
            next_retry_at: Some(now + full_history_retry_delay(0)),
        });
    }

    /// Start local-set builds for retry entries whose backoff has elapsed.
    fn promote_due_retries(
        &mut self,
        history_id: FullHistorySubId,
        next_request_id: &mut u64,
        local_set_requests: &mut Vec<FullHistoryLocalSetRequest>,
        now: Instant,
    ) -> HashSet<NormRelayUrl> {
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

            due_retries.push((index, retry.target.clone()));
        }

        let mut touched_relays = HashSet::new();
        for index in exhausted_retries {
            touched_relays.insert(self.retry_states[index].target.relay.clone());
            self.retry_states[index].next_retry_at = None;
        }
        for (index, target) in due_retries {
            touched_relays.insert(target.relay.clone());
            if self.enqueue_pending_neg_set(
                history_id,
                next_request_id,
                local_set_requests,
                target.filter.clone(),
                vec![target],
            ) {
                self.retry_states[index].attempts_started += 1;
            }
            self.retry_states[index].next_retry_at = None;
        }
        touched_relays
    }

    fn apply_pending_ingestion_stored(&mut self, id: NoteId) -> bool {
        if !self.fetches.take_stored(&id) {
            return false;
        }

        self.pending_ingestion_is_empty() && self.pending_needs.is_empty()
    }

    /// Returns true once timed-out in-flight fetches leave this sub with no
    /// remaining current fetch batch.
    fn apply_ingestion_timeouts(&mut self, now: Instant) -> bool {
        let timed_out = self.fetches.take_timed_out(now);
        if timed_out.is_empty() {
            return false;
        }

        for (id, pending) in timed_out {
            self.schedule_fetch_retry(id, pending, now);
        }

        self.pending_ingestion_is_empty() && self.pending_needs.is_empty()
    }

    fn apply_local_set_ready(&mut self, request_id: u64, storage: NegentropyStorageVector) -> bool {
        let Some(pending) = self
            .pending_neg_sets
            .iter_mut()
            .find(|pending| pending.request_id == request_id)
        else {
            return false;
        };

        pending.storage = Some(storage);
        true
    }

    fn apply_local_set_failed(&mut self, request_id: u64, now: Instant) -> bool {
        let Some(index) = self
            .pending_neg_sets
            .iter()
            .position(|pending| pending.request_id == request_id)
        else {
            return false;
        };

        let pending = self.pending_neg_sets.swap_remove(index);
        for target in pending.relay_filters() {
            self.schedule_retry(target, now);
        }
        true
    }

    /// Clear work that was based on the previous local negentropy set before
    /// scheduling a fresh verification round.
    pub(in crate::relay::outbox) fn clear_round_work(&mut self) {
        self.pending_neg_sets.clear();
        self.retry_states.clear();
        self.pending_needs.clear();
        self.fetches.clear();
    }

    pub(in crate::relay::outbox) fn start_pending_ingestion(
        &mut self,
        id: NoteId,
        pending: PendingIngestion,
    ) {
        self.fetches.start_pending_ingestion(id, pending);
    }

    pub(in crate::relay::outbox) fn pending_ingestion(
        &self,
        id: &NoteId,
    ) -> Option<&PendingIngestion> {
        self.fetches.pending_ingestion(id)
    }

    pub(in crate::relay::outbox) fn pending_ingestions(
        &self,
    ) -> impl Iterator<Item = (&NoteId, &PendingIngestion)> {
        self.fetches.pending_ingestions()
    }

    #[cfg(test)]
    pub(in crate::relay::outbox) fn pending_ingestion_len(&self) -> usize {
        self.pending_ingestions().count()
    }

    pub(in crate::relay::outbox) fn pending_ingestion_is_empty(&self) -> bool {
        self.fetches.active_is_empty()
    }

    pub(in crate::relay::outbox) fn upsert_fetch_retry(
        &mut self,
        id: NoteId,
        target: FullHistoryRelayFilter,
        next_retries_started: usize,
        next_retry_at: Instant,
    ) {
        self.fetches
            .upsert_retry(id, target, next_retries_started, next_retry_at);
    }

    /// Whether retry state already owns the next fetch for this relay/id.
    pub(in crate::relay::outbox) fn fetch_retry_waiting(
        &self,
        id: &NoteId,
        relay: &NormRelayUrl,
    ) -> bool {
        self.fetches.state_matches(id, relay, |state| {
            matches!(state, FullHistoryRelayFetchState::Retry(_))
        })
    }

    /// Whether this relay/id exhausted its fetch retry policy this round.
    pub(in crate::relay::outbox) fn fetch_failed(&self, id: &NoteId, relay: &NormRelayUrl) -> bool {
        self.fetches.state_matches(id, relay, |state| {
            matches!(state, FullHistoryRelayFetchState::Failed(_))
        })
    }

    /// Whether an alternate relay is waiting for this id.
    pub(in crate::relay::outbox) fn fetch_candidate_waiting(
        &self,
        id: &NoteId,
        relay: &NormRelayUrl,
    ) -> bool {
        self.fetches.state_matches(id, relay, |state| {
            matches!(state, FullHistoryRelayFetchState::Candidate(_))
        })
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
        self.fetches.clear_id(id);
    }

    fn schedule_fetch_retry(&mut self, id: NoteId, pending: PendingIngestion, now: Instant) {
        let next_retries_started = pending.retries_started + 1;
        if next_retries_started > MAX_FULL_HISTORY_FETCH_RETRIES_PER_RELAY_ID {
            self.record_failed_fetch(id, pending.target);
            return;
        }

        let next_retry_at = next_fetch_retry_at(next_retries_started, now);
        self.upsert_fetch_retry(id, pending.target, next_retries_started, next_retry_at);
    }

    fn record_failed_fetch(&mut self, id: NoteId, target: FullHistoryRelayFilter) {
        self.fetches.record_failed(id, target);
    }

    /// Remember another relay that can be tried if the active fetch for `id`
    /// times out.
    pub(in crate::relay::outbox) fn queue_fetch_candidate(
        &mut self,
        id: NoteId,
        target: FullHistoryRelayFilter,
        retries_started: usize,
    ) {
        self.fetches.queue_candidate(id, target, retries_started);
    }

    /// Queue one surfaced need for bounded local presence planning.
    fn queue_need(&mut self, need: FullHistoryNeed) {
        if self.pending_ingestion(&need.id).is_some() {
            self.queue_fetch_candidate(need.id, need.target, 0);
            return;
        }
        if let Some(pending) = self
            .pending_needs
            .iter_mut()
            .find(|pending| pending.matches_relay_filter(&need.target))
        {
            pending.push_id(need.id);
            return;
        }

        self.pending_needs
            .push_back(QueuedFullHistoryNeeds::from_need(need));
    }

    /// Take queued ids for this sub, preserving relay/filter grouping.
    fn take_queued_need_batches(
        &mut self,
        history_id: FullHistorySubId,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        let mut index = 0;
        let active_ids = self
            .pending_ingestions()
            .map(|(id, _)| *id)
            .collect::<HashSet<_>>();

        while index < self.pending_needs.len() {
            let mut ids = Vec::new();
            let mut fetch_candidates = Vec::new();
            let target;
            {
                let pending = &mut self.pending_needs[index];
                target = pending.target.clone();
                while let Some(id) = pending.ids.pop_front() {
                    pending.id_set.remove(&id);
                    if active_ids.contains(&id) {
                        fetch_candidates.push(id);
                        continue;
                    }
                    ids.push(id);
                }
            }
            for id in fetch_candidates {
                self.queue_fetch_candidate(id, target.clone(), 0);
            }

            let pending = &self.pending_needs[index];
            if !ids.is_empty() {
                batches.push(FullHistoryNeedBatch {
                    history_id,
                    target: pending.target.clone(),
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
    ) -> Vec<FullHistoryNeedBatch> {
        self.fetches.take_candidate_batches(history_id)
    }

    /// Take due relay-local fetch retries, preserving relay/filter grouping.
    fn take_due_fetch_retry_batches(
        &mut self,
        history_id: FullHistorySubId,
        now: Instant,
    ) -> Vec<FullHistoryNeedBatch> {
        self.fetches.take_due_retry_batches(history_id, now)
    }
}

fn push_need_batch(batches: &mut Vec<FullHistoryNeedBatch>, batch: FullHistoryNeedBatch) {
    if let Some(existing) = batches.iter_mut().find(|existing| {
        existing.retries_started == batch.retries_started
            && existing.target.semantically_matches(&batch.target)
    }) {
        existing.ids.extend(batch.ids);
        return;
    }

    batches.push(batch);
}

fn refresh_target_policy(
    target: &mut FullHistoryRelayFilter,
    snapshot: &FullHistorySnapshot,
) -> bool {
    let Some(current) = snapshot.target_for_relay_filter(&target.relay, &target.filter) else {
        return false;
    };
    target.relay_policy = current.relay_policy;
    true
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

    /// Replace the snapshot and reconcile progress with the caller's new target set.
    fn replace_snapshot(&mut self, snapshot: FullHistorySnapshot, reset_rounds: bool) {
        self.snapshot = snapshot;
        if reset_rounds {
            self.rounds_started = 0;
        }

        self.progress.retain_relay_filters(&self.snapshot);
    }

    /// Schedule one bounded negentropy round for this tracked sub.
    fn schedule_round(
        &mut self,
        history_id: FullHistorySubId,
        next_request_id: &mut u64,
        local_set_requests: &mut Vec<FullHistoryLocalSetRequest>,
    ) {
        if self.rounds_started >= MAX_FULL_HISTORY_ROUNDS {
            return;
        }

        let had_retained_work = self.progress.has_pending_work_for_snapshot(&self.snapshot);
        let queued = self.enqueue_round(history_id, next_request_id, local_set_requests);
        if !queued && !had_retained_work {
            return;
        }

        self.rounds_started += 1;
    }

    /// Start any retry local-set builds whose backoff has elapsed.
    fn promote_due_retries(
        &mut self,
        history_id: FullHistorySubId,
        next_request_id: &mut u64,
        local_set_requests: &mut Vec<FullHistoryLocalSetRequest>,
        now: Instant,
    ) -> HashSet<NormRelayUrl> {
        self.progress
            .promote_due_retries(history_id, next_request_id, local_set_requests, now)
    }

    fn enqueue_round(
        &mut self,
        history_id: FullHistorySubId,
        next_request_id: &mut u64,
        local_set_requests: &mut Vec<FullHistoryLocalSetRequest>,
    ) -> bool {
        let mut grouped: Vec<(Filter, Vec<FullHistoryRelayFilter>)> = Vec::new();
        for relay_filter in self.snapshot.relay_filters() {
            if let Some((_, relays)) = grouped
                .iter_mut()
                .find(|(filter, _)| filter.same_canonical_attributes(&relay_filter.filter))
            {
                relays.push(relay_filter);
                continue;
            }
            grouped.push((relay_filter.filter.clone(), vec![relay_filter]));
        }

        let mut queued = false;
        for (filter, relay_filters) in grouped {
            queued |= self.progress.enqueue_pending_neg_set(
                history_id,
                next_request_id,
                local_set_requests,
                filter,
                relay_filters,
            );
        }
        queued
    }

    /// Schedule local-set builds for newly added relay/filter pairs.
    fn schedule_relay_filters(
        &mut self,
        history_id: FullHistorySubId,
        relay_filters: Vec<FullHistoryRelayFilter>,
        next_request_id: &mut u64,
        local_set_requests: &mut Vec<FullHistoryLocalSetRequest>,
    ) {
        if relay_filters.is_empty() {
            return;
        }

        let mut grouped: Vec<(Filter, Vec<FullHistoryRelayFilter>)> = Vec::new();
        for relay_filter in relay_filters {
            if let Some((_, relays)) = grouped
                .iter_mut()
                .find(|(filter, _)| filter.same_canonical_attributes(&relay_filter.filter))
            {
                relays.push(relay_filter);
                continue;
            }
            grouped.push((relay_filter.filter.clone(), vec![relay_filter]));
        }

        for (filter, relay_filters) in grouped {
            self.progress.enqueue_pending_neg_set(
                history_id,
                next_request_id,
                local_set_requests,
                filter,
                relay_filters,
            );
        }
    }

    /// Whether this tracked full-history sub still needs upkeep work.
    #[cfg(test)]
    fn has_pending_work(&self) -> bool {
        self.progress.has_pending_work()
    }

    /// Whether at least one initial reconciliation round has drained locally.
    #[cfg(test)]
    fn initial_round_complete(&self) -> bool {
        self.rounds_started > 0 && !self.has_pending_work()
    }

    /// Visit relay transport demand contributed by this tracked sub's pending work.
    fn for_each_relay_transport_demand(
        &self,
        visit: impl FnMut(&NormRelayUrl, RelayConnectionPriority, RelayUrlSource, u32),
    ) {
        self.progress.for_each_relay_transport_demand(visit);
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
pub(in crate::relay::outbox) struct FullHistoryRuntime {
    pub(in crate::relay::outbox) tracked_subs: HashMap<FullHistorySubId, TrackedFullHistorySub>,
    relay_transport_demand: HashMap<NormRelayUrl, RelayTransportDemand>,
    relay_transport_demand_by_sub:
        HashMap<FullHistorySubId, HashMap<NormRelayUrl, RelayTransportDemand>>,
    relay_transport_demand_by_relay:
        HashMap<NormRelayUrl, HashMap<FullHistorySubId, RelayTransportDemand>>,
    next_local_set_request_id: u64,
    next_local_presence_request_id: u64,
    pending_local_presence_plans: HashMap<u64, Vec<FullHistoryNeedBatch>>,
}

impl FullHistoryRuntime {
    pub(in crate::relay::outbox) fn take_local_presence_plan(
        &mut self,
        request_id: u64,
    ) -> Option<Vec<FullHistoryNeedBatch>> {
        self.pending_local_presence_plans.remove(&request_id)
    }

    /// Upsert one full-history sub snapshot and report relay/filter changes.
    pub(in crate::relay::outbox) fn upsert(
        &mut self,
        snapshot: FullHistorySnapshot,
    ) -> FullHistoryUpsert {
        let id = snapshot.id;
        let upsert = match self.tracked_subs.get_mut(&id) {
            Some(tracked) => {
                if tracked.snapshot.semantically_matches(&snapshot) {
                    tracked.replace_snapshot(snapshot, false);
                    FullHistoryUpsert::Unchanged
                } else {
                    let previous_relays = tracked.snapshot.relay_filters();
                    let next_relays = snapshot.relay_filters();
                    let added = full_history_relay_filter_diff(&next_relays, &previous_relays);
                    let removed = full_history_relay_filter_diff(&previous_relays, &next_relays);
                    let previous_filters = tracked.snapshot.filters();
                    let next_filters = snapshot.filters();
                    let filters_changed =
                        !same_canonical_filter_set(&previous_filters, &next_filters);
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
        };

        if !matches!(upsert, FullHistoryUpsert::Inserted) {
            self.retain_local_presence_plans_for_snapshot(id);
        }

        upsert
    }

    /// Whether one tracked sub already has the same normalized target set.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn normalized_targets_fully_match(
        &self,
        id: FullHistorySubId,
        targets: &[FullHistoryRelayFilter],
    ) -> bool {
        self.tracked_subs
            .get(&id)
            .is_some_and(|tracked| tracked.snapshot.fully_matches_targets(id, targets))
    }

    /// Clone one tracked sub snapshot for callers that need stable projection
    /// metadata while mutating other pool state.
    pub(in crate::relay::outbox) fn snapshot(
        &self,
        id: FullHistorySubId,
    ) -> Option<FullHistorySnapshot> {
        self.tracked_subs
            .get(&id)
            .map(|tracked| tracked.snapshot.clone())
    }

    /// Return the current relay/filter target for one tracked sub leg.
    pub(in crate::relay::outbox) fn target_for_relay_filter(
        &self,
        id: FullHistorySubId,
        relay: &NormRelayUrl,
        filter: &Filter,
    ) -> Option<FullHistoryRelayFilter> {
        self.tracked_subs
            .get(&id)
            .and_then(|tracked| tracked.snapshot.target_for_relay_filter(relay, filter))
    }

    /// Schedule a delayed retry for one current relay/filter target.
    pub(in crate::relay::outbox) fn schedule_relay_filter_retry(
        &mut self,
        id: FullHistorySubId,
        target: FullHistoryRelayFilter,
        now: Instant,
    ) -> bool {
        let Some(tracked) = self.tracked_subs.get_mut(&id) else {
            return false;
        };
        tracked.progress.schedule_retry(target, now);
        true
    }

    /// Return current relay/filter targets for one tracked full-history sub.
    pub(in crate::relay::outbox) fn relay_filters(
        &self,
        id: FullHistorySubId,
    ) -> Vec<FullHistoryRelayFilter> {
        self.tracked_subs
            .get(&id)
            .map(|tracked| tracked.snapshot.relay_filters())
            .unwrap_or_default()
    }

    /// Drop one tracked full-history sub and all owned progress state tied to it.
    pub(in crate::relay::outbox) fn remove(
        &mut self,
        id: FullHistorySubId,
    ) -> HashMap<NormRelayUrl, Option<RelayTransportDemand>> {
        self.tracked_subs.remove(&id);
        self.retain_local_presence_plans(|batch| batch.history_id != id);
        self.refresh_relay_transport_demand_for_sub(id)
    }

    /// Schedule one bounded negentropy round for a tracked sub.
    pub(in crate::relay::outbox) fn schedule_round(
        &mut self,
        id: FullHistorySubId,
    ) -> FullHistoryOutput {
        let mut local_set_requests = Vec::new();
        if let Some(tracked) = self.tracked_subs.get_mut(&id) {
            tracked.schedule_round(
                id,
                &mut self.next_local_set_request_id,
                &mut local_set_requests,
            );
        }
        FullHistoryOutput {
            local_set_requests,
            relay_demand_changes: self.refresh_relay_transport_demand_for_sub(id),
            ..Default::default()
        }
    }

    /// Schedule local-set builds for newly added relay/filter pairs.
    pub(in crate::relay::outbox) fn schedule_relay_filters(
        &mut self,
        id: FullHistorySubId,
        relay_filters: Vec<FullHistoryRelayFilter>,
    ) -> FullHistoryOutput {
        let mut local_set_requests = Vec::new();
        if let Some(tracked) = self.tracked_subs.get_mut(&id) {
            tracked.schedule_relay_filters(
                id,
                relay_filters,
                &mut self.next_local_set_request_id,
                &mut local_set_requests,
            );
        }
        FullHistoryOutput {
            local_set_requests,
            relay_demand_changes: self.refresh_relay_transport_demand_for_sub(id),
            ..Default::default()
        }
    }

    /// Clear current round progress and schedule a fresh bounded round.
    pub(in crate::relay::outbox) fn restart_round(
        &mut self,
        id: FullHistorySubId,
    ) -> FullHistoryOutput {
        let Some(tracked) = self.tracked_subs.get_mut(&id) else {
            return FullHistoryOutput::default();
        };
        tracked.progress.clear_round_work();
        self.retain_local_presence_plans(|batch| batch.history_id != id);
        self.schedule_round(id)
    }

    /// Start due retry work for all tracked subs.
    pub(in crate::relay::outbox) fn promote_due_retries(
        &mut self,
        now: Instant,
    ) -> FullHistoryOutput {
        let mut local_set_requests = Vec::new();
        let mut touched_subs = HashSet::new();
        for (&history_id, tracked) in &mut self.tracked_subs {
            if !tracked
                .promote_due_retries(
                    history_id,
                    &mut self.next_local_set_request_id,
                    &mut local_set_requests,
                    now,
                )
                .is_empty()
            {
                touched_subs.insert(history_id);
            }
        }
        FullHistoryOutput {
            local_set_requests,
            relay_demand_changes: self.refresh_relay_transport_demand_for_subs(touched_subs),
            ..Default::default()
        }
    }

    /// Queue relay-surfaced needs under the owning tracked sub.
    pub(in crate::relay::outbox) fn queue_needs(&mut self, needs: Vec<FullHistoryNeed>) {
        for need in needs {
            let Some(tracked) = self.tracked_subs.get_mut(&need.history_id) else {
                continue;
            };
            if !tracked.snapshot.contains_relay_filter_target(&need.target) {
                continue;
            }
            tracked.progress.queue_need(need);
        }
    }

    /// Take all relay-local fetch ids ready for local presence planning.
    pub(in crate::relay::outbox) fn take_need_batches(
        &mut self,
        now: Instant,
    ) -> Vec<FullHistoryNeedBatch> {
        let mut batches = Vec::new();
        for (&history_id, tracked) in &mut self.tracked_subs {
            let mut taken = tracked.progress.take_fetch_candidate_batches(history_id);
            taken.append(&mut tracked.progress.take_queued_need_batches(history_id));
            taken.extend(
                tracked
                    .progress
                    .take_due_fetch_retry_batches(history_id, now),
            );
            batches.extend(taken);
        }
        batches
    }

    pub(in crate::relay::outbox) fn enqueue_local_presence_request(
        &mut self,
        batches: Vec<FullHistoryNeedBatch>,
    ) -> Option<FullHistoryLocalPresenceRequest> {
        if batches.is_empty() {
            return None;
        }

        let candidate_ids = batches
            .iter()
            .flat_map(|batch| batch.ids.iter().copied())
            .collect::<HashSet<_>>();
        if candidate_ids.is_empty() {
            return None;
        }

        let request_id = self.next_local_presence_request_id;
        self.next_local_presence_request_id = self.next_local_presence_request_id.wrapping_add(1);
        self.pending_local_presence_plans
            .insert(request_id, batches);
        Some(FullHistoryLocalPresenceRequest {
            request_id,
            candidate_ids,
        })
    }

    pub(in crate::relay::outbox) fn enqueue_pending_ingestion_presence_request(
        &mut self,
        candidate_ids: HashSet<NoteId>,
        deadline: Instant,
    ) -> Option<FullHistoryPendingIngestionPresenceRequest> {
        if candidate_ids.is_empty() {
            return None;
        }

        Some(FullHistoryPendingIngestionPresenceRequest {
            candidate_ids,
            deadline,
        })
    }

    /// Earliest time-based deadline among tracked full-history subs.
    pub(in crate::relay::outbox) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.tracked_subs
            .values()
            .filter_map(|tracked| tracked.next_deadline(true, true, now))
            .min()
    }

    pub(in crate::relay::outbox) fn apply_local_set_ready(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
        storage: NegentropyStorageVector,
    ) -> bool {
        self.tracked_subs
            .get_mut(&history_id)
            .is_some_and(|tracked| tracked.progress.apply_local_set_ready(request_id, storage))
    }

    pub(in crate::relay::outbox) fn apply_local_set_failed(
        &mut self,
        history_id: FullHistorySubId,
        request_id: u64,
        now: Instant,
    ) -> bool {
        self.tracked_subs
            .get_mut(&history_id)
            .is_some_and(|tracked| tracked.progress.apply_local_set_failed(request_id, now))
    }

    /// Apply backend storage presence to every active full-history fetch state
    /// still waiting for those ids.
    pub(in crate::relay::outbox) fn apply_pending_ingestion_presence_result(
        &mut self,
        result: FullHistoryPendingIngestionPresenceResult,
    ) -> Vec<FullHistorySubId> {
        let mut completed = HashSet::new();
        for id in result.stored_ids {
            for (&history_id, tracked) in &mut self.tracked_subs {
                if tracked.progress.apply_pending_ingestion_stored(id) {
                    completed.insert(history_id);
                }
            }
        }

        completed.into_iter().collect()
    }

    /// Return the tracked full-history ids whose current fetch batch timed out.
    pub(in crate::relay::outbox) fn timed_out_ingestion_subs(
        &mut self,
        now: Instant,
    ) -> Vec<FullHistorySubId> {
        self.tracked_subs
            .iter_mut()
            .filter_map(|(&history_id, tracked)| {
                tracked
                    .progress
                    .apply_ingestion_timeouts(now)
                    .then_some(history_id)
            })
            .collect()
    }

    /// Return tracked full-history ids currently contributing demand for `relay`.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn ids_with_relay_transport_demand(
        &self,
        relay: &NormRelayUrl,
    ) -> Vec<FullHistorySubId> {
        self.relay_transport_demand_by_relay
            .get(relay)
            .map(|contributors| contributors.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Return tracked full-history ids with a completed local set ready to use
    /// relay-local negentropy capacity.
    pub(in crate::relay::outbox) fn ids_with_ready_pending_neg_set_for_relay(
        &self,
        relay: &NormRelayUrl,
    ) -> Vec<FullHistorySubId> {
        self.tracked_subs
            .iter()
            .filter_map(|(&id, tracked)| {
                tracked
                    .progress
                    .has_ready_pending_neg_set_for_relay(relay)
                    .then_some(id)
            })
            .collect()
    }

    /// Advance one tracked sub's completed local-set builds, asking the caller
    /// to materialize each ready relay leg.
    pub(in crate::relay::outbox) fn advance_pending_neg_sets_for_sub(
        &mut self,
        history_id: FullHistorySubId,
        mut try_start: impl FnMut(FullHistoryNegentropyStart<'_>) -> FullHistoryNegentropyStartOutcome,
    ) {
        self.advance_pending_neg_sets_for_sub_matching(history_id, None, &mut try_start)
    }

    /// Advance one tracked sub's completed local-set builds for selected relays.
    pub(in crate::relay::outbox) fn advance_pending_neg_sets_for_sub_relays(
        &mut self,
        history_id: FullHistorySubId,
        relays: &HashSet<NormRelayUrl>,
        mut try_start: impl FnMut(FullHistoryNegentropyStart<'_>) -> FullHistoryNegentropyStartOutcome,
    ) {
        self.advance_pending_neg_sets_for_sub_matching(history_id, Some(relays), &mut try_start)
    }

    fn advance_pending_neg_sets_for_sub_matching(
        &mut self,
        history_id: FullHistorySubId,
        eligible_relays: Option<&HashSet<NormRelayUrl>>,
        try_start: &mut impl FnMut(FullHistoryNegentropyStart<'_>) -> FullHistoryNegentropyStartOutcome,
    ) {
        let Some(tracked) = self.tracked_subs.get_mut(&history_id) else {
            return;
        };
        if tracked.progress.pending_neg_sets.is_empty() {
            return;
        }

        let progress = &mut tracked.progress;
        let mut i = 0;
        while i < progress.pending_neg_sets.len() {
            if progress.pending_neg_sets[i].storage.is_none() {
                i += 1;
                continue;
            }

            let filter = progress.pending_neg_sets[i].filter.clone();
            let mut remaining_relays = Vec::new();

            for relay in std::mem::take(&mut progress.pending_neg_sets[i].relays) {
                if eligible_relays.is_some_and(|eligible_relays| !eligible_relays.contains(&relay))
                {
                    remaining_relays.push(relay);
                    continue;
                }
                let Some(relay_policy) =
                    progress.pending_neg_sets[i].relay_policy_for_relay(&relay)
                else {
                    continue;
                };
                let storage = progress.pending_neg_sets[i]
                    .storage
                    .as_ref()
                    .expect("ready storage");
                match try_start(FullHistoryNegentropyStart {
                    history_id,
                    relay: relay.clone(),
                    filter: &filter,
                    relay_policy,
                    storage,
                }) {
                    FullHistoryNegentropyStartOutcome::Started => {}
                    FullHistoryNegentropyStartOutcome::Drop => {}
                    FullHistoryNegentropyStartOutcome::Retry => {
                        remaining_relays.push(relay);
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

    /// Whether any tracked sub still has local full-history work in flight.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn has_pending_work(&self) -> bool {
        !self.pending_local_presence_plans.is_empty()
            || self
                .tracked_subs
                .values()
                .any(TrackedFullHistorySub::has_pending_work)
    }

    /// Whether a tracked sub's first local full-history round has completed.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn initial_round_complete(&self, id: FullHistorySubId) -> bool {
        self.tracked_subs
            .get(&id)
            .is_some_and(TrackedFullHistorySub::initial_round_complete)
    }

    fn retain_local_presence_plans_for_snapshot(&mut self, id: FullHistorySubId) {
        let Some(tracked) = self.tracked_subs.get(&id) else {
            self.retain_local_presence_plans(|batch| batch.history_id != id);
            return;
        };
        let snapshot = tracked.snapshot.clone();
        self.retain_local_presence_plans(|batch| {
            batch.history_id != id || snapshot.contains_relay_filter_target(&batch.target)
        });
    }

    fn retain_local_presence_plans(
        &mut self,
        mut retain: impl FnMut(&FullHistoryNeedBatch) -> bool,
    ) {
        self.pending_local_presence_plans.retain(|_, batches| {
            batches.retain(|batch| retain(batch));
            !batches.is_empty()
        });
    }

    /// Visit aggregate relay transport demand contributed by pending
    /// full-history work.
    #[cfg(test)]
    pub(in crate::relay::outbox) fn for_each_relay_transport_demand(
        &self,
        mut visit: impl FnMut(&NormRelayUrl, RelayConnectionPriority, RelayUrlSource, u32),
    ) {
        for (relay, demand) in &self.relay_transport_demand {
            visit(
                relay,
                demand.priority,
                demand.source,
                demand.connection_weight,
            );
        }
    }

    pub(in crate::relay::outbox) fn refresh_relay_transport_demand_for_sub(
        &mut self,
        id: FullHistorySubId,
    ) -> HashMap<NormRelayUrl, Option<RelayTransportDemand>> {
        let previous = self
            .relay_transport_demand_by_sub
            .remove(&id)
            .unwrap_or_default();
        let next = self.relay_transport_demand_for_sub(id);
        let touched_relays = previous
            .keys()
            .chain(next.keys())
            .cloned()
            .collect::<HashSet<_>>();

        for relay in previous.keys() {
            let remove_relay =
                if let Some(contributors) = self.relay_transport_demand_by_relay.get_mut(relay) {
                    contributors.remove(&id);
                    contributors.is_empty()
                } else {
                    false
                };
            if remove_relay {
                self.relay_transport_demand_by_relay.remove(relay);
            }
        }

        for (relay, demand) in &next {
            self.relay_transport_demand_by_relay
                .entry(relay.clone())
                .or_default()
                .insert(id, *demand);
        }

        if !next.is_empty() {
            self.relay_transport_demand_by_sub.insert(id, next);
        }

        self.relay_transport_demand_changes_from_index(touched_relays)
    }

    pub(in crate::relay::outbox) fn refresh_relay_transport_demand_for_subs(
        &mut self,
        ids: impl IntoIterator<Item = FullHistorySubId>,
    ) -> HashMap<NormRelayUrl, Option<RelayTransportDemand>> {
        let mut changes = HashMap::new();
        let mut seen = HashSet::new();
        for id in ids {
            if seen.insert(id) {
                changes.extend(self.refresh_relay_transport_demand_for_sub(id));
            }
        }
        changes
    }

    fn relay_transport_demand_for_sub(
        &self,
        id: FullHistorySubId,
    ) -> HashMap<NormRelayUrl, RelayTransportDemand> {
        let mut demands = HashMap::new();
        let Some(tracked) = self.tracked_subs.get(&id) else {
            return demands;
        };

        tracked.for_each_relay_transport_demand(|relay, priority, source, connection_weight| {
            let next = RelayTransportDemand::new(priority, source, connection_weight);
            if let Some(demand) =
                RelayTransportDemand::merge_optional(demands.get(relay).copied(), Some(next))
            {
                demands.insert(relay.clone(), demand);
            }
        });
        demands
    }

    fn relay_transport_demand_changes_from_index(
        &mut self,
        relays: HashSet<NormRelayUrl>,
    ) -> HashMap<NormRelayUrl, Option<RelayTransportDemand>> {
        let mut changes = HashMap::new();
        for relay in relays {
            let demand = self.indexed_relay_transport_demand_for(&relay);
            if self.relay_transport_demand.get(&relay).copied() == demand {
                continue;
            }

            if let Some(demand) = demand {
                self.relay_transport_demand.insert(relay.clone(), demand);
            } else {
                self.relay_transport_demand.remove(&relay);
            }
            changes.insert(relay, demand);
        }
        changes
    }

    fn indexed_relay_transport_demand_for(
        &self,
        demand_relay: &NormRelayUrl,
    ) -> Option<RelayTransportDemand> {
        self.relay_transport_demand_by_relay
            .get(demand_relay)?
            .values()
            .copied()
            .fold(None, |demand, next| {
                RelayTransportDemand::merge_optional(demand, Some(next))
            })
    }
}
