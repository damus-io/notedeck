use enostr::{
    NormRelayUrl, NoteId, OutboxIdRegistry, OutboxSubId, Pubkey, RelayReqStatus, RelayUrlPkgs,
    RelayUrlPolicy,
};
use futures_util::{future::poll_fn, StreamExt};
use hashbrown::{HashMap, HashSet};
use nostrdb::{Filter, Ndb, SendFilter};
use std::task::Poll;
use std::time::{Duration, Instant};

use super::config::{ScopedSubKey, SubConfig};
use super::planner::{AuthorOutboxPlanGeneration, PlannedAuthorOutboxRoutes, PlannedRoutedRelay};
use super::{ScopedSubEffect, ScopedSubEffects, ScopedSubOutboxOps};
use crate::author_outbox::{
    filter_author_pubkeys, plan_author_outbox_augmentation_for_indexed_filters,
    RelayDirectorySnapshot, RoutedFilter, RoutedRelayPriority,
};

mod discovery;
mod thread;

use discovery::{start_relay_list_discovery, RelayListDiscovery, RelayListDiscoveryAdvance};
use thread::{build_thread_plan, ThreadPlanSnapshot, ThreadWatch};

const THREAD_PLAN_RETRY_DELAY: Duration = Duration::from_millis(100);
const THREAD_PLAN_RETRY_MAX: Duration = Duration::from_secs(60);

const RELAY_LIST_INGESTION_WAIT_DELAYS: [Duration; 6] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_millis(1_000),
    Duration::from_millis(2_000),
];

/// Scoped-sub owner of one shared author-outbox plan slot.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AuthorOutboxPlanOwner {
    account_pubkey: Pubkey,
    scoped: ScopedSubKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AuthorOutboxPlanSlotId(u64);

/// Input snapshot that must still match before a cached plan can be reused.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AuthorOutboxPlanInputs {
    account_read_relays: HashSet<NormRelayUrl>,
    bootstrap_relays: HashSet<NormRelayUrl>,
    spec: SubConfig,
}

impl AuthorOutboxPlanInputs {
    fn new(
        account_read_relays: &HashSet<NormRelayUrl>,
        bootstrap_relays: &HashSet<NormRelayUrl>,
        spec: &SubConfig,
    ) -> Self {
        Self {
            account_read_relays: account_read_relays.clone(),
            bootstrap_relays: bootstrap_relays.clone(),
            spec: spec.clone(),
        }
    }
}

/// Frozen routed author-outbox plan for one input generation.
#[derive(Debug)]
pub(super) struct CachedAuthorOutboxPlan {
    generation: AuthorOutboxPlanGeneration,
    routes: PlannedAuthorOutboxRoutes,
}

/// One sendable filter paired to its original `SubConfig` filter index.
struct SendPlanFilter {
    filter_index: usize,
    filter: SendFilter,
}

/// Bridge-executed author-outbox plan job request.
pub(crate) struct AuthorOutboxPlanJobRequest {
    slot_id: AuthorOutboxPlanSlotId,
    build_stage: AuthorOutboxBuildStage,
    input: SendAuthorOutboxPlanJobInput,
}

impl AuthorOutboxPlanJobRequest {
    fn new(
        slot_id: AuthorOutboxPlanSlotId,
        build_stage: AuthorOutboxBuildStage,
        inputs: &AuthorOutboxPlanInputs,
    ) -> Self {
        Self {
            slot_id,
            build_stage,
            input: send_author_outbox_plan_job_input(inputs),
        }
    }

    pub(crate) fn slot_id(&self) -> u64 {
        self.slot_id.0
    }

    pub(crate) fn run(self, ndb: Ndb) -> AuthorOutboxPlanJobCompletion {
        let result = build_author_outbox_plan(ndb, self.input);
        AuthorOutboxPlanJobCompletion {
            slot_id: self.slot_id,
            build_stage: self.build_stage,
            result,
        }
    }
}

/// Completed bridge-executed author-outbox plan job.
pub(crate) struct AuthorOutboxPlanJobCompletion {
    slot_id: AuthorOutboxPlanSlotId,
    build_stage: AuthorOutboxBuildStage,
    result: SendAuthorOutboxPlanJobResult,
}

/// Owned sendable data needed by one background author-outbox plan job.
enum SendAuthorOutboxPlanJobInput {
    /// Route authors named by the configured filters.
    AuthorFilters(SendAuthorOutboxPlanConfig),
    /// Route the ancestry of the required thread note IDs.
    Thread {
        config: SendAuthorOutboxPlanConfig,
        note_ids: HashSet<NoteId>,
    },
}

/// Relay coverage and filters shared by both background plan kinds.
struct SendAuthorOutboxPlanConfig {
    account_read_relays: HashSet<NormRelayUrl>,
    live_filters: Vec<SendPlanFilter>,
    full_history_filters: Vec<SendPlanFilter>,
}

/// One sendable routed filter produced by background author-outbox planning.
struct SendPlannedRoutedRelay {
    relay: NormRelayUrl,
    relay_priority: RoutedRelayPriority,
    filters: Vec<SendFilter>,
    authors_by_filter_index: Vec<(usize, Vec<Pubkey>)>,
}

/// Completed sendable background author-outbox plan.
struct SendAuthorOutboxPlanJobResult {
    live_routed_relays: Vec<SendPlannedRoutedRelay>,
    full_history_routed_relays: Vec<SendPlannedRoutedRelay>,
    missing_authors: HashSet<Pubkey>,
    thread: Option<Result<ThreadPlanSnapshot, nostrdb::Error>>,
}

/// Current shared author-outbox plan lifecycle for one input snapshot.
struct AuthorOutboxPlanSlot {
    inputs: AuthorOutboxPlanInputs,
    owners: HashSet<AuthorOutboxPlanOwner>,
    state: AuthorOutboxPlanState,
    thread: Option<ThreadPlanState>,
}

/// Dynamic thread inputs and the last usable routes while a replacement builds.
/// Owned by the same plan slot as relay-list discovery, never by the UI.
struct ThreadPlanState {
    watch: Option<ThreadWatch>,
    /// Only failed reads or subscription setup use a timer; arrivals wake the stream.
    retry_after: Option<Instant>,
    /// Next failure's delay, doubled up to the cap and reset after recovery.
    retry_delay: Duration,
    available_plan: Option<CachedAuthorOutboxPlan>,
    baseline_fetch: MissingIdFetch,
    bootstrap_fetch: MissingIdFetch,
    /// Shared discovery implementation, kept alive across ancestry snapshots.
    discovery: Option<RelayListDiscovery>,
    requested_authors: HashSet<Pubkey>,
}

impl Default for ThreadPlanState {
    fn default() -> Self {
        Self {
            watch: None,
            retry_after: None,
            retry_delay: THREAD_PLAN_RETRY_DELAY,
            available_plan: None,
            baseline_fetch: MissingIdFetch::default(),
            bootstrap_fetch: MissingIdFetch::default(),
            discovery: None,
            requested_authors: HashSet::new(),
        }
    }
}

impl ThreadPlanState {
    /// Back off consecutive read/setup failures, including across planning jobs.
    fn record_failed_attempt(&mut self) {
        self.retry_after = Some(Instant::now() + self.retry_delay);
        self.retry_delay = self
            .retry_delay
            .saturating_mul(2)
            .min(THREAD_PLAN_RETRY_MAX);
    }

    /// Discover each newly encountered author once per thread plan lifetime.
    /// Existing discovery legs retain their normal EOSE/retry policy while
    /// arriving ancestors extend the set of authors being discovered.
    fn discover_authors(
        &mut self,
        ids: &OutboxIdRegistry,
        missing: HashSet<Pubkey>,
        relays: &HashSet<NormRelayUrl>,
    ) -> ScopedSubOutboxOps {
        if relays.is_empty() {
            return ScopedSubOutboxOps::default();
        }
        let new_authors = missing
            .into_iter()
            .filter(|author| self.requested_authors.insert(*author))
            .collect::<HashSet<_>>();
        if new_authors.is_empty() {
            return ScopedSubOutboxOps::default();
        }
        let (discovery, ops) = start_relay_list_discovery(ids, new_authors, relays.clone());
        if let Some(existing) = &mut self.discovery {
            existing.chunks.extend(discovery.chunks);
        } else {
            self.discovery = Some(discovery);
        }
        ops
    }
}

/// One exact-ID fetch retained across thread snapshots, with normal one-shot EOSE cleanup.
/// Remembering the requested IDs and relays prevents resending an unchanged fetch.
#[derive(Default)]
struct MissingIdFetch {
    missing_ids: HashSet<NoteId>,
    relays: HashSet<NormRelayUrl>,
    id: Option<OutboxSubId>,
}

impl MissingIdFetch {
    /// Replace the one-shot only when its missing IDs or destination relays change.
    fn update(
        &mut self,
        missing_ids: HashSet<NoteId>,
        relays: HashSet<NormRelayUrl>,
        ids: &OutboxIdRegistry,
        policy: RelayUrlPolicy,
    ) -> ScopedSubOutboxOps {
        let mut ops = ScopedSubOutboxOps::default();
        if self.missing_ids == missing_ids && self.relays == relays {
            return ops;
        }
        self.missing_ids = missing_ids;
        self.relays = relays;
        if let Some(id) = self.id.take() {
            ops.clear_fetch(id);
        }
        if self.missing_ids.is_empty() || self.relays.is_empty() {
            return ops;
        }
        let mut missing = self.missing_ids.iter().copied().collect::<Vec<_>>();
        missing.sort_unstable_by_key(|id| *id.bytes());
        let id = ids.next_sub_id();
        ops.start_fetch(
            id,
            vec![Filter::new().ids(missing.iter().map(NoteId::bytes)).build()],
            RelayUrlPkgs::new(self.relays.clone(), policy),
        );
        self.id = Some(id);
        ops
    }
}

enum AuthorOutboxPlanState {
    BuildingInitial,
    /// No successful initial thread snapshot yet; retry at the error deadline.
    WaitingForThreadRetry,
    DiscoveringRelays {
        discovery: RelayListDiscovery,
        original_missing_author_count: usize,
    },
    WaitingForRelayListIngestion(RelayListIngestionWait),
    BuildingAfterRelayListDiscovery {
        original_missing_author_count: usize,
        attempt: u8,
    },
    Ready(CachedAuthorOutboxPlan),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorOutboxBuildStage {
    Initial,
    AfterRelayListDiscovery {
        original_missing_author_count: usize,
        attempt: u8,
    },
}

/// Retained delay before resampling local NDB after relay-list discovery EOSE.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelayListIngestionWait {
    ready_at: Instant,
    original_missing_author_count: usize,
    attempt: u8,
}

impl RelayListIngestionWait {
    fn new(original_missing_author_count: usize, attempt: u8, now: Instant) -> Self {
        Self {
            ready_at: now + relay_list_ingestion_wait_delay(attempt),
            original_missing_author_count,
            attempt,
        }
    }
}

/// Result of advancing one author-outbox plan lifecycle.
pub(super) enum AuthorOutboxPlanAdvance<'a> {
    /// A cached plan is ready and should be realized.
    Ready {
        routes: &'a PlannedAuthorOutboxRoutes,
        generation: AuthorOutboxPlanGeneration,
    },
    /// Planning is still waiting on NDB work, relay-list discovery, or application work.
    Pending,
    /// The scoped config does not use author-outbox.
    NotAuthorOutbox,
}

pub(super) struct AuthorOutboxPlanAdvanceRequest<'a> {
    pub(super) account_pubkey: Pubkey,
    pub(super) scoped: ScopedSubKey,
    pub(super) account_read_relays: &'a HashSet<NormRelayUrl>,
    pub(super) spec: &'a SubConfig,
}

pub(super) struct AuthorOutboxPlanAdvanceResult<'a> {
    pub(super) advance: AuthorOutboxPlanAdvance<'a>,
    pub(super) pre_realization_ops: ScopedSubOutboxOps,
    pub(super) effects: ScopedSubEffects,
}

/// Shared author-outbox planner. Author-filter plans remain frozen; thread
/// plans rebuild on NDB ingestion while retaining their relay-list discovery.
pub(super) struct AuthorOutboxPlanRuntime {
    /// Injected discovery destinations, captured in each immutable plan input.
    pub(super) bootstrap_relays: HashSet<NormRelayUrl>,
    owner_slots: HashMap<AuthorOutboxPlanOwner, AuthorOutboxPlanSlotId>,
    slots: HashMap<AuthorOutboxPlanSlotId, AuthorOutboxPlanSlot>,
    next_slot_id: u64,
    next_generation: AuthorOutboxPlanGeneration,
}

