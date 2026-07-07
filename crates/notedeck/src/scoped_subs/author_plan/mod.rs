use enostr::{NormRelayUrl, OutboxIdRegistry, OutboxSubId, Pubkey, RelayReqStatus};
use hashbrown::{HashMap, HashSet};
use nostrdb::{Ndb, SendFilter};
use std::time::{Duration, Instant};

use super::config::{ScopedSubKey, SubConfig};
use super::planner::{AuthorOutboxPlanGeneration, PlannedAuthorOutboxRoutes, PlannedRoutedRelay};
use super::{ScopedSubEffect, ScopedSubEffects, ScopedSubOutboxOps};
use crate::author_outbox::{
    filter_author_pubkeys, plan_author_outbox_augmentation_for_indexed_filters,
    RelayDirectorySnapshot, RoutedFilter, RoutedRelayPriority,
};

mod discovery;

use discovery::{start_relay_list_discovery, RelayListDiscovery, RelayListDiscoveryAdvance};

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
pub(super) struct AuthorOutboxPlanSlotId(u64);

/// Input snapshot that must still match before a cached plan can be reused.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AuthorOutboxPlanInputs {
    account_read_relays: HashSet<NormRelayUrl>,
    spec: SubConfig,
}

impl AuthorOutboxPlanInputs {
    fn new(account_read_relays: &HashSet<NormRelayUrl>, spec: &SubConfig) -> Self {
        Self {
            account_read_relays: account_read_relays.clone(),
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
struct SendAuthorOutboxPlanJobInput {
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
}

/// Current shared author-outbox plan lifecycle for one input snapshot.
struct AuthorOutboxPlanSlot {
    inputs: AuthorOutboxPlanInputs,
    owners: HashSet<AuthorOutboxPlanOwner>,
    state: AuthorOutboxPlanState,
}

enum AuthorOutboxPlanState {
    BuildingInitial,
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

/// Retained frozen author-outbox plans and in-flight relay-list discovery.
pub(super) struct AuthorOutboxPlanRuntime {
    owner_slots: HashMap<AuthorOutboxPlanOwner, AuthorOutboxPlanSlotId>,
    slots: HashMap<AuthorOutboxPlanSlotId, AuthorOutboxPlanSlot>,
    next_slot_id: u64,
    next_generation: AuthorOutboxPlanGeneration,
}

impl Default for AuthorOutboxPlanRuntime {
    fn default() -> Self {
        Self {
            owner_slots: HashMap::new(),
            slots: HashMap::new(),
            next_slot_id: 1,
            next_generation: 1,
        }
    }
}

impl AuthorOutboxPlanRuntime {
    /// Return the next relay-list discovery retry deadline.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter_map(AuthorOutboxPlanSlot::next_deadline)
            .min()
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
    /// completed frozen plans for fast switch-back.
    pub(super) fn deactivate_account(&mut self, account_pubkey: Pubkey) -> ScopedSubOutboxOps {
        let owners = self
            .owner_slots
            .iter()
            .filter_map(|(owner, slot_id)| {
                if owner.account_pubkey != account_pubkey {
                    return None;
                }
                let is_ready = self
                    .slots
                    .get(slot_id)
                    .is_some_and(AuthorOutboxPlanSlot::is_ready);
                (!is_ready).then_some(owner.clone())
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
        let inputs = AuthorOutboxPlanInputs::new(account_read_relays, spec);
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
            if let AuthorOutboxPlanState::Ready(plan) = &slot.state {
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
        if let AuthorOutboxPlanState::DiscoveringRelays { discovery, .. } = slot.state {
            return discovery.unsubscribe_all();
        }
        ScopedSubOutboxOps::default()
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
                matches!(slot.state, AuthorOutboxPlanState::DiscoveringRelays { .. })
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
    ) -> (Vec<ScopedSubKey>, ScopedSubOutboxOps) {
        let slot_id = completion.slot_id;
        let outbox_ops = self.apply_build_result(ids, completion, account_read_relays);
        if !self.slot_is_ready(slot_id) {
            return (Vec::new(), outbox_ops);
        }

        (self.slot_scoped_keys(slot_id), outbox_ops)
    }

    /// Apply retained relay-list discovery retry and ingestion-wait deadlines.
    pub(super) fn apply_relay_list_discovery_retry_due(
        &mut self,
        now: Instant,
    ) -> (ScopedSubOutboxOps, ScopedSubEffects) {
        let slot_ids = self
            .slots
            .iter()
            .filter_map(|(slot_id, slot)| {
                matches!(
                    slot.state,
                    AuthorOutboxPlanState::DiscoveringRelays { .. }
                        | AuthorOutboxPlanState::WaitingForRelayListIngestion(_)
                )
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
    ) -> ScopedSubOutboxOps {
        let AuthorOutboxPlanJobCompletion {
            slot_id,
            build_stage,
            result,
        } = completion;
        let Some(slot) = self.slots.get_mut(&slot_id) else {
            return ScopedSubOutboxOps::default();
        };
        if !slot.state.matches_build_stage(build_stage) {
            return ScopedSubOutboxOps::default();
        }

        if build_stage == AuthorOutboxBuildStage::Initial
            && !result.missing_authors.is_empty()
            && !account_read_relays.is_empty()
        {
            let original_missing_author_count = result.missing_authors.len();
            let (discovery, outbox_ops) = start_relay_list_discovery(
                ids,
                result.missing_authors,
                account_read_relays.clone(),
            );
            slot.state = AuthorOutboxPlanState::DiscoveringRelays {
                discovery,
                original_missing_author_count,
            };
            return outbox_ops;
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
                return ScopedSubOutboxOps::default();
            }
        }

        let cached = self.cached_plan_from_job_result(result);
        if let Some(slot) = self.slots.get_mut(&slot_id) {
            slot.state = AuthorOutboxPlanState::Ready(cached);
        }
        ScopedSubOutboxOps::default()
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
    fn is_ready(&self) -> bool {
        matches!(self.state, AuthorOutboxPlanState::Ready(_))
    }

    fn next_deadline(&self) -> Option<Instant> {
        match &self.state {
            AuthorOutboxPlanState::DiscoveringRelays { discovery, .. } => discovery.next_deadline(),
            AuthorOutboxPlanState::WaitingForRelayListIngestion(wait) => Some(wait.ready_at),
            AuthorOutboxPlanState::BuildingInitial
            | AuthorOutboxPlanState::BuildingAfterRelayListDiscovery { .. }
            | AuthorOutboxPlanState::Ready(_) => None,
        }
    }
}

impl AuthorOutboxPlanState {
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
    SendAuthorOutboxPlanJobInput {
        account_read_relays: inputs.account_read_relays.clone(),
        live_filters: send_plan_filters(inputs.spec.filters()),
        full_history_filters: inputs
            .spec
            .full_history_config()
            .map(|full_history| send_plan_filters(full_history.filters()))
            .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::discovery::{
        start_relay_list_discovery, RelayListDiscovery, RelayListDiscoveryAdvance,
        RELAY_LIST_DISCOVERY_AUTHORS_PER_REQ,
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
                    bridge.with_returned_outbox(|ids| {
                        let (_, outbox_ops) =
                            runtime.apply_plan_slot_ready(ids, completion, account_read_relays);
                        outbox_ops
                    });
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
        let input = SendAuthorOutboxPlanJobInput {
            account_read_relays: HashSet::new(),
            live_filters: send_plan_filters(&[filter]),
            full_history_filters: Vec::new(),
        };

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
                inputs: AuthorOutboxPlanInputs::new(&account_read_relays, &spec),
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
                "open discovery should not retry without relay CLOSED"
            );
            assert!(leg.retry_after.is_none());
        }
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