impl Default for AuthorOutboxPlanRuntime {
    fn default() -> Self {
        Self {
            bootstrap_relays: HashSet::new(),
            owner_slots: HashMap::new(),
            slots: HashMap::new(),
            next_slot_id: 1,
            next_generation: 1,
        }
    }
}

impl AuthorOutboxPlanRuntime {
    /// Return the next discovery or failed-thread-plan retry deadline.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter_map(AuthorOutboxPlanSlot::next_deadline)
            .min()
    }

    /// Await a relevant NDB arrival without a timer or a task per thread.
    ///
    /// Do not consume notifications while a job is running: the subscription's
    /// queue retains them for a subsequent job. Cancelling this wait keeps the
    /// streams in their slots, so other bridge activity cannot lose arrivals.
    pub(super) async fn next_thread_change(&mut self) -> AuthorOutboxPlanSlotId {
        poll_fn(|cx| {
            for (slot_id, slot) in &mut self.slots {
                if slot.state.is_building() {
                    continue;
                }
                let Some(thread) = &mut slot.thread else {
                    continue;
                };
                let Some(watch) = &mut thread.watch else {
                    continue;
                };
                match watch.poll_next_unpin(cx) {
                    Poll::Ready(Some(())) => return Poll::Ready(*slot_id),
                    Poll::Ready(None) => {
                        // An ended stream must be replaced, never selected repeatedly.
                        thread.watch = None;
                        return Poll::Ready(*slot_id);
                    }
                    Poll::Pending => {}
                }
            }
            Poll::Pending
        })
        .await
    }

    /// Schedule one new thread plan while preserving the last completed routes.
    /// Used by NDB notifications, expanded subscription coverage, and failed-job retries.
    pub(super) fn apply_thread_change(
        &mut self,
        slot_id: AuthorOutboxPlanSlotId,
    ) -> ScopedSubEffects {
        let mut effects = ScopedSubEffects::default();
        let Some(slot) = self.slots.get_mut(&slot_id) else {
            return effects;
        };
        if slot.state.is_building() {
            return effects;
        }
        let Some(thread) = &mut slot.thread else {
            return effects;
        };
        thread.retry_after = None;
        let previous = std::mem::replace(&mut slot.state, AuthorOutboxPlanState::BuildingInitial);
        if let AuthorOutboxPlanState::Ready(plan) = previous {
            thread.available_plan = Some(plan);
        }
        effects.push(ScopedSubEffect::from(AuthorOutboxPlanJobRequest::new(
            slot_id,
            AuthorOutboxBuildStage::Initial,
            &slot.inputs,
        )));
        effects
    }

    /// Drop every cached or in-flight owner binding for one scoped subscription.
    pub(super) fn remove_scoped(&mut self, scoped: &ScopedSubKey) -> ScopedSubOutboxOps {
        let owners = self
            .owner_slots
            .keys()
            .filter(|owner| owner.scoped == *scoped)
            .cloned()
            .collect::<Vec<_>>();
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for owner in owners {
            outbox_ops.extend(self.remove_owner(&owner));
        }
        outbox_ops
    }

    /// Drop cached or in-flight plan ownership tied to a deleted account.
    pub(super) fn purge_account(&mut self, account_pubkey: Pubkey) -> ScopedSubOutboxOps {
        let owners = self
            .owner_slots
            .keys()
            .filter(|owner| owner.account_pubkey == account_pubkey)
            .cloned()
            .collect::<Vec<_>>();
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for owner in owners {
            outbox_ops.extend(self.remove_owner(&owner));
        }
        outbox_ops
    }

    /// Drop in-flight ownership tied to an inactive account while retaining
    /// completed frozen author-filter plans for fast switch-back. Thread watches
    /// belong only to the active account and are rebuilt on switch-back.
    pub(super) fn deactivate_account(&mut self, account_pubkey: Pubkey) -> ScopedSubOutboxOps {
        let owners = self
            .owner_slots
            .iter()
            .filter_map(|(owner, slot_id)| {
                if owner.account_pubkey != account_pubkey {
                    return None;
                }
                let retain = self
                    .slots
                    .get(slot_id)
                    .is_some_and(|slot| slot.thread.is_none() && slot.is_ready());
                (!retain).then_some(owner.clone())
            })
            .collect::<Vec<_>>();
        let mut outbox_ops = ScopedSubOutboxOps::default();
        for owner in owners {
            outbox_ops.extend(self.remove_owner(&owner));
        }
        outbox_ops
    }

    /// Advance author-outbox planning for one active scoped subscription.
    ///
    /// `pre_realization_ops` must be applied before realized-state ops derived
    /// from `advance`.
    pub(super) fn advance(
        &mut self,
        request: AuthorOutboxPlanAdvanceRequest<'_>,
    ) -> AuthorOutboxPlanAdvanceResult<'_> {
        let AuthorOutboxPlanAdvanceRequest {
            account_pubkey,
            scoped,
            account_read_relays,
            spec,
        } = request;
        let mut effects = ScopedSubEffects::default();
        if !spec.uses_author_outbox() {
            return AuthorOutboxPlanAdvanceResult {
                advance: AuthorOutboxPlanAdvance::NotAuthorOutbox,
                pre_realization_ops: self.remove_scoped(&scoped),
                effects,
            };
        }

        let owner = AuthorOutboxPlanOwner {
            account_pubkey,
            scoped,
        };
        let inputs = AuthorOutboxPlanInputs::new(account_read_relays, &self.bootstrap_relays, spec);
        let (slot_id, outbox_ops, slot_effects) = self.ensure_owner_slot(owner, inputs);
        effects.extend(slot_effects);
        let Some(slot_id) = slot_id else {
            return AuthorOutboxPlanAdvanceResult {
                advance: AuthorOutboxPlanAdvance::Pending,
                pre_realization_ops: outbox_ops,
                effects,
            };
        };

        if let Some(slot) = self.slots.get(&slot_id) {
            if let Some(plan) = slot.ready_plan() {
                return AuthorOutboxPlanAdvanceResult {
                    advance: AuthorOutboxPlanAdvance::Ready {
                        routes: &plan.routes,
                        generation: plan.generation,
                    },
                    pre_realization_ops: outbox_ops,
                    effects,
                };
            }
        }

        AuthorOutboxPlanAdvanceResult {
            advance: AuthorOutboxPlanAdvance::Pending,
            pre_realization_ops: outbox_ops,
            effects,
        }
    }

    fn ensure_owner_slot(
        &mut self,
        owner: AuthorOutboxPlanOwner,
        inputs: AuthorOutboxPlanInputs,
    ) -> (
        Option<AuthorOutboxPlanSlotId>,
        ScopedSubOutboxOps,
        ScopedSubEffects,
    ) {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        if let Some(existing_slot_id) = self.owner_slots.get(&owner).copied() {
            if self
                .slots
                .get(&existing_slot_id)
                .is_some_and(|slot| slot.inputs == inputs)
            {
                return (Some(existing_slot_id), outbox_ops, effects);
            }
            outbox_ops.extend(self.remove_owner(&owner));
        }

        if let Some((slot_id, slot)) = self
            .slots
            .iter_mut()
            .find(|(_, slot)| slot.inputs == inputs)
        {
            slot.owners.insert(owner.clone());
            self.owner_slots.insert(owner, *slot_id);
            return (Some(*slot_id), outbox_ops, effects);
        }

        let slot_id = self.allocate_slot_id();
        effects.push(ScopedSubEffect::from(AuthorOutboxPlanJobRequest::new(
            slot_id,
            AuthorOutboxBuildStage::Initial,
            &inputs,
        )));
        self.owner_slots.insert(owner.clone(), slot_id);
        self.slots.insert(
            slot_id,
            AuthorOutboxPlanSlot {
                thread: inputs
                    .spec
                    .thread_notes()
                    .map(|_| ThreadPlanState::default()),
                inputs,
                owners: HashSet::from([owner]),
                state: Self::building_state(AuthorOutboxBuildStage::Initial),
            },
        );
        (Some(slot_id), outbox_ops, effects)
    }

    fn remove_owner(&mut self, owner: &AuthorOutboxPlanOwner) -> ScopedSubOutboxOps {
        let Some(slot_id) = self.owner_slots.remove(owner) else {
            return ScopedSubOutboxOps::default();
        };
        let Some(slot) = self.slots.get_mut(&slot_id) else {
            return ScopedSubOutboxOps::default();
        };
        slot.owners.remove(owner);
        if !slot.owners.is_empty() {
            return ScopedSubOutboxOps::default();
        }
        let Some(slot) = self.slots.remove(&slot_id) else {
            return ScopedSubOutboxOps::default();
        };
        let mut ops = ScopedSubOutboxOps::default();
        if let Some(thread) = slot.thread {
            for fetch in [thread.baseline_fetch, thread.bootstrap_fetch] {
                if let Some(id) = fetch.id {
                    ops.clear_fetch(id);
                }
            }
            if let Some(discovery) = thread.discovery {
                ops.extend(discovery.unsubscribe_all());
            }
        }
        if let AuthorOutboxPlanState::DiscoveringRelays { discovery, .. } = slot.state {
            ops.extend(discovery.unsubscribe_all());
        }
        ops
    }

    /// Apply one relay request status fact to discovery slots that own the
    /// matching relay-list discovery leg.
    pub(super) fn apply_relay_req_status(
        &mut self,
        id: OutboxSubId,
        relay: &NormRelayUrl,
        status: Option<RelayReqStatus>,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let slot_ids = self
            .slots
            .iter()
            .filter_map(|(slot_id, slot)| {
                (matches!(slot.state, AuthorOutboxPlanState::DiscoveringRelays { .. })
                    || slot
                        .thread
                        .as_ref()
                        .is_some_and(|thread| thread.discovery.is_some()))
                .then_some(*slot_id)
            })
            .collect::<Vec<_>>();

        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        for slot_id in slot_ids {
            let (ops, slot_effects) =
                self.apply_relay_req_status_to_discovery_slot(slot_id, id, relay, status);
            outbox_ops.extend(ops);
            effects.extend(slot_effects);
        }
        (outbox_ops, effects)
    }

    /// Apply the completed background-plan wake for one retained plan slot and
    /// return owners whose scoped subscriptions can now realize the ready plan.
    pub(super) fn apply_plan_slot_ready(
        &mut self,
        ids: &OutboxIdRegistry,
        completion: AuthorOutboxPlanJobCompletion,
        account_read_relays: &HashSet<NormRelayUrl>,
        ndb: &Ndb,
    ) -> (Vec<ScopedSubKey>, ScopedSubOutboxOps, ScopedSubEffects) {
        let slot_id = completion.slot_id;
        let (outbox_ops, effects) =
            self.apply_build_result(ids, completion, account_read_relays, ndb);
        if !self.slot_is_ready(slot_id) {
            return (Vec::new(), outbox_ops, effects);
        }

        (self.slot_scoped_keys(slot_id), outbox_ops, effects)
    }

    /// Apply discovery retries, ingestion waits, and failed-thread-plan retries.
    pub(super) fn apply_relay_list_discovery_retry_due(
        &mut self,
        now: Instant,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let slot_ids = self
            .slots
            .iter()
            .filter_map(|(slot_id, slot)| {
                slot.next_deadline()
                    .is_some_and(|deadline| deadline <= now)
                    .then_some(*slot_id)
            })
            .collect::<Vec<_>>();

        let mut outbox_ops = ScopedSubOutboxOps::default();
        let mut effects = ScopedSubEffects::default();
        for slot_id in slot_ids {
            let (ops, slot_effects) = self.apply_relay_list_timer_due_to_slot(slot_id, now);
            outbox_ops.extend(ops);
            effects.extend(slot_effects);
        }
        (outbox_ops, effects)
    }

    fn apply_build_result(
        &mut self,
        ids: &OutboxIdRegistry,
        completion: AuthorOutboxPlanJobCompletion,
        account_read_relays: &HashSet<NormRelayUrl>,
        ndb: &Ndb,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let AuthorOutboxPlanJobCompletion {
            slot_id,
            build_stage,
            mut result,
        } = completion;
        let Some(slot) = self.slots.get(&slot_id) else {
            return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
        };
        if !slot.state.matches_build_stage(build_stage) {
            return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
        }

        let discovery_relays = account_read_relays
            .union(&slot.inputs.bootstrap_relays)
            .cloned()
            .collect::<HashSet<_>>();

        if let Some(snapshot) = result.thread.take() {
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    tracing::warn!(?err, "failed to read thread routing context");
                    let slot = self.slots.get_mut(&slot_id).expect("validated plan slot");
                    let thread = slot
                        .thread
                        .as_mut()
                        .expect("thread snapshot for thread inputs");
                    // Failure does not mean that ancestors or routes disappeared.
                    // Keep the prior subscription, queued arrivals, and fetches.
                    thread.record_failed_attempt();
                    slot.state = thread
                        .available_plan
                        .take()
                        .map(AuthorOutboxPlanState::Ready)
                        .unwrap_or(AuthorOutboxPlanState::WaitingForThreadRetry);
                    return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
                }
            };
            // Account reads already have an explicit exact-ID fetch. A planned
            // author route may still be unadmitted, so it cannot replace this demand.
            let bootstrap_relays = slot
                .inputs
                .bootstrap_relays
                .difference(&slot.inputs.account_read_relays)
                .cloned()
                .collect();
            let missing_authors = std::mem::take(&mut result.missing_authors);
            let cached = self.cached_plan_from_job_result(result);
            let slot = self.slots.get_mut(&slot_id).expect("validated plan slot");
            let thread = slot
                .thread
                .as_mut()
                .expect("thread snapshot for thread inputs");
            thread.retry_after = None;
            let needs_watch = thread
                .watch
                .as_ref()
                .is_none_or(|watch| !watch.covers(&snapshot.note_ids, &snapshot.authors));
            let catch_up = if needs_watch {
                match ThreadWatch::new(ndb, snapshot.note_ids, snapshot.authors) {
                    Ok(watch) => {
                        // Install before dropping the old subscription or scheduling
                        // another job. That job also catches arrivals preceding setup.
                        thread.watch = Some(watch);
                        true
                    }
                    Err(err) => {
                        tracing::warn!(?err, "failed to subscribe to thread routing changes");
                        thread.record_failed_attempt();
                        false
                    }
                }
            } else {
                false
            };
            // A successful read alone must not reset failed subscription setup.
            if thread.retry_after.is_none() {
                thread.retry_delay = THREAD_PLAN_RETRY_DELAY;
            }
            let policy = slot.inputs.spec.baseline_policy();
            let policy =
                RelayUrlPolicy::explicit(policy.demand_priority(), policy.routing_preference());
            let mut ops = thread.baseline_fetch.update(
                snapshot.missing_ids,
                slot.inputs.account_read_relays.clone(),
                ids,
                policy,
            );
            ops.extend(thread.bootstrap_fetch.update(
                snapshot.missing_ids_without_author,
                bootstrap_relays,
                ids,
                policy,
            ));
            ops.extend(thread.discover_authors(ids, missing_authors, &discovery_relays));
            thread.available_plan = None;
            slot.state = AuthorOutboxPlanState::Ready(cached);
            let effects = if catch_up {
                self.apply_thread_change(slot_id)
            } else {
                ScopedSubEffects::default()
            };
            return (ops, effects);
        }

        let slot = self.slots.get_mut(&slot_id).expect("validated plan slot");
        if build_stage == AuthorOutboxBuildStage::Initial
            && !result.missing_authors.is_empty()
            && !discovery_relays.is_empty()
        {
            let original_missing_author_count = result.missing_authors.len();
            let (discovery, outbox_ops) =
                start_relay_list_discovery(ids, result.missing_authors, discovery_relays);
            slot.state = AuthorOutboxPlanState::DiscoveringRelays {
                discovery,
                original_missing_author_count,
            };
            return (outbox_ops, ScopedSubEffects::default());
        }

        if let AuthorOutboxBuildStage::AfterRelayListDiscovery {
            original_missing_author_count,
            attempt,
        } = build_stage
        {
            if should_wait_for_more_relay_list_ingestion(
                original_missing_author_count,
                attempt,
                &result,
            ) {
                slot.state = AuthorOutboxPlanState::WaitingForRelayListIngestion(
                    RelayListIngestionWait::new(
                        original_missing_author_count,
                        attempt.saturating_add(1),
                        Instant::now(),
                    ),
                );
                return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
            }
        }

        let cached = self.cached_plan_from_job_result(result);
        if let Some(slot) = self.slots.get_mut(&slot_id) {
            slot.state = AuthorOutboxPlanState::Ready(cached);
        }
        (ScopedSubOutboxOps::default(), ScopedSubEffects::default())
    }

    fn apply_relay_req_status_to_discovery_slot(
        &mut self,
        slot_id: AuthorOutboxPlanSlotId,
        id: OutboxSubId,
        relay: &NormRelayUrl,
        status: Option<RelayReqStatus>,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let (discovery_advance, outbox_ops, original_missing_author_count) = {
            let Some(slot) = self.slots.get_mut(&slot_id) else {
                return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
            };
            if let Some(thread) = &mut slot.thread {
                let Some(discovery) = &mut thread.discovery else {
                    return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
                };
                let (advance, ops) = discovery.apply_relay_req_status(id, relay, status);
                if advance == RelayListDiscoveryAdvance::Complete {
                    thread.discovery = None;
                }
                return (ops, ScopedSubEffects::default());
            }
            let AuthorOutboxPlanState::DiscoveringRelays {
                discovery,
                original_missing_author_count,
            } = &mut slot.state
            else {
                return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
            };
            let (advance, outbox_ops) = discovery.apply_relay_req_status(id, relay, status);
            (advance, outbox_ops, *original_missing_author_count)
        };

        if discovery_advance != RelayListDiscoveryAdvance::Complete {
            return (outbox_ops, ScopedSubEffects::default());
        }

        if let Some(slot) = self.slots.get_mut(&slot_id) {
            slot.state = AuthorOutboxPlanState::WaitingForRelayListIngestion(
                RelayListIngestionWait::new(original_missing_author_count, 1, Instant::now()),
            );
        }
        (outbox_ops, ScopedSubEffects::default())
    }

    fn apply_relay_list_timer_due_to_slot(
        &mut self,
        slot_id: AuthorOutboxPlanSlotId,
        now: Instant,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let retry_thread = self.slots.get(&slot_id).is_some_and(|slot| {
            !slot.state.is_building()
                && slot
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.retry_after)
                    .is_some_and(|deadline| now >= deadline)
        });
        if retry_thread {
            return (
                ScopedSubOutboxOps::default(),
                self.apply_thread_change(slot_id),
            );
        }
        if let Some(slot) = self.slots.get_mut(&slot_id) {
            if let Some(thread) = &mut slot.thread {
                let Some(discovery) = &mut thread.discovery else {
                    return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
                };
                let (advance, ops) = discovery.apply_retry_due(now);
                if advance == RelayListDiscoveryAdvance::Complete {
                    thread.discovery = None;
                }
                return (ops, ScopedSubEffects::default());
            }
        }
        match self.slots.get(&slot_id).map(|slot| &slot.state) {
            Some(AuthorOutboxPlanState::DiscoveringRelays { .. }) => {
                self.apply_discovery_retry_due_to_slot(slot_id, now)
            }
            Some(AuthorOutboxPlanState::WaitingForRelayListIngestion(_)) => (
                ScopedSubOutboxOps::default(),
                self.apply_relay_list_ingestion_wait_due_to_slot(slot_id, now),
            ),
            _ => (ScopedSubOutboxOps::default(), ScopedSubEffects::default()),
        }
    }

    fn apply_relay_list_ingestion_wait_due_to_slot(
        &mut self,
        slot_id: AuthorOutboxPlanSlotId,
        now: Instant,
    ) -> ScopedSubEffects {
        let Some((inputs, wait)) = self.slots.get(&slot_id).and_then(|slot| {
            let AuthorOutboxPlanState::WaitingForRelayListIngestion(wait) = slot.state else {
                return None;
            };
            (now >= wait.ready_at).then(|| (slot.inputs.clone(), wait))
        }) else {
            return ScopedSubEffects::default();
        };

        let build_stage = AuthorOutboxBuildStage::AfterRelayListDiscovery {
            original_missing_author_count: wait.original_missing_author_count,
            attempt: wait.attempt,
        };
        let mut effects = ScopedSubEffects::default();
        effects.push(ScopedSubEffect::from(AuthorOutboxPlanJobRequest::new(
            slot_id,
            build_stage,
            &inputs,
        )));
        if let Some(slot) = self.slots.get_mut(&slot_id) {
            slot.state = Self::building_state(build_stage);
        }
        effects
    }

    fn apply_discovery_retry_due_to_slot(
        &mut self,
        slot_id: AuthorOutboxPlanSlotId,
        now: Instant,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let (discovery_advance, outbox_ops, original_missing_author_count) = {
            let Some(slot) = self.slots.get_mut(&slot_id) else {
                return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
            };
            let AuthorOutboxPlanState::DiscoveringRelays {
                discovery,
                original_missing_author_count,
            } = &mut slot.state
            else {
                return (ScopedSubOutboxOps::default(), ScopedSubEffects::default());
            };
            let (advance, outbox_ops) = discovery.apply_retry_due(now);
            (advance, outbox_ops, *original_missing_author_count)
        };

        if discovery_advance != RelayListDiscoveryAdvance::Complete {
            return (outbox_ops, ScopedSubEffects::default());
        }

        if let Some(slot) = self.slots.get_mut(&slot_id) {
            slot.state = AuthorOutboxPlanState::WaitingForRelayListIngestion(
                RelayListIngestionWait::new(original_missing_author_count, 1, now),
            );
        }
        (outbox_ops, ScopedSubEffects::default())
    }

    fn cached_plan_from_job_result(
        &mut self,
        result: SendAuthorOutboxPlanJobResult,
    ) -> CachedAuthorOutboxPlan {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        CachedAuthorOutboxPlan {
            generation,
            routes: planned_author_outbox_routes_from_job_result(result),
        }
    }

    fn slot_is_ready(&self, slot_id: AuthorOutboxPlanSlotId) -> bool {
        self.slots
            .get(&slot_id)
            .is_some_and(AuthorOutboxPlanSlot::is_ready)
    }

    fn building_state(build_stage: AuthorOutboxBuildStage) -> AuthorOutboxPlanState {
        match build_stage {
            AuthorOutboxBuildStage::Initial => AuthorOutboxPlanState::BuildingInitial,
            AuthorOutboxBuildStage::AfterRelayListDiscovery {
                original_missing_author_count,
                attempt,
            } => AuthorOutboxPlanState::BuildingAfterRelayListDiscovery {
                original_missing_author_count,
                attempt,
            },
        }
    }

    fn allocate_slot_id(&mut self) -> AuthorOutboxPlanSlotId {
        let slot_id = AuthorOutboxPlanSlotId(self.next_slot_id);
        self.next_slot_id = self.next_slot_id.wrapping_add(1).max(1);
        slot_id
    }

    fn slot_scoped_keys(&self, slot_id: AuthorOutboxPlanSlotId) -> Vec<ScopedSubKey> {
        self.slots
            .get(&slot_id)
            .map(|slot| {
                slot.owners
                    .iter()
                    .map(|owner| owner.scoped.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl AuthorOutboxPlanSlot {
    /// Keep usable thread routes available during the next background rebuild.
    fn ready_plan(&self) -> Option<&CachedAuthorOutboxPlan> {
        if let AuthorOutboxPlanState::Ready(plan) = &self.state {
            return Some(plan);
        }
        self.thread.as_ref()?.available_plan.as_ref()
    }

    fn is_ready(&self) -> bool {
        self.ready_plan().is_some()
    }

    fn next_deadline(&self) -> Option<Instant> {
        let discovery = match &self.state {
            AuthorOutboxPlanState::DiscoveringRelays { discovery, .. } => discovery.next_deadline(),
            AuthorOutboxPlanState::WaitingForRelayListIngestion(wait) => Some(wait.ready_at),
            AuthorOutboxPlanState::BuildingInitial
            | AuthorOutboxPlanState::WaitingForThreadRetry
            | AuthorOutboxPlanState::BuildingAfterRelayListDiscovery { .. }
            | AuthorOutboxPlanState::Ready(_) => None,
        };
        let retry = self
            .thread
            .as_ref()
            .filter(|_| !self.state.is_building())
            .and_then(|thread| thread.retry_after);
        let thread_discovery = self
            .thread
            .as_ref()
            .and_then(|thread| thread.discovery.as_ref())
            .and_then(RelayListDiscovery::next_deadline);
        discovery
            .into_iter()
            .chain(retry)
            .chain(thread_discovery)
            .min()
    }
}

impl AuthorOutboxPlanState {
    fn is_building(&self) -> bool {
        matches!(
            self,
            Self::BuildingInitial | Self::BuildingAfterRelayListDiscovery { .. }
        )
    }

    fn matches_build_stage(&self, build_stage: AuthorOutboxBuildStage) -> bool {
        match (self, build_stage) {
            (AuthorOutboxPlanState::BuildingInitial, AuthorOutboxBuildStage::Initial) => true,
            (
                AuthorOutboxPlanState::BuildingAfterRelayListDiscovery {
                    original_missing_author_count: state_count,
                    attempt: state_attempt,
                },
                AuthorOutboxBuildStage::AfterRelayListDiscovery {
                    original_missing_author_count: stage_count,
                    attempt: stage_attempt,
                },
            ) => *state_count == stage_count && *state_attempt == stage_attempt,
            _ => false,
        }
    }
}

fn should_wait_for_more_relay_list_ingestion(
    original_missing_author_count: usize,
    attempt: u8,
    result: &SendAuthorOutboxPlanJobResult,
) -> bool {
    if usize::from(attempt) >= RELAY_LIST_INGESTION_WAIT_DELAYS.len() {
        return false;
    }
    original_missing_author_count > 0
        && result.missing_authors.len() >= original_missing_author_count
}

fn relay_list_ingestion_wait_delay(attempt: u8) -> Duration {
    let index = usize::from(attempt.saturating_sub(1));
    RELAY_LIST_INGESTION_WAIT_DELAYS
        .get(index)
        .copied()
        .unwrap_or(
            *RELAY_LIST_INGESTION_WAIT_DELAYS
                .last()
                .expect("wait delays"),
        )
}

fn send_author_outbox_plan_job_input(
    inputs: &AuthorOutboxPlanInputs,
) -> SendAuthorOutboxPlanJobInput {
    let config = SendAuthorOutboxPlanConfig {
        account_read_relays: inputs.account_read_relays.clone(),
        live_filters: send_plan_filters(inputs.spec.filters()),
        full_history_filters: inputs
            .spec
            .full_history_config()
            .map(|full_history| send_plan_filters(full_history.filters()))
            .unwrap_or_default(),
    };
    match inputs.spec.thread_notes() {
        Some(note_ids) => SendAuthorOutboxPlanJobInput::Thread {
            config,
            note_ids: note_ids.clone(),
        },
        None => SendAuthorOutboxPlanJobInput::AuthorFilters(config),
    }
}

fn send_plan_filters(filters: &[SendFilter]) -> Vec<SendPlanFilter> {
    filters
        .iter()
        .enumerate()
        .map(|(filter_index, filter)| SendPlanFilter {
            filter_index,
            filter: filter.clone(),
        })
        .collect()
}

fn build_author_outbox_plan(
    ndb: Ndb,
    input: SendAuthorOutboxPlanJobInput,
) -> SendAuthorOutboxPlanJobResult {
    match input {
        SendAuthorOutboxPlanJobInput::AuthorFilters(config) => {
            build_author_filter_plan(ndb, config)
        }
        SendAuthorOutboxPlanJobInput::Thread { config, note_ids } => {
            build_thread_plan(ndb, config, note_ids)
        }
    }
}

/// Build routes for the authors named by the configured live and history filters.
fn build_author_filter_plan(
    ndb: Ndb,
    input: SendAuthorOutboxPlanConfig,
) -> SendAuthorOutboxPlanJobResult {
    let authors = send_plan_filter_authors(&input.live_filters, &input.full_history_filters);
    let directory = RelayDirectorySnapshot::from_ndb_authors(&ndb, &authors);
    let missing_authors = directory.missing_authors(&authors);
    let routes = PlannedAuthorOutboxRoutes::from_routed_filters(
        plan_send_filters(&input.live_filters, &directory, &input.account_read_relays),
        plan_send_filters(
            &input.full_history_filters,
            &directory,
            &input.account_read_relays,
        ),
    );
    SendAuthorOutboxPlanJobResult {
        thread: None,
        live_routed_relays: send_routed_relays(routes.live_routed_relays),
        full_history_routed_relays: send_routed_relays(routes.full_history_routed_relays),
        missing_authors,
    }
}

fn send_plan_filter_authors<'a>(
    live_filters: impl IntoIterator<Item = &'a SendPlanFilter>,
    full_history_filters: impl IntoIterator<Item = &'a SendPlanFilter>,
) -> HashSet<Pubkey> {
    live_filters
        .into_iter()
        .chain(full_history_filters)
        .flat_map(|filter| filter_author_pubkeys(filter.filter.as_filter()))
        .collect()
}

fn plan_send_filters(
    filters: &[SendPlanFilter],
    directory: &RelayDirectorySnapshot,
    account_read_relays: &HashSet<NormRelayUrl>,
) -> Vec<RoutedFilter> {
    plan_author_outbox_augmentation_for_indexed_filters(
        filters
            .iter()
            .map(|filter| (filter.filter_index, filter.filter.as_filter())),
        directory,
        account_read_relays,
    )
}

fn send_routed_relays(routes: Vec<PlannedRoutedRelay>) -> Vec<SendPlannedRoutedRelay> {
    routes
        .into_iter()
        .map(|route| SendPlannedRoutedRelay {
            relay: route.relay,
            relay_priority: route.relay_priority,
            filters: route
                .filters
                .into_iter()
                .map(|filter| {
                    SendFilter::try_from_filter(filter)
                        .expect("routed author-outbox filter should be sendable")
                })
                .collect(),
            authors_by_filter_index: route
                .authors_by_filter_index
                .into_iter()
                .map(|(filter_index, authors)| (filter_index, authors.into_iter().collect()))
                .collect(),
        })
        .collect()
}

fn planned_author_outbox_routes_from_job_result(
    result: SendAuthorOutboxPlanJobResult,
) -> PlannedAuthorOutboxRoutes {
    PlannedAuthorOutboxRoutes {
        live_routed_relays: result
            .live_routed_relays
            .into_iter()
            .map(planned_routed_relay_from_send)
            .collect(),
        full_history_routed_relays: result
            .full_history_routed_relays
            .into_iter()
            .map(planned_routed_relay_from_send)
            .collect(),
    }
}

fn planned_routed_relay_from_send(route: SendPlannedRoutedRelay) -> PlannedRoutedRelay {
    PlannedRoutedRelay {
        relay: route.relay,
        relay_priority: route.relay_priority,
        filters: route
            .filters
            .into_iter()
            .map(SendFilter::into_filter)
            .collect(),
        authors_by_filter_index: route
            .authors_by_filter_index
            .into_iter()
            .map(|(filter_index, authors)| (filter_index, authors.into_iter().collect()))
            .collect(),
    }
}

#[test]
fn thread_plan_job_only_builds_a_snapshot_without_subscribing() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().expect("path"), &nostrdb::Config::new()).expect("ndb");
    let missing = NoteId::new([42; 32]);
    let spec = SubConfig::builder(vec![nostrdb::Filter::new().ids([missing.bytes()]).build()])
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .for_thread(missing, [])
        .build();
    let inputs = AuthorOutboxPlanInputs::new(&HashSet::new(), &HashSet::new(), &spec);
    let job = AuthorOutboxPlanJobRequest::new(
        AuthorOutboxPlanSlotId(1),
        AuthorOutboxBuildStage::Initial,
        &inputs,
    );

    let completion = job.run(ndb.clone());

    assert_eq!(
        ndb.subscription_count(),
        0,
        "a planning job must return its snapshot without creating a subscription"
    );
    assert_eq!(
        completion
            .result
            .thread
            .expect("thread result")
            .expect("thread snapshot")
            .missing_ids,
        HashSet::from([missing])
    );
}

#[test]
fn thread_plan_reuses_partial_routes_rebuilds_on_ingestion_and_cleans_up_owners() {
    use super::config::{ResolvedSubScope, SubKey};
    use super::ScopedSubOutboxOp;
    use crate::test_utils::{nip65_write_relay_note_for_test, wait_for_nip65_for_test};
    use enostr::FullKeypair;
    use futures_util::FutureExt;
    use nostrdb::{Config, NoteBuilder, Transaction};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ndb = Ndb::new(tmp.path().to_str().unwrap(), &Config::new()).expect("ndb");
    let author = FullKeypair::generate();
    let parent = NoteId::new([12; 32]);
    let selected = NoteBuilder::new()
        .kind(1)
        .created_at(1)
        .content("reply")
        .start_tag()
        .tag_str("e")
        .tag_id(parent.bytes())
        .tag_str("wss://hint.example.com/inbox")
        .tag_str("reply")
        .sign(&author.secret_key.secret_bytes())
        .build()
        .expect("note");
    ndb.process_client_event(&selected.json().unwrap())
        .expect("ingest");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let txn = Transaction::new(&ndb).unwrap();
        if ndb.get_note_by_id(&txn, selected.id()).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "note not ingested");
        std::thread::sleep(Duration::from_millis(5));
    }
    let selected_id = NoteId::new(*selected.id());
    let spec = SubConfig::builder(vec![Filter::new().ids([selected.id()]).build()])
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .for_thread(selected_id, [])
        .build();
    let reads = HashSet::from([NormRelayUrl::new("wss://account.example.com").unwrap()]);
    let scoped = ScopedSubKey {
        scope: ResolvedSubScope::Global,
        key: SubKey::new("thread"),
    };
    let second = ScopedSubKey {
        scope: ResolvedSubScope::Global,
        key: SubKey::new("second"),
    };
    let account = Pubkey::new([1; 32]);
    let ids = OutboxIdRegistry::new();
    let mut runtime = AuthorOutboxPlanRuntime::default();
    let request = |scoped| AuthorOutboxPlanAdvanceRequest {
        account_pubkey: account,
        scoped,
        account_read_relays: &reads,
        spec: &spec,
    };
    let initial = runtime.advance(request(scoped.clone()));
    assert!(matches!(initial.advance, AuthorOutboxPlanAdvance::Pending));
    let mut jobs = initial.effects.into_effects();
    assert_eq!(jobs.len(), 1);
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().unwrap();
    let completion = job.run(ndb.clone());
    assert_eq!(ndb.subscription_count(), 0);
    let (owners, ops, effects) = runtime.apply_plan_slot_ready(&ids, completion, &reads, &ndb);
    assert_eq!(ndb.subscription_count(), 1);
    assert_eq!(
        owners,
        vec![scoped.clone()],
        "hint route is ready before relay-list EOSE"
    );
    assert!(ops.into_ops().iter().any(|op| matches!(op, ScopedSubOutboxOp::StartFetch { filters, .. }
        if filters.iter().any(|filter| filter.same_canonical_attributes(&Filter::new().ids([parent.bytes()]).build())))));
    assert!(runtime
        .slots
        .values()
        .next()
        .unwrap()
        .thread
        .as_ref()
        .unwrap()
        .discovery
        .is_some());
    let mut jobs = effects.into_effects();
    assert_eq!(jobs.len(), 1, "first subscription requires a catch-up job");
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().unwrap();
    let (_, ops, effects) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    assert!(ops.is_empty(), "catch-up retains the baseline fetch");
    assert!(
        effects.into_effects().is_empty(),
        "unchanged coverage is ready"
    );
    let shared = runtime.advance(request(second.clone()));
    assert!(matches!(
        shared.advance,
        AuthorOutboxPlanAdvance::Ready { .. }
    ));
    assert!(shared.effects.into_effects().is_empty());
    assert_eq!(runtime.slots.len(), 1);
    assert!(runtime.remove_scoped(&scoped).is_empty());
    assert_eq!(ndb.subscription_count(), 1, "other owner retains the watch");
    assert!(runtime.next_thread_change().now_or_never().is_none());

    let list = nip65_write_relay_note_for_test(&author, &["wss://author.example.com"]);
    ndb.process_client_event(&list.json().unwrap())
        .expect("relay list ingestion");
    wait_for_nip65_for_test(&ndb, &author.pubkey);
    let changed = runtime
        .next_thread_change()
        .now_or_never()
        .expect("relay-list notification");
    let effects = runtime.apply_thread_change(changed);
    let mut jobs = effects.into_effects();
    assert_eq!(jobs.len(), 1, "new relay list rebuilds without EOSE");
    assert!(
        runtime
            .apply_thread_change(changed)
            .into_effects()
            .is_empty(),
        "one snapshot job at a time"
    );
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().unwrap();
    let (owners, ops, effects) =
        runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
    assert!(effects.into_effects().is_empty());
    assert_eq!(owners, vec![second.clone()]);
    assert!(
        ops.is_empty(),
        "unchanged missing IDs do not restart the baseline fetch"
    );
    assert_eq!(
        ndb.subscription_count(),
        1,
        "unchanged coverage retains the existing watch"
    );
    let slot = runtime.slots.values().next().unwrap();
    let routes = &slot.ready_plan().unwrap().routes.live_routed_relays;
    assert!(routes
        .iter()
        .any(|route| route.relay.as_str() == "wss://author.example.com/"));
    assert!(routes
        .iter()
        .any(|route| route.relay.as_str() == "wss://hint.example.com/inbox"));
    let cleanup = runtime.deactivate_account(account).into_ops();
    assert!(cleanup
        .iter()
        .any(|op| matches!(op, ScopedSubOutboxOp::ClearFetch { .. })));
    assert!(runtime.slots.is_empty());
    assert_eq!(ndb.subscription_count(), 0);

    // A job finishing after the last owner closes cannot resurrect subscriptions.
    let mut jobs = runtime
        .advance(request(second.clone()))
        .effects
        .into_effects();
    let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = jobs.pop().unwrap();
    let completion = job.run(ndb.clone());
    runtime.remove_scoped(&second);
    assert!(runtime
        .apply_plan_slot_ready(&ids, completion, &reads, &ndb)
        .0
        .is_empty());
    assert_eq!(ndb.subscription_count(), 0);
}

#[cfg(test)]
mod tests {
    use super::discovery::{
        start_relay_list_discovery, RelayListDiscovery, RelayListDiscoveryAdvance,
        RELAY_LIST_DISCOVERY_AUTHORS_PER_REQ, RELAY_LIST_DISCOVERY_EOSE_GRACE,
    };
    use super::*;
    use crate::scoped_subs::ScopedSubOutboxOp;
    use crate::test_utils::RemoteOutboxReadModelHarness;
    use crate::test_utils::{nip65_write_relay_note_for_test, wait_for_nip65_for_test};
    use enostr::{FullKeypair, RelayDemandPriority, RelayReqStatus, RelayRoutingPreference};
    use nostrdb::{Config, Filter, Ndb};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    use super::super::config::{ResolvedSubScope, SubKey, SubRelayPolicy};

    #[derive(Debug, Eq, PartialEq)]
    enum TestAdvance {
        Ready {
            generation: AuthorOutboxPlanGeneration,
            live_routes: usize,
            full_history_routes: usize,
        },
        Pending,
        NotAuthorOutbox,
    }

    fn test_pubkey(index: u16) -> Pubkey {
        let mut bytes = [0; 32];
        bytes[0] = (index >> 8) as u8;
        bytes[1] = index as u8;
        Pubkey::new(bytes)
    }

    fn new_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb = Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
        (tmp, ndb)
    }

    fn runtime_with_ndb(ndb: &Ndb) -> AuthorOutboxPlanRuntime {
        let _ = ndb;
        AuthorOutboxPlanRuntime::default()
    }

    fn author_outbox_config(author: Pubkey) -> SubConfig {
        let baseline = SubRelayPolicy::new(
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        );
        let author_outbox = SubRelayPolicy::new(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        );

        SubConfig::builder(vec![Filter::new()
            .authors([author.bytes()])
            .kinds([1])
            .limit(20)
            .build()])
        .accounts_read(baseline)
        .with_author_outbox(author_outbox)
        .build()
    }

    fn scoped_key(key: &str) -> ScopedSubKey {
        ScopedSubKey {
            scope: ResolvedSubScope::Global,
            key: SubKey::new(key),
        }
    }

    fn advance_state(
        runtime: &mut AuthorOutboxPlanRuntime,
        bridge: &mut RemoteOutboxReadModelHarness,
        account: Pubkey,
        scoped: &ScopedSubKey,
        account_read_relays: &HashSet<NormRelayUrl>,
        spec: &SubConfig,
        ndb: &Ndb,
    ) -> TestAdvance {
        let (advance, effects) = bridge.with_returned_outbox(|_| {
            let advance_result = runtime.advance(AuthorOutboxPlanAdvanceRequest {
                account_pubkey: account,
                scoped: scoped.clone(),
                account_read_relays,
                spec,
            });
            let result = match advance_result.advance {
                AuthorOutboxPlanAdvance::Ready { routes, generation } => TestAdvance::Ready {
                    generation,
                    live_routes: routes.live_routed_relays.len(),
                    full_history_routes: routes.full_history_routed_relays.len(),
                },
                AuthorOutboxPlanAdvance::Pending => TestAdvance::Pending,
                AuthorOutboxPlanAdvance::NotAuthorOutbox => TestAdvance::NotAuthorOutbox,
            };
            (
                result,
                advance_result.pre_realization_ops,
                advance_result.effects,
            )
        });
        apply_author_outbox_effects_for_test(runtime, bridge, account_read_relays, ndb, effects);
        advance
    }

    fn apply_author_outbox_effects_for_test(
        runtime: &mut AuthorOutboxPlanRuntime,
        bridge: &mut RemoteOutboxReadModelHarness,
        account_read_relays: &HashSet<NormRelayUrl>,
        ndb: &Ndb,
        effects: ScopedSubEffects,
    ) {
        for effect in effects.into_effects() {
            match effect {
                ScopedSubEffect::StartAuthorOutboxPlanJob(request) => {
                    let completion = request.run(ndb.clone());
                    let next_effects = bridge.with_returned_outbox(|ids| {
                        let (_, outbox_ops, next_effects) = runtime.apply_plan_slot_ready(
                            ids,
                            completion,
                            account_read_relays,
                            ndb,
                        );
                        (next_effects, outbox_ops)
                    });
                    apply_author_outbox_effects_for_test(
                        runtime,
                        bridge,
                        account_read_relays,
                        ndb,
                        next_effects,
                    );
                }
            }
        }
    }

    fn advance_until_ready(
        runtime: &mut AuthorOutboxPlanRuntime,
        bridge: &mut RemoteOutboxReadModelHarness,
        account: Pubkey,
        scoped: &ScopedSubKey,
        account_read_relays: &HashSet<NormRelayUrl>,
        spec: &SubConfig,
        ndb: &Ndb,
    ) -> TestAdvance {
        let mut last = TestAdvance::Pending;
        for _ in 0..8 {
            last = advance_state(
                runtime,
                bridge,
                account,
                scoped,
                account_read_relays,
                spec,
                ndb,
            );
            if matches!(last, TestAdvance::Ready { .. }) {
                return last;
            }
        }
        last
    }

    fn discovery_mut<'a>(
        runtime: &'a mut AuthorOutboxPlanRuntime,
        account_pubkey: Pubkey,
        scoped: &ScopedSubKey,
    ) -> &'a mut RelayListDiscovery {
        let owner = AuthorOutboxPlanOwner {
            account_pubkey,
            scoped: scoped.clone(),
        };
        let slot_id = *runtime
            .owner_slots
            .get(&owner)
            .expect("owner should have a plan slot");
        let slot = runtime.slots.get_mut(&slot_id).expect("plan slot");
        let AuthorOutboxPlanState::DiscoveringRelays { discovery, .. } = &mut slot.state else {
            panic!("expected retained relay-list discovery");
        };
        discovery
    }

    fn force_ingestion_wait_due(
        runtime: &mut AuthorOutboxPlanRuntime,
        account_pubkey: Pubkey,
        scoped: &ScopedSubKey,
    ) {
        let owner = AuthorOutboxPlanOwner {
            account_pubkey,
            scoped: scoped.clone(),
        };
        let slot_id = *runtime
            .owner_slots
            .get(&owner)
            .expect("owner should have a plan slot");
        let slot = runtime.slots.get_mut(&slot_id).expect("plan slot");
        let AuthorOutboxPlanState::WaitingForRelayListIngestion(wait) = &mut slot.state else {
            panic!("expected relay-list ingestion wait");
        };
        wait.ready_at = Instant::now() - Duration::from_millis(1);
    }

    fn ingestion_wait_attempt(
        runtime: &AuthorOutboxPlanRuntime,
        account_pubkey: Pubkey,
        scoped: &ScopedSubKey,
    ) -> u8 {
        let owner = AuthorOutboxPlanOwner {
            account_pubkey,
            scoped: scoped.clone(),
        };
        let slot_id = *runtime
            .owner_slots
            .get(&owner)
            .expect("owner should have a plan slot");
        let slot = runtime.slots.get(&slot_id).expect("plan slot");
        let AuthorOutboxPlanState::WaitingForRelayListIngestion(wait) = &slot.state else {
            panic!("expected relay-list ingestion wait");
        };
        wait.attempt
    }

    fn start_discovery_for_test(
        bridge: &mut RemoteOutboxReadModelHarness,
        authors: HashSet<Pubkey>,
        relays: HashSet<NormRelayUrl>,
    ) -> RelayListDiscovery {
        bridge.with_returned_outbox(|ids| start_relay_list_discovery(ids, authors, relays))
    }

    #[test]
    fn build_author_outbox_plan_reads_local_nip65_snapshot() {
        let (_tmp, ndb) = new_ndb();
        let author = FullKeypair::generate();
        let relay_url = "wss://author-a.example.com";
        let relay = NormRelayUrl::new(relay_url).expect("relay");
        let note = nip65_write_relay_note_for_test(&author, &[relay_url]);
        ndb.process_client_event(&note.json().expect("json"))
            .expect("ingest nip65");
        wait_for_nip65_for_test(&ndb, &author.pubkey);
        let filter = Filter::new()
            .authors([author.pubkey.bytes()])
            .kinds([1])
            .build();
        let filter = SendFilter::try_from_filter(filter).expect("sendable test filter");
        let input = SendAuthorOutboxPlanJobInput::AuthorFilters(SendAuthorOutboxPlanConfig {
            account_read_relays: HashSet::new(),
            live_filters: send_plan_filters(&[filter]),
            full_history_filters: Vec::new(),
        });

        let result = build_author_outbox_plan(ndb, input);

        assert!(result.missing_authors.is_empty());
        assert_eq!(result.live_routed_relays.len(), 1);
        assert_eq!(result.live_routed_relays[0].relay, relay);
    }

    #[test]
    fn pending_author_starts_retained_relay_list_discovery() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = test_pubkey(0xA1);
        let account_relay =
            NormRelayUrl::new("wss://account-read.example.com").expect("account relay");
        let account_read_relays = HashSet::from([account_relay.clone()]);
        let scoped = scoped_key("author-plan-discovery");
        let spec = author_outbox_config(author);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        assert_eq!(
            advance_state(
                &mut runtime,
                &mut bridge,
                account,
                &scoped,
                &account_read_relays,
                &spec,
                &ndb,
            ),
            TestAdvance::Pending
        );

        let discovery = discovery_mut(&mut runtime, account, &scoped);
        assert_eq!(discovery.chunks.len(), 1);
        assert_eq!(discovery.chunks[0].legs.len(), 1);
        let leg = &discovery.chunks[0].legs[0];
        assert_eq!(leg.relay, account_relay);
        assert!(leg.id.is_some());
        assert_eq!(discovery.chunks[0].authors_for_test(), vec![author]);
    }

    /// Metadata discovery adds injected bootstrap coverage without changing event reads.
    #[test]
    fn author_filter_discovery_adds_bootstrap_relays() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(1);
        let author = test_pubkey(2);
        let read = NormRelayUrl::new("wss://account-read.example.com").expect("read relay");
        let bootstrap = NormRelayUrl::new("wss://bootstrap.example.com").expect("bootstrap");
        let reads = HashSet::from([read.clone()]);
        let expected = HashSet::from([read.clone(), bootstrap.clone()]);
        let mut runtime = runtime_with_ndb(&ndb);
        runtime.bootstrap_relays = expected.clone();
        let ids = OutboxIdRegistry::new();
        let spec = author_outbox_config(author);
        let initial = runtime.advance(AuthorOutboxPlanAdvanceRequest {
            account_pubkey: account,
            scoped: scoped_key("author-discovery-bootstrap"),
            account_read_relays: &reads,
            spec: &spec,
        });
        let mut effects = initial.effects.into_effects();
        assert_eq!(effects.len(), 1);
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = effects.pop().expect("job");
        runtime.bootstrap_relays.clear();
        let (_, ops, _) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
        let expected_filter = Filter::new()
            .authors([author.bytes()])
            .kinds([10002])
            .build();
        let mut destinations = HashSet::new();
        let ops = ops.into_ops();
        assert_eq!(
            ops.len(),
            expected.len(),
            "discovery retains its input snapshot and deduplicates overlapping relays"
        );
        for op in ops {
            let ScopedSubOutboxOp::StartFetch {
                filters,
                relay_pkgs,
                ..
            } = op
            else {
                panic!("discovery should stage a one-shot");
            };
            assert_eq!(filters.len(), 1);
            assert!(filters[0].same_canonical_attributes(&expected_filter));
            destinations.extend(relay_pkgs.urls().iter().cloned());
        }
        assert_eq!(destinations, expected);
        assert_eq!(reads, HashSet::from([read]));
    }

    /// Empty account reads must not prevent configured metadata discovery.
    #[test]
    fn author_filter_discovery_uses_bootstrap_without_account_reads() {
        let (_tmp, ndb) = new_ndb();
        let author = test_pubkey(2);
        let reads = HashSet::new();
        let bootstrap = NormRelayUrl::new("wss://bootstrap.example.com").expect("bootstrap");
        let mut runtime = runtime_with_ndb(&ndb);
        runtime.bootstrap_relays = HashSet::from([bootstrap.clone()]);
        let spec = author_outbox_config(author);
        let initial = runtime.advance(AuthorOutboxPlanAdvanceRequest {
            account_pubkey: test_pubkey(1),
            scoped: scoped_key("bootstrap-only-discovery"),
            account_read_relays: &reads,
            spec: &spec,
        });
        let mut effects = initial.effects.into_effects();
        assert_eq!(effects.len(), 1);
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = effects.pop().expect("job");
        let (_, ops, _) = runtime.apply_plan_slot_ready(
            &OutboxIdRegistry::new(),
            job.run(ndb.clone()),
            &reads,
            &ndb,
        );
        let mut ops = ops.into_ops();
        assert_eq!(ops.len(), 1);
        let ScopedSubOutboxOp::StartFetch {
            filters,
            relay_pkgs,
            ..
        } = ops.pop().unwrap()
        else {
            panic!("metadata discovery should stage a one-shot");
        };
        assert_eq!(relay_pkgs.urls(), &HashSet::from([bootstrap]));
        assert_eq!(filters.len(), 1);
        assert!(filters[0].same_canonical_attributes(
            &Filter::new()
                .authors([author.bytes()])
                .kinds([10002])
                .build()
        ));
    }

    /// A missing thread parent discovers its author's list on reads plus bootstrap,
    /// while the ordinary parent one-shot remains on selected-account reads.
    #[test]
    fn thread_discovery_adds_bootstrap_without_expanding_parent_reads() {
        use nostrdb::{NoteBuilder, Transaction};

        let (_tmp, ndb) = new_ndb();
        let author = FullKeypair::generate();
        let parent = NoteId::new([42; 32]);
        let reply = NoteBuilder::new()
            .kind(1)
            .created_at(1)
            .content("reply to a missing parent")
            .start_tag()
            .tag_str("e")
            .tag_id(parent.bytes())
            .tag_str("")
            .tag_str("reply")
            .tag_id(author.pubkey.bytes())
            .sign(&author.secret_key.secret_bytes())
            .build()
            .expect("reply");
        ndb.process_client_event(&reply.json().expect("reply JSON"))
            .expect("ingest reply");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(&ndb).expect("transaction");
            if ndb.get_note_by_id(&txn, reply.id()).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "reply was not ingested");
            std::thread::sleep(Duration::from_millis(5));
        }
        let read = NormRelayUrl::new("wss://account-read.example.com").expect("read relay");
        let bootstrap = NormRelayUrl::new("wss://bootstrap.example.com").expect("bootstrap");
        let reads = HashSet::from([read.clone()]);
        let expected_discovery = HashSet::from([read, bootstrap.clone()]);
        let parent_filter = Filter::new().ids([parent.bytes()]).build();
        let spec = SubConfig::builder(vec![parent_filter.clone()])
            .accounts_read_important()
            .with_author_outbox_augmentation()
            .for_thread(parent, [NoteId::new(*reply.id())])
            .build();
        let mut runtime = runtime_with_ndb(&ndb);
        runtime.bootstrap_relays = HashSet::from([bootstrap]);
        let initial = runtime.advance(AuthorOutboxPlanAdvanceRequest {
            account_pubkey: test_pubkey(1),
            scoped: scoped_key("thread-discovery-bootstrap"),
            account_read_relays: &reads,
            spec: &spec,
        });
        let mut effects = initial.effects.into_effects();
        assert_eq!(effects.len(), 1);
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = effects.pop().expect("job");
        let (_, ops, _) = runtime.apply_plan_slot_ready(
            &OutboxIdRegistry::new(),
            job.run(ndb.clone()),
            &reads,
            &ndb,
        );
        let relay_list_filter = Filter::new()
            .authors([author.pubkey.bytes()])
            .kinds([10002])
            .build();
        let mut discovery_destinations = HashSet::new();
        let mut parent_destinations = HashSet::new();
        let mut discovery_count = 0;
        for op in ops.into_ops() {
            let ScopedSubOutboxOp::StartFetch {
                filters,
                relay_pkgs,
                ..
            } = op
            else {
                panic!("initial thread discovery should stage one-shots");
            };
            assert_eq!(filters.len(), 1);
            if filters[0].same_canonical_attributes(&relay_list_filter) {
                discovery_count += 1;
                discovery_destinations.extend(relay_pkgs.urls().iter().cloned());
            } else {
                assert!(filters[0].same_canonical_attributes(&parent_filter));
                parent_destinations.extend(relay_pkgs.urls().iter().cloned());
            }
        }
        assert_eq!(discovery_count, expected_discovery.len());
        assert_eq!(discovery_destinations, expected_discovery);
        assert_eq!(parent_destinations, reads);
    }

    /// Authorless parents retain explicit bootstrap ID requests even on a planned hint route.
    #[test]
    fn thread_bootstrap_fetch_is_explicit_exact_deduplicated_and_cleared_on_arrival() {
        use futures_util::FutureExt;
        use nostrdb::{NoteBuilder, Transaction};

        let (_tmp, ndb) = new_ndb();
        let author = FullKeypair::generate();
        let unknown_parent = NoteBuilder::new()
            .kind(1)
            .created_at(1)
            .content("parent whose author is not in the reply tag")
            .sign(&[3; 32])
            .build()
            .expect("parent");
        let unknown_id = NoteId::new(*unknown_parent.id());
        let known_id = NoteId::new([42; 32]);
        let read = NormRelayUrl::new("wss://account-read.example.com").expect("read relay");
        let hint = NormRelayUrl::new("wss://hint.example.com").expect("hint relay");
        let bootstrap = NormRelayUrl::new("ws://127.0.0.1:7777").expect("local bootstrap");
        let reads = HashSet::from([read.clone()]);
        let reply = NoteBuilder::new()
            .kind(1)
            .created_at(2)
            .content("reply with one authorless and one author-tagged ancestor")
            .start_tag()
            .tag_str("e")
            .tag_id(unknown_id.bytes())
            .tag_str(hint.as_str())
            .tag_str("root")
            .start_tag()
            .tag_str("e")
            .tag_id(known_id.bytes())
            .tag_str("")
            .tag_str("reply")
            .tag_id(author.pubkey.bytes())
            .sign(&author.secret_key.secret_bytes())
            .build()
            .expect("reply");
        ndb.process_client_event(&reply.json().expect("reply JSON"))
            .expect("ingest reply");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(&ndb).expect("transaction");
            if ndb.get_note_by_id(&txn, reply.id()).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "reply was not ingested");
            std::thread::sleep(Duration::from_millis(5));
        }
        let spec = SubConfig::builder(vec![Filter::new()
            .kinds([1])
            .event(unknown_id.bytes())
            .limit(500)
            .build()])
        .accounts_read_important()
        .with_author_outbox_augmentation()
        .for_thread(unknown_id, [NoteId::new(*reply.id())])
        .build();
        let mut runtime = runtime_with_ndb(&ndb);
        runtime.bootstrap_relays = HashSet::from([read, hint.clone(), bootstrap.clone()]);
        let ids = OutboxIdRegistry::new();
        let initial = runtime.advance(AuthorOutboxPlanAdvanceRequest {
            account_pubkey: test_pubkey(1),
            scoped: scoped_key("explicit-authorless-bootstrap"),
            account_read_relays: &reads,
            spec: &spec,
        });
        let mut effects = initial.effects.into_effects();
        assert_eq!(effects.len(), 1);
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = effects.pop().expect("job");
        let (_, ops, catch_up) =
            runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
        let expected = Filter::new().ids([unknown_id.bytes()]).build();
        let relay_list_filter = Filter::new()
            .authors([author.pubkey.bytes()])
            .kinds([10002])
            .build();
        let mut bootstrap_fetch = None;
        for op in ops.into_ops() {
            let ScopedSubOutboxOp::StartFetch {
                id,
                filters,
                relay_pkgs,
            } = op
            else {
                continue;
            };
            if !relay_pkgs.urls().contains(&bootstrap) {
                continue;
            }
            if filters.len() == 1 && filters[0].same_canonical_attributes(&relay_list_filter) {
                continue;
            }
            assert!(
                bootstrap_fetch.replace(id).is_none(),
                "only one bootstrap parent fetch"
            );
            assert_eq!(
                filters.len(),
                1,
                "no reply/#e, kind, author, or limit filters"
            );
            assert!(filters[0].same_canonical_attributes(&expected));
            assert_eq!(
                relay_pkgs.urls(),
                &HashSet::from([bootstrap.clone(), hint.clone()])
            );
            assert_eq!(relay_pkgs.source(), enostr::RelayUrlSource::Explicit);
            assert_eq!(
                relay_pkgs.demand_priority(),
                enostr::RelayDemandPriority::Important
            );
            assert!(bootstrap.allowed_for_source(relay_pkgs.source()));
            assert!(!bootstrap.allowed_for_source(enostr::RelayUrlSource::RemoteAdvertised));
        }
        let bootstrap_fetch = bootstrap_fetch.expect("explicit authorless parent fetch");
        let slot = runtime.slots.values().next().expect("thread slot");
        let routes = &slot
            .ready_plan()
            .expect("ready thread plan")
            .routes
            .live_routed_relays;
        assert!(routes.iter().any(|route| route.relay == hint));
        assert!(routes.iter().all(|route| route.relay != bootstrap));

        let mut catch_up = catch_up.into_effects();
        assert_eq!(catch_up.len(), 1);
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = catch_up.pop().expect("catch-up job");
        let (_, ops, effects) =
            runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
        assert!(
            ops.is_empty(),
            "same snapshot must not restart either missing-ID fetch"
        );
        assert!(effects.into_effects().is_empty());

        ndb.process_client_event(&unknown_parent.json().expect("parent JSON"))
            .expect("ingest parent");
        let deadline = Instant::now() + Duration::from_secs(2);
        let changed = loop {
            let present = {
                let txn = Transaction::new(&ndb).expect("transaction");
                let present = ndb.get_note_by_id(&txn, unknown_id.bytes()).is_ok();
                present
            };
            if present {
                if let Some(changed) = runtime.next_thread_change().now_or_never() {
                    break changed;
                }
            }
            assert!(
                Instant::now() < deadline,
                "parent arrival did not notify thread watch"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut effects = runtime.apply_thread_change(changed).into_effects();
        assert_eq!(effects.len(), 1);
        let ScopedSubEffect::StartAuthorOutboxPlanJob(job) = effects.pop().expect("arrival job");
        let (_, ops, _) = runtime.apply_plan_slot_ready(&ids, job.run(ndb.clone()), &reads, &ndb);
        let ops = ops.into_ops();
        assert!(ops.iter().any(|op| matches!(
            op,
            ScopedSubOutboxOp::ClearFetch { id } if *id == bootstrap_fetch
        )));
        assert!(ops.iter().all(|op| !matches!(
            op,
            ScopedSubOutboxOp::StartFetch { filters, relay_pkgs, .. }
                if relay_pkgs.urls().contains(&bootstrap)
                    && filters.iter().any(|filter| filter.same_canonical_attributes(&expected))
        )));
        let txn = Transaction::new(&ndb).expect("transaction");
        assert!(ndb.get_note_by_id(&txn, unknown_id.bytes()).is_ok());
        assert!(ndb.get_note_by_id(&txn, known_id.bytes()).is_err());
    }

    #[test]
    fn relay_list_ingestion_wait_delay_has_short_start_and_longer_tail() {
        assert_eq!(
            relay_list_ingestion_wait_delay(1),
            Duration::from_millis(50)
        );
        assert_eq!(
            relay_list_ingestion_wait_delay(2),
            Duration::from_millis(100)
        );
        assert_eq!(
            relay_list_ingestion_wait_delay(3),
            Duration::from_millis(200)
        );
        assert_eq!(
            relay_list_ingestion_wait_delay(4),
            Duration::from_millis(500)
        );
        assert_eq!(
            relay_list_ingestion_wait_delay(5),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            relay_list_ingestion_wait_delay(6),
            Duration::from_millis(2_000)
        );
    }

    #[test]
    fn relay_list_discovery_eose_waits_before_post_discovery_build() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = test_pubkey(0xA2);
        let account_relay =
            NormRelayUrl::new("wss://account-read-wait.example.com").expect("account relay");
        let account_read_relays = HashSet::from([account_relay.clone()]);
        let scoped = scoped_key("author-plan-discovery-wait");
        let spec = author_outbox_config(author);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        assert_eq!(
            advance_state(
                &mut runtime,
                &mut bridge,
                account,
                &scoped,
                &account_read_relays,
                &spec,
                &ndb,
            ),
            TestAdvance::Pending
        );
        let eose_id = discovery_mut(&mut runtime, account, &scoped).chunks[0].legs[0]
            .id
            .expect("discovery id");

        let (_ops, effects) =
            runtime.apply_relay_req_status(eose_id, &account_relay, Some(RelayReqStatus::Eose));

        assert!(
            effects.into_effects().is_empty(),
            "EOSE should start an ingestion wait, not immediately rebuild routes"
        );
        assert!(
            runtime.next_deadline().is_some(),
            "ingestion wait should be driven by the existing author-plan timer"
        );
    }

    #[test]
    fn relay_list_ingestion_wait_uses_capped_backoff_without_progress_then_finalizes() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = test_pubkey(0xA3);
        let account_relay =
            NormRelayUrl::new("wss://account-read-final.example.com").expect("account relay");
        let account_read_relays = HashSet::from([account_relay.clone()]);
        let scoped = scoped_key("author-plan-discovery-final-wait");
        let spec = author_outbox_config(author);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        let _ = advance_state(
            &mut runtime,
            &mut bridge,
            account,
            &scoped,
            &account_read_relays,
            &spec,
            &ndb,
        );
        let eose_id = discovery_mut(&mut runtime, account, &scoped).chunks[0].legs[0]
            .id
            .expect("discovery id");
        let (_ops, effects) =
            runtime.apply_relay_req_status(eose_id, &account_relay, Some(RelayReqStatus::Eose));
        assert!(effects.into_effects().is_empty());
        assert_eq!(ingestion_wait_attempt(&runtime, account, &scoped), 1);

        for expected_attempt in 2..=RELAY_LIST_INGESTION_WAIT_DELAYS.len() as u8 {
            force_ingestion_wait_due(&mut runtime, account, &scoped);
            let (_ops, effects) = runtime.apply_relay_list_discovery_retry_due(Instant::now());
            apply_author_outbox_effects_for_test(
                &mut runtime,
                &mut bridge,
                &account_read_relays,
                &ndb,
                effects,
            );
            assert_eq!(
                ingestion_wait_attempt(&runtime, account, &scoped),
                expected_attempt
            );
        }

        force_ingestion_wait_due(&mut runtime, account, &scoped);
        let (_ops, effects) = runtime.apply_relay_list_discovery_retry_due(Instant::now());
        apply_author_outbox_effects_for_test(
            &mut runtime,
            &mut bridge,
            &account_read_relays,
            &ndb,
            effects,
        );

        assert_eq!(
            advance_state(
                &mut runtime,
                &mut bridge,
                account,
                &scoped,
                &account_read_relays,
                &spec,
                &ndb,
            ),
            TestAdvance::Ready {
                generation: 1,
                live_routes: 0,
                full_history_routes: 0,
            }
        );
    }

    #[test]
    fn relay_list_ingestion_wait_finalizes_after_resolving_one_author() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = FullKeypair::generate();
        let account_relay =
            NormRelayUrl::new("wss://account-read-progress.example.com").expect("account relay");
        let routed_relay = "wss://author-progress.example.com";
        let account_read_relays = HashSet::from([account_relay.clone()]);
        let scoped = scoped_key("author-plan-discovery-progress");
        let spec = author_outbox_config(author.pubkey);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        let _ = advance_state(
            &mut runtime,
            &mut bridge,
            account,
            &scoped,
            &account_read_relays,
            &spec,
            &ndb,
        );
        let eose_id = discovery_mut(&mut runtime, account, &scoped).chunks[0].legs[0]
            .id
            .expect("discovery id");
        let (_ops, effects) =
            runtime.apply_relay_req_status(eose_id, &account_relay, Some(RelayReqStatus::Eose));
        assert!(effects.into_effects().is_empty());

        let note = nip65_write_relay_note_for_test(&author, &[routed_relay]);
        ndb.process_client_event(&note.json().expect("json"))
            .expect("ingest discovered relay list");
        wait_for_nip65_for_test(&ndb, &author.pubkey);
        force_ingestion_wait_due(&mut runtime, account, &scoped);
        let (_ops, effects) = runtime.apply_relay_list_discovery_retry_due(Instant::now());
        apply_author_outbox_effects_for_test(
            &mut runtime,
            &mut bridge,
            &account_read_relays,
            &ndb,
            effects,
        );

        assert_eq!(
            advance_state(
                &mut runtime,
                &mut bridge,
                account,
                &scoped,
                &account_read_relays,
                &spec,
                &ndb,
            ),
            TestAdvance::Ready {
                generation: 1,
                live_routes: 1,
                full_history_routes: 0,
            }
        );
    }

    #[test]
    fn duplicate_inputs_share_one_plan_slot() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = test_pubkey(0xA1);
        let account_read_relays =
            HashSet::from([
                NormRelayUrl::new("wss://account-read.example.com").expect("account relay")
            ]);
        let left = scoped_key("left-owner");
        let right = scoped_key("right-owner");
        let spec = author_outbox_config(author);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        let _ = advance_state(
            &mut runtime,
            &mut bridge,
            account,
            &left,
            &account_read_relays,
            &spec,
            &ndb,
        );
        let _ = advance_state(
            &mut runtime,
            &mut bridge,
            account,
            &right,
            &account_read_relays,
            &spec,
            &ndb,
        );

        assert_eq!(runtime.slots.len(), 1);
        assert_eq!(runtime.owner_slots.len(), 2);
        let discovery = discovery_mut(&mut runtime, account, &left);
        assert_eq!(discovery.chunks.len(), 1);
    }

    #[test]
    fn deactivate_account_drops_active_slots_but_keeps_ready_plan() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = test_pubkey(0xA0);
        let account_relay =
            NormRelayUrl::new("wss://account-switch-discovery.example.com").expect("relay");
        let account_read_relays = HashSet::from([account_relay]);
        let active_scoped = scoped_key("active-plan");
        let ready_scoped = scoped_key("ready-plan");
        let spec = author_outbox_config(author);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        let _ = advance_state(
            &mut runtime,
            &mut bridge,
            account,
            &active_scoped,
            &account_read_relays,
            &spec,
            &ndb,
        );
        assert!(
            discovery_mut(&mut runtime, account, &active_scoped).chunks[0].legs[0]
                .id
                .is_some(),
            "active slot should retain discovery id",
        );

        let ready_owner = AuthorOutboxPlanOwner {
            account_pubkey: account,
            scoped: ready_scoped,
        };
        let ready_slot_id = AuthorOutboxPlanSlotId(99);
        runtime
            .owner_slots
            .insert(ready_owner.clone(), ready_slot_id);
        runtime.slots.insert(
            ready_slot_id,
            AuthorOutboxPlanSlot {
                inputs: AuthorOutboxPlanInputs::new(&account_read_relays, &HashSet::new(), &spec),
                thread: None,
                owners: HashSet::from([ready_owner.clone()]),
                state: AuthorOutboxPlanState::Ready(CachedAuthorOutboxPlan {
                    generation: 42,
                    routes: PlannedAuthorOutboxRoutes::default(),
                }),
            },
        );

        bridge.with_returned_outbox(|_| runtime.deactivate_account(account));

        assert!(!runtime.owner_slots.contains_key(&AuthorOutboxPlanOwner {
            account_pubkey: account,
            scoped: active_scoped,
        }));
        assert_eq!(runtime.owner_slots.get(&ready_owner), Some(&ready_slot_id));
        assert!(matches!(
            runtime.slots.get(&ready_slot_id).map(|slot| &slot.state),
            Some(AuthorOutboxPlanState::Ready(_))
        ));
    }

    #[test]
    fn ready_plan_is_reused_until_inputs_change() {
        let (_tmp, ndb) = new_ndb();
        let account = test_pubkey(0x01);
        let author = FullKeypair::generate();
        let relay_a_url = "wss://author-a.example.com";
        let relay_b_url = "wss://author-b.example.com";
        let note_a = nip65_write_relay_note_for_test(&author, &[relay_a_url]);
        ndb.process_client_event(&note_a.json().expect("json"))
            .expect("ingest relay a");
        wait_for_nip65_for_test(&ndb, &author.pubkey);
        let account_read_relays = HashSet::new();
        let scoped = scoped_key("frozen-plan");
        let spec = author_outbox_config(author.pubkey);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut runtime = runtime_with_ndb(&ndb);

        let first = advance_until_ready(
            &mut runtime,
            &mut bridge,
            account,
            &scoped,
            &account_read_relays,
            &spec,
            &ndb,
        );
        assert_eq!(
            first,
            TestAdvance::Ready {
                generation: 1,
                live_routes: 1,
                full_history_routes: 0,
            }
        );

        let note_b = nip65_write_relay_note_for_test(&author, &[relay_b_url]);
        ndb.process_client_event(&note_b.json().expect("json"))
            .expect("ingest relay b");
        wait_for_nip65_for_test(&ndb, &author.pubkey);

        let second = advance_until_ready(
            &mut runtime,
            &mut bridge,
            account,
            &scoped,
            &account_read_relays,
            &spec,
            &ndb,
        );
        assert_eq!(
            second,
            TestAdvance::Ready {
                generation: 1,
                live_routes: 1,
                full_history_routes: 0,
            }
        );
    }

    #[test]
    fn author_outbox_plan_result_converts_large_author_groups() {
        let relay = NormRelayUrl::new("wss://large-author-route.example.com").expect("relay");
        let authors = (0..512).map(test_pubkey).collect::<Vec<_>>();
        let filter = Filter::new()
            .authors(authors.iter().map(Pubkey::bytes))
            .kinds([1])
            .build();
        let send_filter = SendFilter::try_from_filter(filter).expect("send filter");
        let result = SendAuthorOutboxPlanJobResult {
            thread: None,
            live_routed_relays: vec![SendPlannedRoutedRelay {
                relay: relay.clone(),
                relay_priority: RoutedRelayPriority::default(),
                filters: vec![send_filter],
                authors_by_filter_index: vec![(0, authors.clone())],
            }],
            full_history_routed_relays: Vec::new(),
            missing_authors: HashSet::new(),
        };
        let routes = planned_author_outbox_routes_from_job_result(result);
        assert_eq!(routes.live_routed_relays.len(), 1);
        let route = &routes.live_routed_relays[0];
        assert_eq!(route.relay, relay);
        assert_eq!(
            route
                .authors_by_filter_index
                .get(&0)
                .expect("filter authors")
                .len(),
            authors.len()
        );
    }

    #[test]
    fn author_outbox_plan_result_converts_all_routes() {
        let author = test_pubkey(0xE1);
        let filter = Filter::new()
            .authors([author.bytes()])
            .kinds([1])
            .limit(10)
            .build();
        let send_filter = SendFilter::try_from_filter(filter).expect("send filter");
        let route_count = 20;
        let result = SendAuthorOutboxPlanJobResult {
            thread: None,
            live_routed_relays: (0..route_count)
                .map(|index| SendPlannedRoutedRelay {
                    relay: NormRelayUrl::new(&format!(
                        "wss://author-plan-window-{index}.example.com"
                    ))
                    .expect("relay"),
                    relay_priority: RoutedRelayPriority::default(),
                    filters: vec![send_filter.clone()],
                    authors_by_filter_index: vec![(0, vec![author])],
                })
                .collect(),
            full_history_routed_relays: Vec::new(),
            missing_authors: HashSet::new(),
        };
        let routes = planned_author_outbox_routes_from_job_result(result);
        assert_eq!(routes.live_routed_relays.len(), route_count);
    }

    #[test]
    fn relay_list_discovery_chunks_authors_per_relay() {
        let authors = (0..260).map(test_pubkey).collect::<HashSet<_>>();
        let relays = HashSet::from([
            NormRelayUrl::new("wss://account-read-a.example.com").expect("relay a"),
            NormRelayUrl::new("wss://account-read-b.example.com").expect("relay b"),
        ]);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let discovery = start_discovery_for_test(&mut bridge, authors.clone(), relays.clone());

        let expected_chunks = authors.len().div_ceil(RELAY_LIST_DISCOVERY_AUTHORS_PER_REQ);
        assert_eq!(discovery.chunks.len(), expected_chunks);
        let mut observed_authors = HashSet::new();
        for chunk in &discovery.chunks {
            let chunk_authors = chunk.authors_for_test();
            assert_eq!(chunk.legs.len(), relays.len());
            assert!(!chunk_authors.is_empty());
            assert!(chunk_authors.len() <= RELAY_LIST_DISCOVERY_AUTHORS_PER_REQ);
            assert!(chunk_authors.iter().all(|author| authors.contains(author)));
            observed_authors.extend(chunk_authors);
            for leg in &chunk.legs {
                assert!(leg.id.is_some());
                assert!(relays.contains(&leg.relay));
            }
        }
        assert_eq!(observed_authors, authors);
    }

    #[test]
    fn relay_list_discovery_uses_outbox_fetch_ops() {
        let author = test_pubkey(2);
        let relays = HashSet::from([
            NormRelayUrl::new("wss://account-read-a.example.com").expect("relay a"),
            NormRelayUrl::new("wss://account-read-b.example.com").expect("relay b"),
        ]);
        let ids = OutboxIdRegistry::new();
        let (discovery, outbox_ops) =
            start_relay_list_discovery(&ids, HashSet::from([author]), relays.clone());
        let ops = outbox_ops.into_ops();

        assert_eq!(discovery.chunks[0].legs.len(), relays.len());
        assert_eq!(ops.len(), relays.len());
        for op in ops {
            let ScopedSubOutboxOp::StartFetch {
                id,
                filters,
                relay_pkgs,
            } = op
            else {
                panic!("relay-list discovery should use outbox fetch ops");
            };
            assert!(discovery.chunks[0]
                .legs
                .iter()
                .any(|leg| leg.id == Some(id)));
            assert_eq!(filters.len(), 1);
            assert_eq!(relay_pkgs.urls().len(), 1);
            assert!(relay_pkgs.urls().iter().all(|relay| relays.contains(relay)));
        }
    }

    #[test]
    fn relay_list_discovery_waits_for_every_read_relay_eose() {
        let author = test_pubkey(2);
        let eose_relay = NormRelayUrl::new("wss://eose.example.com").expect("eose relay");
        let retrying_relay =
            NormRelayUrl::new("wss://retrying.example.com").expect("valid retrying relay url");
        let relays = HashSet::from([eose_relay.clone(), retrying_relay.clone()]);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut discovery = start_discovery_for_test(&mut bridge, HashSet::from([author]), relays);
        let eose_id = discovery.chunks[0]
            .legs
            .iter()
            .find(|leg| leg.relay == eose_relay)
            .and_then(|leg| leg.id)
            .expect("eose relay discovery id");
        let advance = bridge.with_returned_outbox_ops(|_| {
            discovery.apply_relay_req_status(eose_id, &eose_relay, Some(RelayReqStatus::Eose))
        });

        assert_eq!(advance, RelayListDiscoveryAdvance::Waiting);
        assert!(discovery.chunks[0]
            .legs
            .iter()
            .any(|leg| leg.relay == eose_relay && leg.id.is_none()));
        let grace_deadline = discovery.chunks[0]
            .eose_grace_deadline
            .expect("first EOSE should start grace deadline");
        assert!(grace_deadline <= Instant::now() + RELAY_LIST_DISCOVERY_EOSE_GRACE);
        let retrying_id = discovery.chunks[0]
            .legs
            .iter()
            .find(|leg| leg.relay == retrying_relay)
            .and_then(|leg| leg.id)
            .expect("retrying relay should remain open");

        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                retrying_id,
                &retrying_relay,
                Some(RelayReqStatus::Eose),
            )),
            RelayListDiscoveryAdvance::Complete
        );
        assert!(discovery.chunks[0].legs.iter().all(|leg| leg.id.is_none()));
    }

    #[test]
    fn relay_list_discovery_status_fact_only_advances_matching_leg() {
        let author = test_pubkey(2);
        let eose_relay = NormRelayUrl::new("wss://exact-eose.example.com").expect("eose relay");
        let waiting_relay =
            NormRelayUrl::new("wss://exact-waiting.example.com").expect("waiting relay");
        let relays = HashSet::from([eose_relay.clone(), waiting_relay.clone()]);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut discovery = start_discovery_for_test(&mut bridge, HashSet::from([author]), relays);
        let eose_id = discovery.chunks[0]
            .legs
            .iter()
            .find(|leg| leg.relay == eose_relay)
            .and_then(|leg| leg.id)
            .expect("eose relay discovery id");

        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                OutboxSubId(9999),
                &eose_relay,
                Some(RelayReqStatus::Eose),
            )),
            RelayListDiscoveryAdvance::Waiting
        );
        assert!(discovery.chunks[0].legs.iter().all(|leg| leg.id.is_some()));

        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                eose_id,
                &eose_relay,
                Some(RelayReqStatus::Eose),
            )),
            RelayListDiscoveryAdvance::Waiting
        );
        assert!(discovery.chunks[0]
            .legs
            .iter()
            .any(|leg| leg.relay == eose_relay && leg.id.is_none()));
        assert!(discovery.chunks[0].eose_grace_deadline.is_some());

        let waiting_id = discovery.chunks[0]
            .legs
            .iter()
            .find(|leg| leg.relay == waiting_relay)
            .and_then(|leg| leg.id)
            .expect("waiting relay discovery id");
        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                waiting_id,
                &waiting_relay,
                Some(RelayReqStatus::Eose),
            )),
            RelayListDiscoveryAdvance::Complete
        );
        assert!(discovery.chunks[0].legs.iter().all(|leg| leg.id.is_none()));
    }

    #[test]
    fn silent_relay_list_discovery_waits_without_retrying() {
        let author = test_pubkey(2);
        let relay = NormRelayUrl::new("wss://account-read.example.com").expect("relay");
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut discovery =
            start_discovery_for_test(&mut bridge, HashSet::from([author]), HashSet::from([relay]));

        for _ in 0..3 {
            assert_eq!(
                bridge.with_returned_outbox(|_| discovery.apply_retry_due(Instant::now())),
                RelayListDiscoveryAdvance::Waiting
            );
            let leg = &discovery.chunks[0].legs[0];
            assert_eq!(
                leg.retry_attempts, 0,
                "open discovery should not retry without relay CLOSED or prior EOSE"
            );
            assert!(leg.retry_after.is_none());
            assert!(discovery.chunks[0].eose_grace_deadline.is_none());
        }
    }

    #[test]
    fn relay_list_discovery_eose_grace_completes_silent_remaining_legs() {
        let author = test_pubkey(2);
        let eose_relay = NormRelayUrl::new("wss://grace-eose.example.com").expect("eose relay");
        let silent_relay =
            NormRelayUrl::new("wss://grace-silent.example.com").expect("silent relay");
        let relays = HashSet::from([eose_relay.clone(), silent_relay.clone()]);
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut discovery = start_discovery_for_test(&mut bridge, HashSet::from([author]), relays);
        let eose_id = discovery.chunks[0]
            .legs
            .iter()
            .find(|leg| leg.relay == eose_relay)
            .and_then(|leg| leg.id)
            .expect("eose relay discovery id");
        let silent_id = discovery.chunks[0]
            .legs
            .iter()
            .find(|leg| leg.relay == silent_relay)
            .and_then(|leg| leg.id)
            .expect("silent relay discovery id");

        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                eose_id,
                &eose_relay,
                Some(RelayReqStatus::Eose),
            )),
            RelayListDiscoveryAdvance::Waiting
        );
        discovery.chunks[0].eose_grace_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_retry_due(Instant::now())),
            RelayListDiscoveryAdvance::Complete
        );
        assert!(discovery.chunks[0].legs.iter().all(|leg| leg.id.is_none()));
        assert!(discovery.chunks[0].eose_grace_deadline.is_none());
        assert_eq!(
            bridge.with_returned_outbox_ops(|relay_status| (
                relay_status(silent_id, &silent_relay),
                ScopedSubOutboxOps::default(),
            )),
            None
        );
    }

    #[test]
    fn closed_relay_list_discovery_reissues_after_backoff() {
        let author = test_pubkey(2);
        let closed_relay = NormRelayUrl::new("wss://closed.example.com").expect("closed relay");
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut discovery = start_discovery_for_test(
            &mut bridge,
            HashSet::from([author]),
            HashSet::from([closed_relay.clone()]),
        );
        let first_id = discovery.chunks[0].legs[0]
            .id
            .expect("initial discovery id");
        assert_eq!(
            bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                first_id,
                &closed_relay,
                Some(RelayReqStatus::Closed),
            )),
            RelayListDiscoveryAdvance::Waiting
        );
        {
            let leg = &mut discovery.chunks[0].legs[0];
            assert_eq!(leg.id, Some(first_id));
            assert_eq!(leg.retry_attempts, 1);
            assert!(leg.retry_after.is_some());
            leg.retry_after = Some(Instant::now() - Duration::from_millis(1));
        }

        assert_eq!(
            bridge.with_returned_outbox(|_| discovery.apply_retry_due(Instant::now())),
            RelayListDiscoveryAdvance::Waiting
        );
        let leg = &discovery.chunks[0].legs[0];
        assert_eq!(leg.id, Some(first_id), "retry should reuse discovery id");
        assert_eq!(leg.retry_attempts, 1);
        assert!(leg.retry_after.is_none());
    }

    #[test]
    fn closed_relay_list_discovery_completes_after_retry_budget_exhausted() {
        let author = test_pubkey(2);
        let closed_relay =
            NormRelayUrl::new("wss://terminal-closed.example.com").expect("closed relay");
        let mut bridge = RemoteOutboxReadModelHarness::default();
        let mut discovery = start_discovery_for_test(
            &mut bridge,
            HashSet::from([author]),
            HashSet::from([closed_relay.clone()]),
        );
        let discovery_id = discovery.chunks[0].legs[0]
            .id
            .expect("initial discovery id");

        for expected_attempt in 1..=4 {
            assert_eq!(
                bridge.with_returned_outbox_ops(|_| discovery.apply_relay_req_status(
                    discovery_id,
                    &closed_relay,
                    Some(RelayReqStatus::Closed),
                )),
                RelayListDiscoveryAdvance::Waiting
            );
            {
                let leg = &mut discovery.chunks[0].legs[0];
                assert_eq!(leg.id, Some(discovery_id));
                assert_eq!(leg.retry_attempts, expected_attempt);
                assert!(leg.retry_after.is_some());
                leg.retry_after = Some(Instant::now() - Duration::from_millis(1));
            }
            assert_eq!(
                bridge.with_returned_outbox(|_| discovery.apply_retry_due(Instant::now())),
                RelayListDiscoveryAdvance::Waiting
            );
            let leg = &discovery.chunks[0].legs[0];
            assert_eq!(leg.id, Some(discovery_id));
            assert_eq!(leg.retry_attempts, expected_attempt);
            assert!(leg.retry_after.is_none());
        }

        let (advance, outbox_ops) = discovery.apply_relay_req_status(
            discovery_id,
            &closed_relay,
            Some(RelayReqStatus::Closed),
        );
        assert_eq!(advance, RelayListDiscoveryAdvance::Complete);
        let ops = outbox_ops.into_ops();
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops.as_slice(),
            [ScopedSubOutboxOp::ClearFetch { id }] if *id == discovery_id
        ));
        let leg = &discovery.chunks[0].legs[0];
        assert_eq!(leg.id, None);
        assert_eq!(leg.retry_attempts, 4);
        assert!(leg.retry_after.is_none());
    }
}
