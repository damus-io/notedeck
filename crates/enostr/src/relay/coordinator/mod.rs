use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;
use std::time::{Duration, Instant};
use transparent_routing::RouteIndex;

use crate::relay::{
    compaction::{
        CompactionBlockedReason, CompactionCapacityOutcome, CompactionData,
        CompactionOperationPlan, CompactionTransition,
    },
    frame::QueuedRelayFrame,
    indexed_queue::IndexedQueue,
    negentropy::{NegentropyRelayEffect, NegentropyRelayEffects},
    outbox::RelayTransportDemand,
    subscription::StoredSubscriptionRef,
    transparent::{
        take_revoked_transparent_subs, TransparentData, TransparentPlaceResult,
        TransparentPlacementFeasibility, TransparentReplayOutcome,
    },
    FullHistorySubId, OutboxSubId, OutboxSubscriptions, RelayConnectionPriority,
    RelayCoordinatorLimits, RelayLimitations, RelayReqId, RelayReqStatus, RelayRoutingPreference,
    RelayType, ReqFilterLimits, SubPass, SubPassGuardian, SubPassRevocation,
};

mod transparent_routing;

/// RelayCoordinator routes each Outbox subscription to either the compaction or
/// transparent relay engine and tracks their status.
pub struct CoordinationData {
    limits: RelayCoordinatorLimits,
    current_generation: Option<u64>,
    routes: RouteIndex,
    compaction_data: CompactionData,
    transparent_data: TransparentData, // for outbox subs that prefer to be transparent
    preferred_compaction_promotions: IndexedQueue<OutboxSubId>,
    unsupported_capability: Option<UnsupportedRelayCapability>,
    relay_demand_entries: HashMap<OutboxSubId, RelayTransportDemand>,
    relay_demand: Option<RelayTransportDemand>,
}

fn take_available_subpasses(sub_guardian: &mut SubPassGuardian) -> Vec<SubPass> {
    let mut passes = Vec::new();
    while let Some(pass) = sub_guardian.take_pass() {
        passes.push(pass);
    }
    passes
}

fn return_subpasses(sub_guardian: &mut SubPassGuardian, passes: Vec<SubPass>) {
    for pass in passes {
        sub_guardian.return_pass(pass);
    }
}

/// Relay-capacity outcome before a NIP-77 session can be started.
pub(crate) enum NegentropyCapacityError {
    /// The relay cannot start the session yet, but the caller may retry later.
    Retry,
    /// The relay cannot carry this session.
    Drop,
}

/// Relay-local capacity granted for one full-history negentropy session.
#[derive(Debug)]
pub(crate) struct FullHistoryNegentropyCapacityGrant {
    pub(crate) generation: u64,
    pub(crate) pass: SubPass,
}

/// Outcome for the transparent probe pass before fallback work is enabled.
pub(super) enum ProbeTransparentRouteOutcome {
    Placed,
    NeedsCapacity,
    Skipped,
}

/// Result of probing one transparent route plus exact coordinator output.
struct ProbeTransparentRouteResult {
    outcome: ProbeTransparentRouteOutcome,
    output: CoordinationOutput,
}

/// Outcome for the fallback-enabled transparent routing pass.
pub(super) enum FallbackTransparentRouteOutcome {
    Placed,
    Preserved,
    Fallback,
    Queued,
    Skipped,
}

/// Result of fallback-aware transparent routing plus exact coordinator output.
struct FallbackTransparentRouteResult {
    outcome: FallbackTransparentRouteOutcome,
    output: CoordinationOutput,
}

/// Result of trying to materialize one dedicated route.
struct TransparentPlaceAttempt {
    result: Option<TransparentPlaceResult>,
    output: CoordinationOutput,
}

/// Coordinator decision for whether compaction can still own an active request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompactionLimitFit {
    Compactable,
    Unrepresentable,
}

/// Cleanup that must remove relay-engine state before route ownership is cleared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteCleanup {
    Transparent(OutboxSubId),
    Compaction(OutboxSubId),
    RouteOnly(OutboxSubId),
}

/// Next action while draining preferred compaction promotion candidates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreferredCompactionPromotion {
    Promote {
        id: OutboxSubId,
        pass_deficit: usize,
    },
    Cleanup(RouteCleanup),
}

/// Outcome of draining queued transparent retries at a capacity boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransparentRetryDrain {
    Drained,
    MadeProgress {
        still_queued: bool,
    },
    Blocked {
        reason: TransparentRetryBlockedReason,
    },
}

/// Stable reason transparent retry draining stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransparentRetryBlockedReason {
    NoPlacementProgress,
}

/// Result of applying one capacity-available step.
struct CapacityAvailableStepResult {
    output: CoordinationOutput,
    has_immediate_work: bool,
}

/// Coordinator-visible progress from applying available compaction capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompactionCapacityProgress {
    pub(crate) still_queued: bool,
    pub(crate) made_progress: bool,
    pub(crate) blocked_reason: Option<CompactionBlockedReason>,
}

impl CompactionCapacityProgress {
    /// Returns true only when this capacity application left queued work that is not blocked
    /// by the stable placement state reached during capacity application.
    pub(crate) fn has_immediate_work(&self) -> bool {
        self.still_queued && self.made_progress && self.blocked_reason.is_none()
    }
}

impl TransparentRetryDrain {
    /// Returns true when transparent retry draining made progress and left work
    /// that should be retried against the newly stable capacity state.
    fn has_immediate_work(self) -> bool {
        matches!(self, Self::MadeProgress { still_queued: true })
    }
}

/// Planned compaction-side work created while applying a max-subscription
/// downgrade.
struct LimitDowngradePlan {
    compaction_revocations: Vec<crate::relay::SubPassRevocation>,
    fallback_compaction: CompactionOperationPlan,
}

/// Downgrade planning result plus exact coordinator output produced while planning.
struct LimitDowngradePlanResult {
    plan: LimitDowngradePlan,
    output: CoordinationOutput,
}

/// Transparent retry drain result plus exact coordinator output produced while draining.
struct TransparentRetryDrainResult {
    drain: TransparentRetryDrain,
    output: CoordinationOutput,
}

/// One possible pass revocation target during limit reduction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LimitReductionTarget {
    Negentropy,
    Transparent,
    Compaction,
}

/// One transparent route that can release a pass during limit reduction.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TransparentLimitReductionCandidate {
    id: OutboxSubId,
    sid: RelayReqId,
    owner_ids: Vec<OutboxSubId>,
    preference: RelayRoutingPreference,
}

/// One selected transparent route and its matching pass revocation.
struct TransparentLimitReduction {
    id: OutboxSubId,
    revocation: crate::relay::SubPassRevocation,
}

/// Selected pass revocation targets for one relay limit decrease.
#[derive(Default)]
struct LimitReductionTargets {
    negentropy_revocations: Vec<crate::relay::SubPassRevocation>,
    transparent_revocations: Vec<TransparentLimitReduction>,
    compaction_revocations: Vec<crate::relay::SubPassRevocation>,
}

/// Relay capability failures that block subscription-id-bearing protocols.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsupportedRelayCapability {
    SubscriptionIdLength { max_subid_length: usize },
}

/// Effective relay-limit transition that requires coordinator placement to be
/// re-derived before queued work is retried.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LimitChange {
    previous: RelayLimitations,
    updated: RelayLimitations,
}

/// Relay-engine replay effects collected after a websocket reconnect.
#[derive(Default)]
struct RelayOpenReplayEffects {
    invalidated_sub_ids: HashSet<OutboxSubId>,
    blocked_transparent_ids: HashSet<OutboxSubId>,
    released_compaction_ids: HashSet<OutboxSubId>,
}

impl LimitDowngradePlan {
    /// Creates a downgrade plan seeded with any compaction pass revocations.
    fn new(compaction_revocations: Vec<crate::relay::SubPassRevocation>) -> Self {
        Self {
            compaction_revocations,
            fallback_compaction: CompactionOperationPlan::default(),
        }
    }

    /// Returns true when the compaction engine has downgrade work to apply.
    fn has_compaction_work(&self) -> bool {
        !self.compaction_revocations.is_empty() || !self.fallback_compaction.is_empty()
    }
}

impl LimitChange {
    /// Returns a limit change only when the effective coordinator limits differ.
    fn new(previous: RelayLimitations, updated: RelayLimitations) -> Option<Self> {
        (previous != updated).then_some(Self { previous, updated })
    }

    /// Returns true when the change is only relay pass capacity.
    fn only_maximum_subs_changed(&self) -> bool {
        self.previous.maximum_subs != self.updated.maximum_subs
            && self.previous.max_json_bytes == self.updated.max_json_bytes
    }
}

impl CoordinationData {
    /// Creates relay coordination state without opening a websocket.
    pub(crate) fn new(limits: RelayLimitations) -> Self {
        Self::new_with_generation(limits, None)
    }

    fn new_with_generation(limits: RelayLimitations, current_generation: Option<u64>) -> Self {
        let limits = RelayCoordinatorLimits::new(limits);
        let compaction_data = CompactionData::default();
        Self {
            limits,
            current_generation,
            compaction_data,
            transparent_data: TransparentData::default(),
            routes: RouteIndex::default(),
            preferred_compaction_promotions: IndexedQueue::default(),
            unsupported_capability: None,
            relay_demand_entries: HashMap::default(),
            relay_demand: None,
        }
    }

    fn relay_transport_demand_for_sub(
        view: StoredSubscriptionRef<'_>,
    ) -> Option<RelayTransportDemand> {
        let priority = RelayConnectionPriority::from_demand(view.demand_priority, 1)?;
        Some(RelayTransportDemand::new(
            priority,
            view.relay_url_source,
            view.connection_weight,
        ))
    }

    fn aggregate_relay_demand_entries(&self) -> Option<RelayTransportDemand> {
        self.relay_demand_entries
            .values()
            .copied()
            .fold(None, |aggregate, demand| {
                RelayTransportDemand::merge_optional(aggregate, Some(demand))
            })
    }

    fn refresh_relay_demand(&mut self) -> Option<Option<RelayTransportDemand>> {
        let next = self.aggregate_relay_demand_entries();
        if self.relay_demand != next {
            self.relay_demand = next;
            return Some(next);
        }
        None
    }

    fn set_relay_demand_entry(
        &mut self,
        id: OutboxSubId,
        view: Option<StoredSubscriptionRef<'_>>,
    ) -> Option<Option<RelayTransportDemand>> {
        if let Some(demand) = view.and_then(Self::relay_transport_demand_for_sub) {
            self.relay_demand_entries.insert(id, demand);
        } else {
            self.relay_demand_entries.remove(&id);
        }
        self.refresh_relay_demand()
    }

    fn clear_relay_demand_entry(
        &mut self,
        id: OutboxSubId,
    ) -> Option<Option<RelayTransportDemand>> {
        self.relay_demand_entries.remove(&id);
        self.refresh_relay_demand()
    }

    pub(crate) fn current_generation(&self) -> Option<u64> {
        self.current_generation
    }

    /// Current authoritative relay limits for this coordinator.
    pub fn current_limits(&self) -> RelayLimitations {
        RelayLimitations {
            maximum_subs: self.limits.maximum_subs(),
            max_json_bytes: self.limits.max_json_bytes,
        }
    }

    /// Asserts that coordinator route ownership matches active relay-engine state.
    fn assert_route_consistency(&self) {
        #[cfg(debug_assertions)]
        {
            for id in self.transparent_data.request_ids() {
                debug_assert_eq!(
                    self.routes.route_type(&id),
                    Some(RelayType::Transparent),
                    "active transparent request must have transparent coordination ownership"
                );
            }

            for id in self.compaction_data.request_ids() {
                debug_assert_eq!(
                    self.routes.route_type(&id),
                    Some(RelayType::Compaction),
                    "active compaction request must have compaction coordination ownership"
                );
            }

            for (id, route) in self.routes.iter() {
                match route {
                    RelayType::Transparent => debug_assert!(
                        self.compaction_data.req_status(id).is_none(),
                        "transparent coordination route must not also be active in compaction"
                    ),
                    RelayType::Compaction => debug_assert!(
                        self.transparent_data.req_status(id).is_none(),
                        "compaction coordination route must not also be active in another route"
                    ),
                }
            }
        }
    }

    /// Apply new effective relay limits to the coordinator.
    pub fn set_limits(
        &mut self,
        subs: &OutboxSubscriptions,
        active_negentropy_session_count: usize,
        limits: RelayLimitations,
    ) -> CoordinationOutput {
        let Some(change) = LimitChange::new(self.current_limits(), limits) else {
            return CoordinationOutput::empty();
        };

        if change.only_maximum_subs_changed() {
            self.set_max_size(subs, active_negentropy_session_count, limits.maximum_subs)
        } else {
            self.reconcile_placement_for_limit_change(subs, active_negentropy_session_count, change)
        }
    }

    fn set_max_size(
        &mut self,
        subs: &OutboxSubscriptions,
        active_negentropy_session_count: usize,
        max_size: usize,
    ) -> CoordinationOutput {
        let previous = self.current_limits();
        let mut updated = previous;
        updated.maximum_subs = max_size;
        let Some(change) = LimitChange::new(previous, updated) else {
            return CoordinationOutput::empty();
        };

        self.reconcile_placement_for_limit_change(subs, active_negentropy_session_count, change)
    }

    /// Re-derives coordinator placement for any effective relay-limit change.
    #[profiling::function]
    fn reconcile_placement_for_limit_change(
        &mut self,
        subs: &OutboxSubscriptions,
        active_negentropy_session_count: usize,
        change: LimitChange,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        tracing::debug!(
            previous = ?change.previous,
            updated = ?change.updated,
            "reconciling relay placement for limit change"
        );

        let req_limits_changed = change.previous.max_json_bytes != change.updated.max_json_bytes;
        let maximum_subs_increased = change.updated.maximum_subs > change.previous.maximum_subs;
        let maximum_subs_decreased = change.updated.maximum_subs < change.previous.maximum_subs;

        self.limits.max_json_bytes = change.updated.max_json_bytes;

        if maximum_subs_increased {
            output.extend(self.apply_maximum_subs_change(
                subs,
                active_negentropy_session_count,
                change.updated.maximum_subs,
            ));
        }

        if req_limits_changed {
            output.extend(self.repack_dedicated_for_current_limits(subs));
            output.extend(self.repack_compaction_for_current_limits(subs));
        }

        if maximum_subs_decreased {
            output.extend(self.apply_maximum_subs_change(
                subs,
                active_negentropy_session_count,
                change.updated.maximum_subs,
            ));
        }

        output
    }

    /// Applies available passes to route work that must remain coupled to the
    /// current coordinator transition.
    pub(super) fn apply_capacity_available(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        loop {
            let result = self.apply_capacity_available_step(subs);
            let has_immediate_work = result.has_immediate_work;
            output.extend(result.output);
            if !has_immediate_work {
                return output;
            }
        }
    }

    fn apply_capacity_available_step(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> CapacityAvailableStepResult {
        let mut output = CoordinationOutput::empty();
        let transparent_result = self.drain_transparent_retry_queue(subs);
        let transparent_has_immediate_work = transparent_result.drain.has_immediate_work();
        output.extend(transparent_result.output);
        output.extend(self.promote_preferred_compaction_routes(subs));
        let compaction_result = self.apply_available_compaction_capacity_inner(subs);
        let CompactionCapacityResult {
            output: compaction_output,
            queue: compaction_queue,
        } = compaction_result;
        output.extend(compaction_output);
        self.assert_route_consistency();
        CapacityAvailableStepResult {
            output,
            has_immediate_work: transparent_has_immediate_work
                || compaction_queue.has_immediate_work(),
        }
    }

    fn reserve_full_history_subpass(&mut self) -> Option<SubPass> {
        self.limits.sub_guardian.take_pass()
    }

    /// Reserve capacity for one full-history negentropy session if this relay
    /// can carry subscription-id protocol traffic now.
    pub(crate) fn reserve_full_history_negentropy_capacity(
        &mut self,
    ) -> Result<FullHistoryNegentropyCapacityGrant, NegentropyCapacityError> {
        if !self.supports_relay_subscription_ids() {
            return Err(NegentropyCapacityError::Drop);
        }
        let Some(generation) = self.current_generation else {
            return Err(NegentropyCapacityError::Retry);
        };
        let pass = self
            .reserve_full_history_subpass()
            .ok_or(NegentropyCapacityError::Retry)?;
        Ok(FullHistoryNegentropyCapacityGrant { generation, pass })
    }

    /// Request capacity for one pending full-history negentropy session.
    pub(crate) fn request_full_history_negentropy_capacity(
        &mut self,
    ) -> Result<CoordinationOutput, NegentropyCapacityError> {
        match self.reserve_full_history_negentropy_capacity() {
            Ok(grant) => Ok(CoordinationOutput::from_full_history_capacity_grant(grant)),
            Err(err) => Err(err),
        }
    }

    /// Return one relay-local pass held by a completed/cancelled full-history
    /// negentropy session.
    pub(crate) fn return_full_history_subpass(&mut self, pass: SubPass) {
        self.limits.sub_guardian.return_pass(pass);
    }

    pub(crate) fn return_full_history_negentropy_capacity(
        &mut self,
        subs: &OutboxSubscriptions,
        pass: SubPass,
    ) -> CoordinationOutput {
        self.return_full_history_subpass(pass);
        self.apply_capacity_available(subs)
    }

    pub(crate) fn return_full_history_subpasses(&mut self, effects: NegentropyRelayEffects) {
        for pass in effects.returned_passes {
            self.return_full_history_subpass(pass);
        }
    }

    pub(crate) fn apply_negentropy_effects_after_release(
        &mut self,
        subs: &OutboxSubscriptions,
        effects: NegentropyRelayEffects,
    ) -> CoordinationOutput {
        self.return_full_history_subpasses(effects);
        self.apply_capacity_available(subs)
    }

    /// Applies max-subscription capacity changes at the reconcile phase that
    /// matches the direction of the capacity change.
    fn apply_maximum_subs_change(
        &mut self,
        subs: &OutboxSubscriptions,
        active_negentropy_session_count: usize,
        max_size: usize,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        let mut invalidated_sub_ids = HashSet::new();
        let Some(revocations) = self.limits.set_maximum_subs(max_size) else {
            return self.apply_capacity_available(subs);
        };

        self.routes
            .rebuild_from_dedicated(subs, &self.transparent_data);
        let targets =
            self.select_limit_reduction_targets(subs, active_negentropy_session_count, revocations);

        output.extend(CoordinationOutput::from_negentropy_revocations(
            self.current_generation,
            targets.negentropy_revocations,
        ));
        let transparent_revocations = targets
            .transparent_revocations
            .into_iter()
            .map(|target| (target.id, target.revocation))
            .collect();
        let transparent_output = take_revoked_transparent_subs(
            self.current_generation,
            &mut self.transparent_data,
            transparent_revocations,
        );
        let revoked_ids = transparent_output.revoked_ids;
        invalidated_sub_ids.extend(revoked_ids.iter().copied());
        output.extend(CoordinationOutput::from_invalidated_sub_ids(
            invalidated_sub_ids,
            transparent_output.frames,
        ));
        let downgrade =
            self.plan_limit_downgrade(subs, revoked_ids, targets.compaction_revocations);
        output.extend(downgrade.output);
        output.extend(self.execute_limit_downgrade_compaction(subs, downgrade.plan));
        output.extend(self.apply_capacity_available(subs));
        output
    }

    /// Selects exact negentropy, transparent, and compaction victims for a relay
    /// limit decrease by choosing the least disruptive next target each time.
    fn select_limit_reduction_targets(
        &self,
        subs: &OutboxSubscriptions,
        active_negentropy_session_count: usize,
        revocations: Vec<crate::relay::SubPassRevocation>,
    ) -> LimitReductionTargets {
        let mut negentropy_session_count = active_negentropy_session_count;
        let mut seen_transparent_sids = HashSet::new();
        let mut transparent_candidates = self
            .routes
            .limit_reduction_candidates()
            .into_iter()
            .filter_map(|id| {
                let sid = self.transparent_data.active_sid(&id)?;
                if !seen_transparent_sids.insert(sid.clone()) {
                    return None;
                }
                let mut owner_ids = self
                    .transparent_data
                    .owner_ids_for(&id)?
                    .into_iter()
                    .collect::<Vec<_>>();
                owner_ids.sort_unstable();
                Some(TransparentLimitReductionCandidate {
                    id,
                    sid,
                    preference: Self::shared_transparent_owner_preference(subs, &owner_ids)?,
                    owner_ids,
                })
            })
            .collect::<Vec<_>>();
        let compaction_costs = self.compaction_data.downgrade_revocation_costs(subs);
        let mut compaction_costs = compaction_costs.into_iter().peekable();

        let mut targets = LimitReductionTargets::default();
        let mut selected_transparent_sids: HashSet<RelayReqId> = HashSet::new();
        let mut planned_transparent_fallbacks = Vec::new();

        for revocation in revocations {
            let has_compaction_candidate = compaction_costs.peek().is_some();
            let transparent_index = transparent_candidates.iter().position(|candidate| {
                self.limit_reduction_transparent_candidate_can_be_selected(
                    subs,
                    candidate,
                    &selected_transparent_sids,
                    &planned_transparent_fallbacks,
                    has_compaction_candidate,
                )
            });
            let transparent = transparent_index.map(|index| &transparent_candidates[index]);

            match Self::next_limit_reduction_target(
                negentropy_session_count > 0,
                transparent,
                has_compaction_candidate,
            ) {
                Some(LimitReductionTarget::Negentropy) => {
                    negentropy_session_count -= 1;
                    targets.negentropy_revocations.push(revocation);
                }
                Some(LimitReductionTarget::Transparent) => {
                    let selected = transparent_candidates.remove(
                        transparent_index
                            .expect("transparent target selected without candidate index"),
                    );
                    selected_transparent_sids.insert(selected.sid.clone());
                    if matches!(
                        selected.preference,
                        RelayRoutingPreference::PreferDedicated
                            | RelayRoutingPreference::NoPreference
                    ) {
                        for owner_id in &selected.owner_ids {
                            if !planned_transparent_fallbacks.contains(owner_id) {
                                planned_transparent_fallbacks.push(*owner_id);
                            }
                        }
                    }
                    targets
                        .transparent_revocations
                        .push(TransparentLimitReduction {
                            id: selected.id,
                            revocation,
                        });
                }
                Some(LimitReductionTarget::Compaction) => {
                    compaction_costs.next();
                    targets.compaction_revocations.push(revocation);
                }
                None => {
                    debug_assert!(
                        false,
                        "limit decrease requested more revocations than active relay passes"
                    );
                    tracing::error!(
                        "limit decrease requested more revocations than active relay passes"
                    );
                    targets.compaction_revocations.push(revocation);
                }
            }
        }

        targets
    }

    fn shared_transparent_owner_preference(
        subs: &OutboxSubscriptions,
        owner_ids: &[OutboxSubId],
    ) -> Option<RelayRoutingPreference> {
        owner_ids
            .iter()
            .filter_map(|owner_id| subs.routing_preference(owner_id))
            .min_by_key(|preference| Self::transparent_repack_priority(*preference))
    }

    fn shared_transparent_route_preference(
        &self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> Option<RelayRoutingPreference> {
        let owner_ids = self.transparent_data.owner_ids_for(&id)?;
        let owner_ids = owner_ids.into_iter().collect::<Vec<_>>();
        Self::shared_transparent_owner_preference(subs, &owner_ids)
    }

    /// Returns whether a transparent max-subscription revocation can proceed
    /// without trading an active non-required route for queued compaction.
    fn limit_reduction_transparent_candidate_can_be_selected(
        &self,
        subs: &OutboxSubscriptions,
        candidate: &TransparentLimitReductionCandidate,
        selected_transparent_sids: &HashSet<RelayReqId>,
        planned_fallbacks: &[OutboxSubId],
        has_compaction_candidate: bool,
    ) -> bool {
        if candidate.preference == RelayRoutingPreference::RequireDedicated {
            return true;
        }

        if has_compaction_candidate
            && self.dedicated_req_status(&candidate.id) == Some(RelayReqStatus::Eose)
        {
            return false;
        }

        if self.limit_reduction_transparent_fallback_can_place(
            subs,
            candidate,
            selected_transparent_sids,
            planned_fallbacks,
        ) {
            return true;
        }

        !has_compaction_candidate
    }

    /// Simulates compaction fallback for selected non-required transparent
    /// revocations, counting only active transparent legs that are not consumed
    /// by revocation objects as replacement capacity.
    fn limit_reduction_transparent_fallback_can_place(
        &self,
        subs: &OutboxSubscriptions,
        candidate: &TransparentLimitReductionCandidate,
        selected_transparent_sids: &HashSet<RelayReqId>,
        planned_fallbacks: &[OutboxSubId],
    ) -> bool {
        let mut revoked_sids = selected_transparent_sids.clone();
        revoked_sids.insert(candidate.sid.clone());

        let mut fallback_ids = planned_fallbacks.to_vec();
        for owner_id in &candidate.owner_ids {
            if !fallback_ids.contains(owner_id) {
                fallback_ids.push(*owner_id);
            }
        }

        let mut available_passes = self.limits.sub_guardian.available_passes();
        let mut counted_capacity_sids = HashSet::new();
        for id in &fallback_ids {
            let Some(sid) = self.transparent_data.active_sid(id) else {
                continue;
            };
            if revoked_sids.contains(&sid) || !counted_capacity_sids.insert(sid) {
                continue;
            }
            available_passes = available_passes.saturating_add(1);
        }

        self.compaction_data.can_place_subscribes_with_passes(
            subs,
            fallback_ids,
            ReqFilterLimits::from_relay_limits(&self.limits),
            available_passes,
        )
    }

    /// Chooses the next least-disruptive limit-reduction target.
    fn next_limit_reduction_target(
        has_negentropy_session: bool,
        transparent: Option<&TransparentLimitReductionCandidate>,
        has_compaction_candidate: bool,
    ) -> Option<LimitReductionTarget> {
        if has_negentropy_session {
            return Some(LimitReductionTarget::Negentropy);
        }

        let Some(transparent) = transparent else {
            return has_compaction_candidate.then_some(LimitReductionTarget::Compaction);
        };

        if !has_compaction_candidate {
            return Some(LimitReductionTarget::Transparent);
        }

        match transparent.preference {
            RelayRoutingPreference::NoPreference => Some(LimitReductionTarget::Transparent),
            RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::RequireDedicated => {
                Some(LimitReductionTarget::Compaction)
            }
        }
    }

    /// Applies policy-aware rerouting for dedicated subscriptions evicted by a
    /// max-subscription downgrade and returns any resulting compaction work.
    fn plan_limit_downgrade(
        &mut self,
        subs: &OutboxSubscriptions,
        revoked_ids: Vec<OutboxSubId>,
        compaction_revocations: Vec<crate::relay::SubPassRevocation>,
    ) -> LimitDowngradePlanResult {
        let mut downgrade = LimitDowngradePlan::new(compaction_revocations);
        let mut output = CoordinationOutput::empty();
        for id in revoked_ids {
            if subs.stored_ref(&id).is_none() {
                output.extend(self.execute_route_cleanup(subs, RouteCleanup::RouteOnly(id)));
                continue;
            }

            match subs.routing_preference(&id).unwrap_or_default() {
                RelayRoutingPreference::RequireDedicated => {
                    output.extend(self.queue_dedicated_retry(subs, id).output);
                }
                RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::NoPreference => {
                    if self.compaction_limit_fit(subs, id) == Some(CompactionLimitFit::Compactable)
                    {
                        output.extend(self.set_compaction_route(subs, id));
                        downgrade.fallback_compaction.sub(id);
                    } else {
                        output.extend(self.queue_dedicated_retry(subs, id).output);
                    }
                }
            }
        }

        self.routes
            .rebuild_from_dedicated(subs, &self.transparent_data);

        LimitDowngradePlanResult {
            plan: downgrade,
            output,
        }
    }

    /// Executes the compaction-side effects needed after a max-subscription
    /// downgrade.
    fn execute_limit_downgrade_compaction(
        &mut self,
        subs: &OutboxSubscriptions,
        downgrade: LimitDowngradePlan,
    ) -> CoordinationOutput {
        if !downgrade.has_compaction_work() {
            return CoordinationOutput::empty();
        }

        let mut output = CoordinationOutput::empty();
        let LimitDowngradePlan {
            compaction_revocations,
            fallback_compaction,
        } = downgrade;
        if !compaction_revocations.is_empty() {
            let limits = ReqFilterLimits::from_relay_limits(&self.limits);
            let granted_passes = take_available_subpasses(&mut self.limits.sub_guardian);
            let transition = self.compaction_data.revocate_all(
                self.current_generation,
                limits,
                granted_passes,
                subs,
                compaction_revocations,
            );
            output.extend(self.apply_compaction_transition(transition));
        }
        if !fallback_compaction.is_empty() {
            output.extend(self.apply_compaction_plan(subs, fallback_compaction));
        }
        output
    }

    /// Applies compaction engine effects from coordinator-owned policy work.
    fn apply_compaction_plan(
        &mut self,
        subs: &OutboxSubscriptions,
        plan: CompactionOperationPlan,
    ) -> CoordinationOutput {
        if plan.is_empty() {
            return CoordinationOutput::empty();
        }

        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        let granted_passes = take_available_subpasses(&mut self.limits.sub_guardian);
        let transition = self
            .compaction_data
            .apply_operation_plan_without_capacity_application(
                self.current_generation,
                limits,
                granted_passes,
                subs,
                plan,
            );
        self.apply_compaction_transition(transition)
    }

    fn apply_compaction_transition(
        &mut self,
        transition: CompactionTransition,
    ) -> CoordinationOutput {
        return_subpasses(&mut self.limits.sub_guardian, transition.returned_passes);
        let eose_delta = RelayEoseDelta {
            sub_ids: transition.eose_sub_ids,
            ..Default::default()
        };
        let mut facts = CoordinationFacts::new(eose_delta, transition.invalidated_sub_ids);
        facts.status_changed_sub_ids = transition.status_changed_sub_ids;
        CoordinationOutput::new(facts, transition.frames)
    }

    fn compaction_unsubscribe(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut plan = CompactionOperationPlan::default();
        plan.unsub(id);
        self.apply_compaction_plan(subs, plan)
    }

    fn remove_compaction_after_relay_closed(&mut self, id: OutboxSubId) -> CoordinationOutput {
        let transition = self.compaction_data.remove_after_relay_closed(id);
        self.apply_compaction_transition(transition)
    }

    fn compaction_request_free_subs(
        &mut self,
        subs: &OutboxSubscriptions,
        reserve_count: usize,
    ) -> CoordinationOutput {
        let mut plan = CompactionOperationPlan::default();
        plan.request_free_subs(reserve_count);
        self.apply_compaction_plan(subs, plan)
    }

    /// Returns whether `id` can remain in compaction under current REQ limits.
    fn compaction_limit_fit(
        &self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> Option<CompactionLimitFit> {
        let filters = subs.filters_for_compaction(&id)?;
        match ReqFilterLimits::from_relay_limits(&self.limits).filters_fit_single_req(&filters) {
            Some(true) => Some(CompactionLimitFit::Compactable),
            Some(false) | None => Some(CompactionLimitFit::Unrepresentable),
        }
    }

    fn repack_compaction_for_current_limits(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> CoordinationOutput {
        let active = self.compaction_data.request_ids();
        if active.is_empty() {
            return CoordinationOutput::empty();
        }

        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        let granted_passes = take_available_subpasses(&mut self.limits.sub_guardian);
        let transition = self.compaction_data.repack_active_for_current_limits(
            self.current_generation,
            limits,
            granted_passes,
            subs,
        );
        self.apply_compaction_transition(transition)
    }

    /// Returns active dedicated routes in the same preference order used for
    /// normal dedicated placement.
    fn active_transparent_repack_order(&self, subs: &OutboxSubscriptions) -> Vec<OutboxSubId> {
        let active = self.transparent_data.request_ids();
        if active.is_empty() {
            return active;
        }

        let active_set = active.iter().copied().collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let mut ordered = Vec::with_capacity(active.len());

        for id in self.routes.limit_reduction_candidates() {
            if active_set.contains(&id) && seen.insert(id) {
                ordered.push(id);
            }
        }

        for id in active {
            if seen.insert(id) {
                ordered.push(id);
            }
        }

        ordered.sort_by_key(|id| {
            Self::transparent_repack_priority(subs.routing_preference(id).unwrap_or_default())
        });
        ordered
    }

    /// Maps transparent routing preference to active-repack priority.
    fn transparent_repack_priority(preference: RelayRoutingPreference) -> u8 {
        match preference {
            RelayRoutingPreference::RequireDedicated => 0,
            RelayRoutingPreference::PreferDedicated => 1,
            RelayRoutingPreference::NoPreference => 2,
        }
    }

    fn repack_dedicated_for_current_limits(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> CoordinationOutput {
        let active = self.active_transparent_repack_order(subs);
        if active.is_empty() {
            return CoordinationOutput::empty();
        }

        let mut output = CoordinationOutput::empty();
        let mut fallback_compaction = CompactionOperationPlan::default();
        let mut demoted_in_current_pass = HashSet::new();
        for id in active {
            if demoted_in_current_pass.contains(&id) {
                continue;
            }

            if subs.stored_ref(&id).is_none() {
                output.extend(self.execute_route_cleanup(subs, RouteCleanup::Transparent(id)));
                continue;
            }

            output.extend(
                self.route_transparent_request_with_fallback(
                    subs,
                    &mut fallback_compaction,
                    &mut demoted_in_current_pass,
                    id,
                )
                .output,
            );
        }

        if !fallback_compaction.is_empty() {
            output.extend(self.apply_compaction_plan(subs, fallback_compaction));
        }

        output
    }

    fn empty_output_when_subscription_ids_unsupported(&self) -> Option<CoordinationOutput> {
        if !self.supports_relay_subscription_ids() {
            return Some(CoordinationOutput::empty());
        }

        None
    }

    #[profiling::function]
    fn route_subscription(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        if self.routes.route_type(&id) == Some(RelayType::Compaction) {
            output.extend(self.compaction_unsubscribe(subs, id));
        }

        output.extend(self.route_transparent_request(subs, id));
        output
    }

    fn cleanup_existing_route_for_replace(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let Some(current_route) = self.routes.route_type(&id) else {
            return CoordinationOutput::empty();
        };

        match current_route {
            RelayType::Transparent => {
                self.execute_route_cleanup(subs, RouteCleanup::Transparent(id))
            }
            RelayType::Compaction => self.execute_route_cleanup(subs, RouteCleanup::Compaction(id)),
        }
    }

    fn cleanup_existing_route_for_unsubscribe(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        self.cleanup_existing_route_for_replace(subs, id)
    }

    #[profiling::function]
    fn route_transparent_request(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        let mut needs_capacity = false;
        let mut placed_count = 0usize;
        let mut fallback_count = 0usize;
        let mut queued_count = 0usize;
        let mut skipped_count = 0usize;

        let probe = self.probe_transparent_request(subs, id);
        match probe.outcome {
            ProbeTransparentRouteOutcome::Placed => placed_count += 1,
            ProbeTransparentRouteOutcome::NeedsCapacity => needs_capacity = true,
            ProbeTransparentRouteOutcome::Skipped => skipped_count += 1,
        }
        output.extend(probe.output);

        if needs_capacity {
            if let Some(reserve_count) = self.transparent_passes_needed(subs, id) {
                if reserve_count > 0 {
                    output.extend(self.compaction_request_free_subs(subs, reserve_count));
                }
            }

            let mut fallback_compaction = CompactionOperationPlan::default();
            let mut demoted_in_this_pass = HashSet::new();
            let result = self.route_transparent_request_with_fallback(
                subs,
                &mut fallback_compaction,
                &mut demoted_in_this_pass,
                id,
            );
            match result.outcome {
                FallbackTransparentRouteOutcome::Placed
                | FallbackTransparentRouteOutcome::Preserved => placed_count += 1,
                FallbackTransparentRouteOutcome::Fallback => fallback_count += 1,
                FallbackTransparentRouteOutcome::Queued => queued_count += 1,
                FallbackTransparentRouteOutcome::Skipped => skipped_count += 1,
            }
            output.extend(result.output);

            output.extend(self.apply_compaction_plan(subs, fallback_compaction));
            tracing::trace!(
                requested = 1usize,
                placed_count,
                needs_capacity_count = usize::from(needs_capacity),
                fallback_count,
                queued_count,
                skipped_count,
                demotion_count = demoted_in_this_pass.len(),
                available_after = self.limits.sub_guardian.available_passes(),
                "transparent routing pass complete"
            );
        }
        output
    }

    fn log_sub_pass_usage(&self) {
        tracing::trace!(
            "Using {} of {} subs",
            self.limits.sub_guardian.total_passes() - self.limits.sub_guardian.available_passes(),
            self.limits.sub_guardian.total_passes()
        );
    }

    #[profiling::function]
    fn subscribe_with_supported_subscription_ids(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = self.route_subscription(subs, id);
        output.extend(self.apply_capacity_available(subs));
        self.assert_route_consistency();
        self.log_sub_pass_usage();
        output
    }

    #[profiling::function]
    fn replace_subscribe_with_supported_subscription_ids(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = self.cleanup_existing_route_for_replace(subs, id);
        output.extend(self.route_transparent_request(subs, id));
        output.extend(self.apply_capacity_available(subs));
        self.assert_route_consistency();
        self.log_sub_pass_usage();
        output
    }

    #[profiling::function]
    fn unsubscribe_with_supported_subscription_ids(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = self.cleanup_existing_route_for_unsubscribe(subs, id);
        output.extend(self.apply_capacity_available(subs));
        self.assert_route_consistency();
        self.log_sub_pass_usage();
        output
    }

    /// Route one subscription through this relay according to its retained
    /// routing preference.
    pub(crate) fn subscribe(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        if let Some(result) = self.empty_output_when_subscription_ids_unsupported() {
            return result;
        }

        self.subscribe_with_supported_subscription_ids(subs, id)
    }

    /// Replace any current relay route for one subscription, then route it
    /// again according to its retained routing preference.
    pub(crate) fn replace_subscribe(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        if let Some(result) = self.empty_output_when_subscription_ids_unsupported() {
            return result;
        }

        self.replace_subscribe_with_supported_subscription_ids(subs, id)
    }

    /// Remove one subscription from whichever relay engine owns it.
    pub(crate) fn unsubscribe(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        if let Some(result) = self.empty_output_when_subscription_ids_unsupported() {
            return result;
        }

        self.unsubscribe_with_supported_subscription_ids(subs, id)
    }

    /// Attempts dedicated placement during the first probe pass without
    /// demotion or compaction fallback.
    fn probe_transparent_request(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> ProbeTransparentRouteResult {
        let Some(_) = subs.stored_ref(&id) else {
            return ProbeTransparentRouteResult {
                outcome: ProbeTransparentRouteOutcome::Skipped,
                output: CoordinationOutput::empty(),
            };
        };

        match self.transparent_placement_feasibility(subs, id) {
            Some(TransparentPlacementFeasibility::Ready) => {}
            Some(TransparentPlacementFeasibility::NeedsCapacity { .. }) => {
                return ProbeTransparentRouteResult {
                    outcome: ProbeTransparentRouteOutcome::NeedsCapacity,
                    output: CoordinationOutput::empty(),
                };
            }
            Some(TransparentPlacementFeasibility::Unrepresentable) => {
                return ProbeTransparentRouteResult {
                    outcome: ProbeTransparentRouteOutcome::NeedsCapacity,
                    output: CoordinationOutput::empty(),
                };
            }
            None => {
                return ProbeTransparentRouteResult {
                    outcome: ProbeTransparentRouteOutcome::Skipped,
                    output: CoordinationOutput::empty(),
                };
            }
        }

        let placed = self.try_place_dedicated_route(subs, id);
        let result = placed.result.expect("checked view above");

        let outcome = match result {
            TransparentPlaceResult::Placed => ProbeTransparentRouteOutcome::Placed,
            TransparentPlaceResult::NoRoom => ProbeTransparentRouteOutcome::NeedsCapacity,
        };
        ProbeTransparentRouteResult {
            outcome,
            output: placed.output,
        }
    }

    /// Materializes one dedicated route immediately and updates coordinator
    /// ownership if placed.
    fn try_place_dedicated_route(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> TransparentPlaceAttempt {
        let Some(view) = subs.stored_ref(&id) else {
            return TransparentPlaceAttempt {
                result: None,
                output: CoordinationOutput::empty(),
            };
        };
        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        match self.transparent_data.placement_feasibility(
            &view,
            limits,
            self.limits.sub_guardian.available_passes(),
        ) {
            TransparentPlacementFeasibility::Ready => {}
            TransparentPlacementFeasibility::NeedsCapacity { .. }
            | TransparentPlacementFeasibility::Unrepresentable => {
                return TransparentPlaceAttempt {
                    result: Some(TransparentPlaceResult::NoRoom),
                    output: CoordinationOutput::empty(),
                };
            }
        }

        let pass = if self
            .transparent_data
            .pass_deficit(&view, limits)
            .unwrap_or_default()
            > 0
        {
            Some(
                self.limits
                    .sub_guardian
                    .take_pass()
                    .expect("checked transparent pass capacity"),
            )
        } else {
            None
        };
        let placed =
            self.transparent_data
                .try_subscribe(self.current_generation, pass, limits, view);
        return_subpasses(&mut self.limits.sub_guardian, placed.returned_passes);

        let mut output = CoordinationOutput::new(
            CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new()),
            placed.frames,
        );
        if matches!(placed.result, TransparentPlaceResult::Placed) {
            output.extend(self.set_transparent_route(subs, id));
        }

        TransparentPlaceAttempt {
            result: Some(placed.result),
            output,
        }
    }

    pub(super) fn transparent_passes_needed(
        &self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> Option<usize> {
        let view = subs.stored_ref(&id)?;
        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        match self
            .transparent_data
            .placement_feasibility(&view, limits, 0)
        {
            TransparentPlacementFeasibility::Ready => Some(0),
            TransparentPlacementFeasibility::NeedsCapacity { pass_deficit } => Some(pass_deficit),
            TransparentPlacementFeasibility::Unrepresentable => None,
        }
    }

    /// Returns transparent feasibility separated from pass availability.
    fn transparent_placement_feasibility(
        &self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> Option<TransparentPlacementFeasibility> {
        let view = subs.stored_ref(&id)?;
        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        Some(self.transparent_data.placement_feasibility(
            &view,
            limits,
            self.limits.sub_guardian.available_passes(),
        ))
    }

    /// Attempts dedicated placement during the fallback-enabled pass.
    ///
    /// Demotion is planned in `SubPass` deficits so a placement either receives
    /// enough lower-priority capacity in one decision or falls through to the
    /// existing unplaced policy.
    fn route_transparent_request_with_fallback(
        &mut self,
        subs: &OutboxSubscriptions,
        fallback_compaction: &mut CompactionOperationPlan,
        demoted_in_current_pass: &mut HashSet<OutboxSubId>,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteResult {
        let mut output = CoordinationOutput::empty();
        let Some(_) = subs.stored_ref(&id) else {
            return FallbackTransparentRouteResult {
                outcome: FallbackTransparentRouteOutcome::Skipped,
                output,
            };
        };
        let policy = subs.routing_preference(&id).unwrap_or_default();

        let Some(feasibility) = self.transparent_placement_feasibility(subs, id) else {
            debug_assert!(
                false,
                "stored subscription disappeared while deriving transparent feasibility"
            );
            return FallbackTransparentRouteResult {
                outcome: FallbackTransparentRouteOutcome::Skipped,
                output,
            };
        };
        let pass_deficit = match feasibility {
            TransparentPlacementFeasibility::Ready => 0,
            TransparentPlacementFeasibility::NeedsCapacity { pass_deficit } => pass_deficit,
            TransparentPlacementFeasibility::Unrepresentable => {
                return self.handle_unrepresentable_transparent_request(
                    policy,
                    subs,
                    fallback_compaction,
                    id,
                );
            }
        };
        if pass_deficit > 0 {
            let Some(demotions) = self.select_transparent_fallback_demotions(
                policy,
                subs,
                id,
                demoted_in_current_pass,
                pass_deficit,
            ) else {
                let result =
                    self.handle_unplaced_transparent_request(policy, subs, fallback_compaction, id);
                output.extend(result.output);
                return FallbackTransparentRouteResult {
                    outcome: result.outcome,
                    output,
                };
            };

            output.extend(self.apply_transparent_fallback_demotions(
                subs,
                fallback_compaction,
                demoted_in_current_pass,
                &demotions,
            ));
        }

        let placed_after_capacity_change = self.try_place_dedicated_route(subs, id);
        output.extend(placed_after_capacity_change.output);
        let Some(placed_after_capacity_change) = placed_after_capacity_change.result else {
            debug_assert!(
                false,
                "stored subscription disappeared while placing transparent route"
            );
            return FallbackTransparentRouteResult {
                outcome: FallbackTransparentRouteOutcome::Skipped,
                output,
            };
        };

        if matches!(placed_after_capacity_change, TransparentPlaceResult::Placed) {
            return FallbackTransparentRouteResult {
                outcome: FallbackTransparentRouteOutcome::Placed,
                output,
            };
        }

        let result =
            self.handle_unplaced_transparent_request(policy, subs, fallback_compaction, id);
        output.extend(result.output);
        FallbackTransparentRouteResult {
            outcome: result.outcome,
            output,
        }
    }

    /// Selects enough lower-priority transparent routes to satisfy `pass_deficit`.
    ///
    /// The selection is all-or-nothing: if the demotable candidates cannot free
    /// enough active transparent legs, no route is selected.
    fn select_transparent_fallback_demotions(
        &self,
        policy: RelayRoutingPreference,
        subs: &OutboxSubscriptions,
        incoming: OutboxSubId,
        demoted_in_current_pass: &HashSet<OutboxSubId>,
        pass_deficit: usize,
    ) -> Option<Vec<OutboxSubId>> {
        if pass_deficit == 0 {
            return Some(Vec::new());
        }

        let candidates = self.routes.limit_reduction_candidates();
        let mut selected = Vec::new();
        let mut selected_ids = HashSet::new();
        let mut freed_passes = 0usize;

        for candidate_policy in Self::transparent_fallback_demotion_order(policy) {
            for candidate in candidates.iter().copied() {
                if candidate == incoming
                    || demoted_in_current_pass.contains(&candidate)
                    || selected_ids.contains(&candidate)
                {
                    continue;
                }

                let Some(owner_ids) = self.transparent_data.owner_ids_for(&candidate) else {
                    continue;
                };
                if owner_ids.contains(&incoming)
                    || owner_ids
                        .iter()
                        .any(|owner| demoted_in_current_pass.contains(owner))
                    || owner_ids.iter().any(|owner| selected_ids.contains(owner))
                {
                    continue;
                }

                let Some(shared_preference) =
                    self.shared_transparent_route_preference(subs, candidate)
                else {
                    continue;
                };
                if shared_preference != *candidate_policy {
                    continue;
                }

                if owner_ids.iter().any(|owner| {
                    self.compaction_limit_fit(subs, *owner) != Some(CompactionLimitFit::Compactable)
                }) {
                    continue;
                }

                let mut owner_ids = owner_ids.into_iter().collect::<Vec<_>>();
                owner_ids.sort_unstable();
                for owner in owner_ids {
                    selected_ids.insert(owner);
                    selected.push(owner);
                }
                freed_passes += 1;
                if freed_passes >= pass_deficit {
                    return Some(selected);
                }
            }
        }

        None
    }

    /// Returns transparent route preferences that may yield to `policy`.
    fn transparent_fallback_demotion_order(
        policy: RelayRoutingPreference,
    ) -> &'static [RelayRoutingPreference] {
        const REQUIRED_DEMOTION_ORDER: &[RelayRoutingPreference] = &[
            RelayRoutingPreference::NoPreference,
            RelayRoutingPreference::PreferDedicated,
        ];
        const PREFERRED_DEMOTION_ORDER: &[RelayRoutingPreference] =
            &[RelayRoutingPreference::NoPreference];
        const NO_DEMOTION_ORDER: &[RelayRoutingPreference] = &[];

        match policy {
            RelayRoutingPreference::RequireDedicated => REQUIRED_DEMOTION_ORDER,
            RelayRoutingPreference::PreferDedicated => PREFERRED_DEMOTION_ORDER,
            RelayRoutingPreference::NoPreference => NO_DEMOTION_ORDER,
        }
    }

    /// Applies selected transparent demotions and schedules their compaction fallback.
    fn apply_transparent_fallback_demotions(
        &mut self,
        subs: &OutboxSubscriptions,
        fallback_compaction: &mut CompactionOperationPlan,
        demoted_in_current_pass: &mut HashSet<OutboxSubId>,
        demotions: &[OutboxSubId],
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        if demotions.is_empty() {
            return output;
        }

        for demoted in demotions {
            output.extend(self.transparent_unsubscribe(*demoted));
        }

        for demoted in demotions {
            output.extend(self.set_compaction_route(subs, *demoted));
            fallback_compaction.sub(*demoted);
            demoted_in_current_pass.insert(*demoted);
        }
        output
    }

    /// Handles a transparent route whose filters cannot be represented by one REQ.
    fn handle_unrepresentable_transparent_request(
        &mut self,
        policy: RelayRoutingPreference,
        subs: &OutboxSubscriptions,
        fallback_compaction: &mut CompactionOperationPlan,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteResult {
        if self.dedicated_active_leg_count(&id) == 0 {
            return self.handle_unplaced_transparent_request(policy, subs, fallback_compaction, id);
        }

        match policy {
            RelayRoutingPreference::RequireDedicated => self.queue_dedicated_retry(subs, id),
            RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::NoPreference => {
                FallbackTransparentRouteResult {
                    outcome: FallbackTransparentRouteOutcome::Preserved,
                    output: CoordinationOutput::empty(),
                }
            }
        }
    }

    /// Returns whether compaction can immediately replace `id` after its active
    /// transparent legs return their passes.
    fn compaction_fallback_can_replace_transparent_route(
        &self,
        subs: &OutboxSubscriptions,
        fallback_compaction: &CompactionOperationPlan,
        id: OutboxSubId,
    ) -> bool {
        let available_passes_after_unsubscribe = self
            .limits
            .sub_guardian
            .available_passes()
            .saturating_add(self.dedicated_active_leg_count(&id));

        self.compaction_data.can_place_subscribes_with_passes(
            subs,
            fallback_compaction.subscribe_ids().chain([id]),
            ReqFilterLimits::from_relay_limits(&self.limits),
            available_passes_after_unsubscribe,
        )
    }

    fn handle_unplaced_transparent_request(
        &mut self,
        policy: RelayRoutingPreference,
        subs: &OutboxSubscriptions,
        fallback_compaction: &mut CompactionOperationPlan,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteResult {
        match policy {
            RelayRoutingPreference::RequireDedicated => self.queue_dedicated_retry(subs, id),
            RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::NoPreference => {
                let Some(feasibility) = self.transparent_placement_feasibility(subs, id) else {
                    debug_assert!(
                        false,
                        "stored subscription disappeared while handling unplaced transparent route"
                    );
                    return FallbackTransparentRouteResult {
                        outcome: FallbackTransparentRouteOutcome::Skipped,
                        output: CoordinationOutput::empty(),
                    };
                };
                if matches!(
                    feasibility,
                    TransparentPlacementFeasibility::Unrepresentable
                ) {
                    return self.queue_dedicated_retry(subs, id);
                }

                let compactable =
                    self.compaction_limit_fit(subs, id) == Some(CompactionLimitFit::Compactable);
                if !compactable {
                    return self.queue_dedicated_retry(subs, id);
                }

                if self.dedicated_active_leg_count(&id) > 0
                    && !self.compaction_fallback_can_replace_transparent_route(
                        subs,
                        fallback_compaction,
                        id,
                    )
                {
                    return FallbackTransparentRouteResult {
                        outcome: FallbackTransparentRouteOutcome::Preserved,
                        output: CoordinationOutput::empty(),
                    };
                }

                // Dedicated routing is best-effort for non-required requests; when saturated,
                // fallback this request to compaction.
                let mut output = self.transparent_unsubscribe(id);
                output.extend(self.set_compaction_route(subs, id));
                fallback_compaction.sub(id);
                FallbackTransparentRouteResult {
                    outcome: FallbackTransparentRouteOutcome::Fallback,
                    output,
                }
            }
        }
    }

    /// Queues a dedicated request for retry without compaction fallback.
    fn queue_dedicated_retry(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteResult {
        if subs.stored_ref(&id).is_none() {
            debug_assert!(
                false,
                "stored subscription disappeared while queueing dedicated retry"
            );
            return FallbackTransparentRouteResult {
                outcome: FallbackTransparentRouteOutcome::Skipped,
                output: CoordinationOutput::empty(),
            };
        }
        let preserve_active_route = self.dedicated_active_leg_count(&id) > 0;
        self.transparent_data.queue_subscribe(id);
        let mut output = CoordinationOutput::empty();
        if preserve_active_route {
            output.relay_demand = self.refresh_transparent_route(subs, id);
        } else {
            output.extend(self.set_transparent_route(subs, id));
        }
        FallbackTransparentRouteResult {
            outcome: FallbackTransparentRouteOutcome::Queued,
            output,
        }
    }

    fn dedicated_active_leg_count(&self, req_id: &OutboxSubId) -> usize {
        self.transparent_data.active_leg_count(req_id)
    }

    fn dedicated_req_status(&self, req_id: &OutboxSubId) -> Option<RelayReqStatus> {
        self.transparent_data.req_status(req_id)
    }

    fn transparent_unsubscribe(&mut self, id: OutboxSubId) -> CoordinationOutput {
        self.transparent_unsubscribe_with_generation(id, self.current_generation)
    }

    fn transparent_unsubscribe_without_relay_frames(
        &mut self,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        self.transparent_unsubscribe_with_generation(id, None)
    }

    fn transparent_unsubscribe_with_generation(
        &mut self,
        id: OutboxSubId,
        current_generation: Option<u64>,
    ) -> CoordinationOutput {
        let output = self.transparent_data.unsubscribe(current_generation, id);
        return_subpasses(&mut self.limits.sub_guardian, output.returned_passes);
        CoordinationOutput::new(
            CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new()),
            output.frames,
        )
    }

    /// Marks `id` as transparently routed and clears any pending compaction
    /// promotion candidate.
    fn set_transparent_route(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::from_facts(CoordinationFacts::invalidated_sub_id(id));
        output.relay_demand = self.refresh_transparent_route(subs, id);
        output
    }

    /// Refreshes transparent route ownership and demotion indexes without
    /// changing the active relay leg.
    fn refresh_transparent_route(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> Option<Option<RelayTransportDemand>> {
        self.preferred_compaction_promotions.remove(id);
        let view = subs.stored_ref(&id);
        let routing_preference = view
            .as_ref()
            .map(|view| view.routing_preference)
            .unwrap_or_default();
        self.routes.set_transparent_route(id, routing_preference);
        self.set_relay_demand_entry(id, view)
    }

    /// Marks `id` as compaction-routed and indexes it for future promotion when
    /// its preference is `PreferDedicated`.
    fn set_compaction_route(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        self.routes.set_compaction_route(id);

        let view = subs.stored_ref(&id);
        let routing_preference = view
            .as_ref()
            .map(|view| view.routing_preference)
            .unwrap_or_default();
        if routing_preference == RelayRoutingPreference::PreferDedicated {
            self.preferred_compaction_promotions
                .push_back_if_missing(id);
        } else {
            self.preferred_compaction_promotions.remove(id);
        }
        let mut output = CoordinationOutput::from_facts(CoordinationFacts::invalidated_sub_id(id));
        output.relay_demand = self.set_relay_demand_entry(id, view);
        output
    }

    /// Removes all coordinator ownership for `id` and clears promotion state.
    fn clear_route(&mut self, id: OutboxSubId) -> Option<Option<RelayTransportDemand>> {
        self.preferred_compaction_promotions.remove(id);
        self.routes.clear_route(id);
        self.clear_relay_demand_entry(id)
    }

    /// Applies cleanup returned by coordinator decision code.
    ///
    /// Relay-engine state is removed before coordinator ownership is cleared.
    fn execute_route_cleanup(
        &mut self,
        subs: &OutboxSubscriptions,
        cleanup: RouteCleanup,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        match cleanup {
            RouteCleanup::Transparent(id) => {
                output.extend(self.transparent_unsubscribe(id));
                output.extend(self.finish_route_cleanup(id));
            }
            RouteCleanup::Compaction(id) => {
                output.extend(self.compaction_unsubscribe(subs, id));
                output.extend(self.finish_route_cleanup(id));
            }
            RouteCleanup::RouteOnly(id) => {
                output.extend(self.finish_route_cleanup(id));
            }
        }
        output
    }

    /// Removes local route ownership after the relay has already closed the SID.
    ///
    /// This returns transparent/compaction capacity and emits normal read-model
    /// cleanup facts, but does not send `CLOSE` for the already-closed relay SID.
    pub(crate) fn remove_subscription_after_relay_closed(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        if let Some(result) = self.empty_output_when_subscription_ids_unsupported() {
            return result;
        }

        let mut output = self.cleanup_existing_route_after_relay_closed(id);
        output.extend(self.apply_capacity_available(subs));
        self.assert_route_consistency();
        self.log_sub_pass_usage();
        output
    }

    fn cleanup_existing_route_after_relay_closed(&mut self, id: OutboxSubId) -> CoordinationOutput {
        let Some(current_route) = self.routes.route_type(&id) else {
            return CoordinationOutput::empty();
        };

        let mut output = CoordinationOutput::empty();
        match current_route {
            RelayType::Transparent => {
                output.extend(self.transparent_unsubscribe_without_relay_frames(id));
                output.extend(self.finish_route_cleanup(id));
            }
            RelayType::Compaction => {
                output.extend(self.remove_compaction_after_relay_closed(id));
                output.extend(self.finish_route_cleanup(id));
            }
        }
        output
    }

    fn finish_route_cleanup(&mut self, id: OutboxSubId) -> CoordinationOutput {
        let mut output = CoordinationOutput::from_facts(CoordinationFacts::invalidated_sub_id(id));
        output.relay_demand = self.clear_route(id);
        output
    }

    /// Returns the oldest still-valid preferred compaction candidate that can fit now.
    fn pop_preferred_compaction_candidate(
        &mut self,
        subs: &OutboxSubscriptions,
        available_passes: usize,
    ) -> Option<PreferredCompactionPromotion> {
        for id in self
            .preferred_compaction_promotions
            .iter()
            .collect::<Vec<_>>()
        {
            if self.routes.route_type(&id) != Some(RelayType::Compaction) {
                self.preferred_compaction_promotions.remove(id);
                continue;
            }

            if subs.stored_ref(&id).is_none() {
                return Some(PreferredCompactionPromotion::Cleanup(
                    RouteCleanup::Compaction(id),
                ));
            }

            if subs.routing_preference(&id) != Some(RelayRoutingPreference::PreferDedicated) {
                self.preferred_compaction_promotions.remove(id);
                continue;
            }

            let Some(pass_deficit) = self.transparent_passes_needed(subs, id) else {
                continue;
            };
            if pass_deficit > available_passes {
                continue;
            }

            self.preferred_compaction_promotions.remove(id);
            return Some(PreferredCompactionPromotion::Promote { id, pass_deficit });
        }

        None
    }

    /// Promotes compaction-routed preferred subscriptions into dedicated slots
    /// using any leftover pass capacity after current-session work completes.
    #[profiling::function]
    fn promote_preferred_compaction_routes(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        let mut available = self.limits.sub_guardian.available_passes();
        if available == 0 {
            return output;
        }

        while available > 0 {
            match self.pop_preferred_compaction_candidate(subs, available) {
                Some(PreferredCompactionPromotion::Promote { id, pass_deficit }) => {
                    output.extend(self.promote_one_preferred_compaction_route(subs, id));
                    available = available.saturating_sub(pass_deficit);
                }
                Some(PreferredCompactionPromotion::Cleanup(cleanup)) => {
                    output.extend(self.execute_route_cleanup(subs, cleanup));
                }
                None => break,
            }
        }

        self.assert_route_consistency();
        output
    }

    fn promote_one_preferred_compaction_route(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        let mut output = CoordinationOutput::empty();
        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        let granted_passes = take_available_subpasses(&mut self.limits.sub_guardian);
        let transition = self.compaction_data.unsubscribe(
            self.current_generation,
            limits,
            granted_passes,
            subs,
            id,
        );
        output.extend(self.apply_compaction_transition(transition));

        let Some(_) = subs.stored_ref(&id) else {
            output.extend(self.execute_route_cleanup(subs, RouteCleanup::RouteOnly(id)));
            return output;
        };

        let placed = self.try_place_dedicated_route(subs, id);
        output.extend(placed.output);
        let placed = placed.result.expect("checked view above");

        if matches!(placed, TransparentPlaceResult::Placed) {
            return output;
        }

        output.extend(self.set_compaction_route(subs, id));
        let mut restore_compaction = CompactionOperationPlan::default();
        restore_compaction.sub(id);

        output.extend(self.apply_compaction_plan(subs, restore_compaction));
        output
    }

    /// Flushes queued dedicated retries through coordinator-owned placement policy.
    #[profiling::function]
    fn drain_transparent_retry_queue(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> TransparentRetryDrainResult {
        let mut output = CoordinationOutput::empty();
        if self.transparent_data.queued_len() == 0 {
            return TransparentRetryDrainResult {
                drain: TransparentRetryDrain::Drained,
                output,
            };
        }

        let mut fallback_compaction = CompactionOperationPlan::default();
        let mut demoted_in_current_pass = HashSet::new();
        let mut attempted = HashSet::new();
        let mut made_progress = false;
        while let Some(id) = self.transparent_data.pop_queued_retry() {
            if !attempted.insert(id) {
                output.extend(self.queue_dedicated_retry(subs, id).output);
                return TransparentRetryDrainResult {
                    drain: TransparentRetryDrain::Blocked {
                        reason: TransparentRetryBlockedReason::NoPlacementProgress,
                    },
                    output,
                };
            }

            if subs.stored_ref(&id).is_none() {
                output.extend(self.execute_route_cleanup(subs, RouteCleanup::Transparent(id)));
                made_progress = true;
                continue;
            }

            let result = self.route_transparent_request_with_fallback(
                subs,
                &mut fallback_compaction,
                &mut demoted_in_current_pass,
                id,
            );
            output.extend(result.output);
            made_progress |= !matches!(
                result.outcome,
                FallbackTransparentRouteOutcome::Queued | FallbackTransparentRouteOutcome::Skipped
            );
        }

        if fallback_compaction.is_empty() {
            self.assert_route_consistency();
            return TransparentRetryDrainResult {
                drain: transparent_retry_drain_result(
                    made_progress,
                    self.transparent_data.queued_len(),
                ),
                output,
            };
        }

        output.extend(self.apply_compaction_plan(subs, fallback_compaction));
        self.assert_route_consistency();
        TransparentRetryDrainResult {
            drain: transparent_retry_drain_result(true, self.transparent_data.queued_len()),
            output,
        }
    }

    fn apply_available_compaction_capacity_inner(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> CompactionCapacityResult {
        if !self.compaction_data.has_queued_subs() {
            return CompactionCapacityResult {
                output: CoordinationOutput::empty(),
                queue: CompactionCapacityProgress {
                    still_queued: false,
                    made_progress: false,
                    blocked_reason: None,
                },
            };
        }

        let limits = ReqFilterLimits::from_relay_limits(&self.limits);
        let granted_passes = take_available_subpasses(&mut self.limits.sub_guardian);
        let outcome = self.compaction_data.apply_granted_capacity(
            self.current_generation,
            limits,
            granted_passes,
            subs,
        );
        let CompactionCapacityOutcome {
            transition,
            still_queued,
            made_progress,
            blocked_reason,
        } = outcome;
        CompactionCapacityResult {
            output: self.apply_compaction_transition(transition),
            queue: CompactionCapacityProgress {
                still_queued,
                made_progress,
                blocked_reason,
            },
        }
    }

    /// Returns whether subscription-id-bearing protocols may use this relay.
    pub(crate) fn supports_relay_subscription_ids(&self) -> bool {
        self.unsupported_capability.is_none()
    }

    /// Disables relay subscription-id-bearing protocols and drops relay-local
    /// state without sending invalid close frames.
    pub(crate) fn mark_subscription_id_length_unsupported(
        &mut self,
        max_subid_length: usize,
    ) -> CoordinationOutput {
        self.unsupported_capability =
            Some(UnsupportedRelayCapability::SubscriptionIdLength { max_subid_length });

        let mut affected = self.routes.route_ids().into_iter().collect::<HashSet<_>>();
        let transparent_clear = self.transparent_data.clear_without_closing();
        return_subpasses(
            &mut self.limits.sub_guardian,
            transparent_clear.returned_passes,
        );
        affected.extend(transparent_clear.affected);
        let compaction_clear = self.compaction_data.clear_without_closing();
        return_subpasses(
            &mut self.limits.sub_guardian,
            compaction_clear.returned_passes,
        );
        affected.extend(compaction_clear.invalidated_sub_ids);
        for id in &affected {
            self.clear_route(*id);
        }
        self.assert_route_consistency();
        let mut output = CoordinationOutput::from_facts(CoordinationFacts::new(
            RelayEoseDelta::default(),
            affected,
        ));
        output.extend(CoordinationOutput::from_negentropy_effect(
            NegentropyRelayEffect::DropSessionsWithoutNegClose,
        ));
        self.relay_demand_entries.clear();
        self.relay_demand = None;
        output.relay_demand = Some(None);
        output
    }

    /// Returns the current request status for `id` if this coordinator still
    /// owns a relay leg for that subscription.
    pub fn req_status(&self, id: &OutboxSubId) -> Option<RelayReqStatus> {
        match self.routes.route_type(id)? {
            RelayType::Compaction => self.compaction_data.req_status(id),
            RelayType::Transparent => self.transparent_data.req_status(id),
        }
    }

    /// Returns which relay engine currently owns this subscription, if any.
    pub(crate) fn route_type(&self, id: &OutboxSubId) -> Option<RelayType> {
        self.routes.route_type(id)
    }

    /// Returns how many transparent retry entries are currently queued.
    #[cfg(test)]
    pub(crate) fn transparent_queue_len_for_test(&self) -> usize {
        self.transparent_data.queued_len_for_test()
    }

    /// Returns the active transparent relay subscription id for test assertions.
    #[cfg(test)]
    pub(crate) fn active_transparent_sid_for_test(&self, id: &OutboxSubId) -> Option<RelayReqId> {
        self.transparent_data.active_sid(id)
    }

    /// Returns the active compaction relay subscription id for test assertions.
    #[cfg(test)]
    pub(crate) fn active_compaction_sid_for_test(&self, id: &OutboxSubId) -> Option<RelayReqId> {
        self.compaction_data.active_sid_for_test(id)
    }

    fn url(&self) -> &str {
        ""
    }

    /// Tear down the current websocket leg and requeue any relay-local
    /// negentropy work that was in flight on it.
    pub(crate) fn disconnect_websocket_leg_at(&mut self) -> CoordinationOutput {
        let Some(_generation) = self.current_generation.take() else {
            return CoordinationOutput::empty();
        };
        CoordinationOutput::from_negentropy_effect(NegentropyRelayEffect::RelayDisconnect)
    }

    /// Evict the current websocket leg while preserving relay-local
    /// coordination state for later replay.
    pub(crate) fn evict_websocket_leg_at(&mut self) -> CoordinationOutput {
        self.disconnect_websocket_leg_at()
    }

    pub(crate) fn apply_websocket_opened(
        &mut self,
        subs: &OutboxSubscriptions,
        _reconnect_delay: Duration,
        generation: u64,
    ) -> RecvResponse {
        let replay_subscription_reqs = self.supports_relay_subscription_ids();
        self.current_generation = Some(generation);
        let mut resp = RecvResponse::received();
        resp.websocket_open_transition = Some(WebsocketOpenTransition::Opened);
        resp.output
            .extend(self.replay_after_websocket_open(subs, replay_subscription_reqs));
        resp
    }

    pub(crate) fn apply_websocket_closed(&mut self, generation: u64) -> RecvResponse {
        if !self.websocket_generation_matches(generation) {
            return RecvResponse::default();
        }
        let output = self.disconnect_websocket_leg_at();
        let mut resp = RecvResponse::received();
        resp.websocket_closed = true;
        resp.output.extend(output);
        resp
    }

    pub(crate) fn apply_websocket_error(&mut self, generation: u64, err: String) -> RecvResponse {
        if !self.websocket_generation_matches(generation) {
            return RecvResponse::default();
        }
        let was_connecting = true;
        let mut resp = RecvResponse::received();
        if was_connecting {
            resp.websocket_open_transition =
                Some(WebsocketOpenTransition::Failed(err.clone().into()));
        }
        resp.websocket_transport_failure = true;
        tracing::error!("relay {} error: {:?}", self.url(), err);
        let output = self.disconnect_websocket_leg_at();
        resp.output.extend(output);
        resp
    }

    pub(crate) fn apply_websocket_pong(&mut self, generation: u64) -> bool {
        if !self.websocket_generation_matches(generation) {
            return false;
        }
        true
    }

    pub(crate) fn apply_relay_eose(&mut self, generation: u64, sid: &str) -> RecvResponse {
        if !self.websocket_generation_matches(generation) {
            return RecvResponse::default();
        }
        self.handle_relay_eose(sid)
    }

    pub(crate) fn apply_relay_closed(&mut self, generation: u64, sid: &str) -> RecvResponse {
        if !self.websocket_generation_matches(generation) {
            return RecvResponse::default();
        }
        self.handle_relay_closed_sid(sid)
    }

    fn websocket_generation_matches(&self, generation: u64) -> bool {
        self.current_generation == Some(generation)
    }

    /// Replay relay-local outbound work after a websocket reconnect.
    #[profiling::function]
    fn replay_after_websocket_open(
        &mut self,
        subs: &OutboxSubscriptions,
        replay_subscription_reqs: bool,
    ) -> CoordinationOutput {
        let (effects, frames) = collect_relay_open_replay_effects(
            self.current_generation,
            &mut self.compaction_data,
            &mut self.transparent_data,
            &mut self.limits,
            subs,
            replay_subscription_reqs,
        );
        if !replay_subscription_reqs {
            return CoordinationOutput::empty();
        }

        let invalidated_sub_ids = effects.invalidated_sub_ids;
        let mut output = CoordinationOutput::from_invalidated_sub_ids(invalidated_sub_ids, frames);
        output.extend(self.queue_blocked_transparent_replay_ids(effects.blocked_transparent_ids));

        for id in effects.released_compaction_ids {
            output.extend(self.route_released_compaction_replay_id(subs, id));
        }
        output.extend(self.apply_capacity_available(subs));
        self.assert_route_consistency();
        output
    }

    fn queue_blocked_transparent_replay_ids(
        &mut self,
        ids: HashSet<OutboxSubId>,
    ) -> CoordinationOutput {
        if ids.is_empty() {
            return CoordinationOutput::empty();
        }

        for id in ids {
            self.transparent_data.queue_subscribe(id);
        }
        CoordinationOutput::new(
            CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new()),
            Vec::new(),
        )
    }

    fn route_released_compaction_replay_id(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CoordinationOutput {
        if subs.stored_ref(&id).is_none() {
            return self.execute_route_cleanup(subs, RouteCleanup::RouteOnly(id));
        }

        let placed = self.try_place_dedicated_route(subs, id);
        let mut output = placed.output;
        match placed.result {
            Some(TransparentPlaceResult::Placed) => {}
            Some(TransparentPlaceResult::NoRoom) => {
                output.extend(self.queue_dedicated_retry(subs, id).output);
            }
            None => {
                debug_assert!(
                    false,
                    "stored subscription disappeared while routing released compaction replay id"
                );
                output.extend(self.execute_route_cleanup(subs, RouteCleanup::RouteOnly(id)));
            }
        }
        output
    }

    fn handle_relay_eose(&mut self, sid: &str) -> RecvResponse {
        tracing::debug!("Relay {} received EOSE for subscription: {sid}", self.url());
        let req_id = RelayReqId(sid.to_string());
        let compaction_transition = self.compaction_data.apply_eose(&req_id);
        let mut output = self.apply_compaction_transition(compaction_transition);
        let transparent_ids = self
            .transparent_data
            .ids_for_sid(&req_id)
            .unwrap_or_default();
        self.transparent_data
            .set_req_status(sid, RelayReqStatus::Eose);

        let mut facts = CoordinationFacts::new(
            RelayEoseDelta {
                sub_ids: transparent_ids.clone(),
                invalidated_sub_ids: HashSet::new(),
            },
            HashSet::new(),
        );
        facts
            .status_changed_sub_ids
            .extend(transparent_ids.iter().copied());
        output.extend(CoordinationOutput::from_facts(facts));

        RecvResponse {
            output,
            ..RecvResponse::received()
        }
    }

    fn handle_relay_closed_sid(&mut self, sid: &str) -> RecvResponse {
        tracing::trace!("Relay {} received CLOSED: {sid}", self.url());
        let req_id = RelayReqId(sid.to_string());
        let compaction_transition = self.compaction_data.apply_closed(&req_id);
        let mut output = self.apply_compaction_transition(compaction_transition);
        let transparent_ids = self
            .transparent_data
            .ids_for_sid(&req_id)
            .unwrap_or_default();
        self.transparent_data
            .set_req_status(sid, RelayReqStatus::Closed);

        let mut facts = CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new());
        facts.status_changed_sub_ids.extend(transparent_ids);
        output.extend(CoordinationOutput::from_facts(facts));

        RecvResponse {
            output,
            ..RecvResponse::received()
        }
    }

    /// Apply a relay-local negentropy timeout.
    pub(crate) fn apply_negentropy_timeout(&mut self, now: Instant) -> CoordinationOutput {
        if !self.supports_relay_subscription_ids() {
            return CoordinationOutput::empty();
        }

        CoordinationOutput::from_negentropy_effect(NegentropyRelayEffect::Timeout {
            generation: self.current_generation,
            now,
        })
    }

    /// Cancel relay-local negentropy work owned by one durable subscription.
    pub(crate) fn cancel_negentropy_owner(
        &mut self,
        owner_history_id: FullHistorySubId,
    ) -> CoordinationOutput {
        CoordinationOutput::from_negentropy_effect(NegentropyRelayEffect::CancelOwner {
            generation: self.current_generation,
            owner_history_id,
        })
    }

    /// Cancel relay-local negentropy work owned by one sub for the given filters.
    pub(crate) fn cancel_negentropy_owner_filters(
        &mut self,
        owner_history_id: FullHistorySubId,
        filters: &[Filter],
    ) -> CoordinationOutput {
        CoordinationOutput::from_negentropy_effect(NegentropyRelayEffect::CancelOwnerFilters {
            generation: self.current_generation,
            owner_history_id,
            filters: filters.to_vec(),
        })
    }
}

/// Replays relay-engine state after a websocket reconnect and returns the
/// coordinator-owned effects that still need routing decisions.
fn collect_relay_open_replay_effects(
    current_generation: Option<u64>,
    compaction_data: &mut CompactionData,
    transparent_data: &mut TransparentData,
    relay_limits: &mut RelayCoordinatorLimits,
    subs: &OutboxSubscriptions,
    replay_subscription_reqs: bool,
) -> (RelayOpenReplayEffects, Vec<QueuedRelayFrame>) {
    if !replay_subscription_reqs {
        return (RelayOpenReplayEffects::default(), Vec::new());
    }

    let mut effects = RelayOpenReplayEffects::default();
    let limits = ReqFilterLimits::from_relay_limits(relay_limits);
    let transparent_replay = transparent_data.handle_relay_open(current_generation, limits);
    for outcome in transparent_replay.outcomes {
        match outcome {
            TransparentReplayOutcome::Reissued(id) => {
                effects.invalidated_sub_ids.insert(id);
            }
            TransparentReplayOutcome::Blocked(id) => {
                effects.invalidated_sub_ids.insert(id);
                effects.blocked_transparent_ids.insert(id);
            }
        }
    }
    let mut frames = transparent_replay.frames;
    return_subpasses(
        &mut relay_limits.sub_guardian,
        transparent_replay.returned_passes,
    );

    let granted_passes = take_available_subpasses(&mut relay_limits.sub_guardian);
    let compaction_replay =
        compaction_data.handle_relay_open(current_generation, limits, granted_passes, subs);
    frames.extend(compaction_replay.frames);
    return_subpasses(
        &mut relay_limits.sub_guardian,
        compaction_replay.returned_passes,
    );
    effects
        .invalidated_sub_ids
        .extend(compaction_replay.invalidated_sub_ids);
    effects
        .released_compaction_ids
        .extend(compaction_replay.released_ids);
    (effects, frames)
}

/// One websocket-open state transition observed while polling a relay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebsocketOpenTransition {
    /// The relay websocket finished opening successfully.
    Opened,
    /// The relay websocket failed to open while still in the connecting state.
    Failed(ewebsock::Error),
}

#[derive(Default)]
/// Non-blocking receive outcome for one `CoordinationData::try_recv` poll.
pub struct RecvResponse {
    /// Coordinator effects produced by this receive transition.
    pub(crate) output: CoordinationOutput,
    /// A receive-time classification of websocket opening-state events for this
    /// relay. This is not the result of an explicit open call; it reflects
    /// what `try_recv` observed while the relay was connecting.
    pub websocket_open_transition: Option<WebsocketOpenTransition>,
    /// A websocket transport error was observed for this relay.
    pub websocket_transport_failure: bool,
    /// The websocket closed without a transport error frame.
    pub websocket_closed: bool,
}

impl RecvResponse {
    /// Returns the baseline outcome for a poll that consumed one websocket
    /// frame but has not yet classified any relay-side effects.
    pub fn received() -> Self {
        RecvResponse {
            output: CoordinationOutput::empty(),
            websocket_open_transition: None,
            websocket_transport_failure: false,
            websocket_closed: false,
        }
    }
}

fn transparent_retry_drain_result(made_progress: bool, queued_len: usize) -> TransparentRetryDrain {
    match (made_progress, queued_len) {
        (_, 0) => TransparentRetryDrain::Drained,
        (true, _) => TransparentRetryDrain::MadeProgress { still_queued: true },
        (false, _) => TransparentRetryDrain::Blocked {
            reason: TransparentRetryBlockedReason::NoPlacementProgress,
        },
    }
}

/// Durable relay-local facts produced by one coordinator transition.
pub struct CoordinationFacts {
    /// Relay-local EOSE facts for durable outbox tracking.
    pub eose_delta: RelayEoseDelta,
    pub invalidated_sub_ids: HashSet<OutboxSubId>,
    /// Outbox subscription IDs whose relay-local request status changed.
    pub status_changed_sub_ids: HashSet<OutboxSubId>,
}

impl CoordinationFacts {
    fn new(mut eose_delta: RelayEoseDelta, mut invalidated_sub_ids: HashSet<OutboxSubId>) -> Self {
        invalidated_sub_ids.extend(std::mem::take(&mut eose_delta.invalidated_sub_ids));
        eose_delta.invalidated_sub_ids.clear();
        eose_delta.normalize();
        for id in &invalidated_sub_ids {
            eose_delta.sub_ids.remove(id);
        }
        Self {
            eose_delta,
            invalidated_sub_ids,
            status_changed_sub_ids: HashSet::new(),
        }
    }

    fn invalidated_sub_id(id: OutboxSubId) -> Self {
        Self::new(RelayEoseDelta::default(), HashSet::from([id]))
    }

    fn extend(&mut self, next: CoordinationFacts) {
        self.eose_delta.extend(next.eose_delta);
        self.invalidated_sub_ids.extend(next.invalidated_sub_ids);
        self.status_changed_sub_ids
            .extend(next.status_changed_sub_ids);
        let status_changed_sub_ids = std::mem::take(&mut self.status_changed_sub_ids);
        *self = Self::new(
            std::mem::take(&mut self.eose_delta),
            std::mem::take(&mut self.invalidated_sub_ids),
        );
        self.status_changed_sub_ids = status_changed_sub_ids;
    }
}

/// Explicit output produced by one coordinator transition.
pub(crate) struct CoordinationOutput {
    pub(crate) facts: CoordinationFacts,
    pub(crate) frames: Vec<QueuedRelayFrame>,
    pub(crate) negentropy_effects: Vec<NegentropyRelayEffect>,
    pub(crate) full_history_capacity_grants: Vec<FullHistoryNegentropyCapacityGrant>,
    pub(crate) relay_demand: Option<Option<RelayTransportDemand>>,
}

impl CoordinationOutput {
    fn new(facts: CoordinationFacts, frames: Vec<QueuedRelayFrame>) -> Self {
        Self {
            facts,
            frames,
            negentropy_effects: Vec::new(),
            full_history_capacity_grants: Vec::new(),
            relay_demand: None,
        }
    }

    fn from_negentropy_revocations(
        generation: Option<u64>,
        revocations: Vec<SubPassRevocation>,
    ) -> Self {
        if revocations.is_empty() {
            return Self::empty();
        }

        Self {
            facts: CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new()),
            frames: Vec::new(),
            negentropy_effects: vec![NegentropyRelayEffect::RevocateSessions {
                generation,
                revocations,
            }],
            full_history_capacity_grants: Vec::new(),
            relay_demand: None,
        }
    }

    pub(crate) fn from_negentropy_effect(effect: NegentropyRelayEffect) -> Self {
        Self {
            facts: CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new()),
            frames: Vec::new(),
            negentropy_effects: vec![effect],
            full_history_capacity_grants: Vec::new(),
            relay_demand: None,
        }
    }

    fn from_full_history_capacity_grant(grant: FullHistoryNegentropyCapacityGrant) -> Self {
        Self {
            facts: CoordinationFacts::new(RelayEoseDelta::default(), HashSet::new()),
            frames: Vec::new(),
            negentropy_effects: Vec::new(),
            full_history_capacity_grants: vec![grant],
            relay_demand: None,
        }
    }

    fn from_facts(facts: CoordinationFacts) -> Self {
        Self {
            facts,
            frames: Vec::new(),
            negentropy_effects: Vec::new(),
            full_history_capacity_grants: Vec::new(),
            relay_demand: None,
        }
    }

    fn from_invalidated_sub_ids(
        invalidated_sub_ids: HashSet<OutboxSubId>,
        frames: Vec<QueuedRelayFrame>,
    ) -> Self {
        Self::new(
            CoordinationFacts::new(RelayEoseDelta::default(), invalidated_sub_ids),
            frames,
        )
    }

    fn empty() -> Self {
        Self::from_facts(CoordinationFacts::new(
            RelayEoseDelta::default(),
            HashSet::new(),
        ))
    }

    fn extend(&mut self, next: CoordinationOutput) {
        self.facts.extend(next.facts);
        self.frames.extend(next.frames);
        self.negentropy_effects.extend(next.negentropy_effects);
        self.full_history_capacity_grants
            .extend(next.full_history_capacity_grants);
        if next.relay_demand.is_some() {
            self.relay_demand = next.relay_demand;
        }
    }
}

impl Default for CoordinationOutput {
    fn default() -> Self {
        Self::empty()
    }
}

/// Result returned after applying coordinator-owned compaction capacity.
pub(crate) struct CompactionCapacityResult {
    /// Normal coordinator effects produced by one transition.
    pub(crate) output: CoordinationOutput,
    /// Explicit compaction backlog/progress state from capacity application.
    pub(crate) queue: CompactionCapacityProgress,
}

#[derive(Default)]
pub struct RelayEoseDelta {
    /// Subscriptions that reached EOSE for the current relay-query epoch.
    pub sub_ids: HashSet<OutboxSubId>,
    /// Subscriptions whose prior relay-query epoch was reset during this transition.
    ///
    /// Invalidation wins over any stale queued EOSE resolved earlier in the same
    /// transition, so this set must remain disjoint from `sub_ids`.
    pub invalidated_sub_ids: HashSet<OutboxSubId>,
}

impl RelayEoseDelta {
    fn extend(&mut self, next: RelayEoseDelta) {
        self.sub_ids.extend(next.sub_ids);
        self.invalidated_sub_ids.extend(next.invalidated_sub_ids);
        self.normalize();
    }

    /// Removes stale queued EOSE completions for subscriptions invalidated in
    /// the same coordinator transition.
    fn normalize(&mut self) {
        self.sub_ids
            .retain(|id| !self.invalidated_sub_ids.contains(id));
        debug_assert!(
            self.sub_ids.is_disjoint(&self.invalidated_sub_ids),
            "RelayEoseDelta must not contain overlapping EOSE and invalidation IDs"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::negentropy::{
        ActiveSessionRelayDemand, NegentropyData, NegentropyRelay, NegentropyStartResult,
    };
    use crate::relay::{
        frame::{QueuedRelayFrame, RelayFrameSink},
        FullRelayPkgsModificationTask, ModifyTask, RelayDemandPriority, RelayUrlPkgs,
        SubscribeTask,
    };
    use crate::NormRelayUrl;
    use negentropy::NegentropyStorageVector;
    use nostrdb::Filter;
    use std::time::Duration;

    enum NegentropyStartOutcome {
        Started,
        Retry,
        Drop,
    }

    enum NegentropyStartReadiness {
        Ready,
        Retry,
        Drop,
    }

    fn req_filter_count(frame: &str) -> usize {
        let value: serde_json::Value = serde_json::from_str(frame).expect("parse captured REQ");
        value.as_array().expect("REQ frame array").len() - 2
    }

    fn captured_frame_kind_and_sid(frame: &str) -> (String, String) {
        let value: serde_json::Value =
            serde_json::from_str(frame).expect("parse captured relay frame");
        let array = value.as_array().expect("relay frame array");
        let kind = array
            .first()
            .and_then(serde_json::Value::as_str)
            .expect("relay frame kind")
            .to_owned();
        let sid = array
            .get(1)
            .and_then(serde_json::Value::as_str)
            .expect("relay frame sid")
            .to_owned();
        (kind, sid)
    }

    fn send_frame_jsons(output: CoordinationOutput) -> Vec<String> {
        output
            .frames
            .into_iter()
            .map(|(_, message)| message.to_json().expect("send frame should serialize"))
            .collect()
    }

    fn run_negentropy_relay_for_test<T>(
        generation: Option<u64>,
        data: &mut NegentropyData,
        f: impl FnOnce(&mut NegentropyRelay<'_>) -> T,
    ) -> (T, Vec<QueuedRelayFrame>) {
        let mut relay = NegentropyRelay::new(RelayFrameSink::transport(generation), data);
        let result = f(&mut relay);
        let frames = relay.take_frames();
        (result, frames)
    }

    fn apply_negentropy_effects_for_test(
        coordinator: &mut CoordinationData,
        subs: &OutboxSubscriptions,
        mut output: CoordinationOutput,
        negentropy_data: &mut NegentropyData,
    ) -> CoordinationOutput {
        for effect in std::mem::take(&mut output.negentropy_effects) {
            match effect {
                NegentropyRelayEffect::RevocateSessions {
                    generation,
                    revocations,
                } => {
                    let (revocation_effects, _frames) =
                        run_negentropy_relay_for_test(generation, negentropy_data, |relay| {
                            relay.revocate_sessions(revocations.len())
                        });
                    assert_eq!(revocation_effects.revoked_passes.len(), revocations.len());
                    for (mut revocation, pass) in revocations
                        .into_iter()
                        .zip(revocation_effects.revoked_passes)
                    {
                        revocation.revocate(pass);
                    }
                }
                NegentropyRelayEffect::RelayDisconnect => {
                    let effects =
                        NegentropyRelay::new(RelayFrameSink::disconnected(), negentropy_data)
                            .handle_relay_disconnect();
                    output
                        .extend(coordinator.apply_negentropy_effects_after_release(subs, effects));
                }
                NegentropyRelayEffect::Timeout { generation, now } => {
                    let (effects, frames) =
                        run_negentropy_relay_for_test(generation, negentropy_data, |relay| {
                            relay.handle_timeout(now)
                        });
                    output.frames.extend(frames);
                    output
                        .extend(coordinator.apply_negentropy_effects_after_release(subs, effects));
                }
                NegentropyRelayEffect::CancelOwner {
                    generation,
                    owner_history_id,
                } => {
                    let (effects, frames) =
                        run_negentropy_relay_for_test(generation, negentropy_data, |relay| {
                            relay.cancel_owner(owner_history_id)
                        });
                    output.frames.extend(frames);
                    output
                        .extend(coordinator.apply_negentropy_effects_after_release(subs, effects));
                }
                NegentropyRelayEffect::CancelOwnerFilters {
                    generation,
                    owner_history_id,
                    filters,
                } => {
                    let (effects, frames) =
                        run_negentropy_relay_for_test(generation, negentropy_data, |relay| {
                            relay.cancel_owner_filters(owner_history_id, &filters)
                        });
                    output.frames.extend(frames);
                    output
                        .extend(coordinator.apply_negentropy_effects_after_release(subs, effects));
                }
                NegentropyRelayEffect::DropSessionsWithoutNegClose => {
                    let effects =
                        NegentropyRelay::new(RelayFrameSink::disconnected(), negentropy_data)
                            .drop_sessions_without_neg_close();
                    output
                        .extend(coordinator.apply_negentropy_effects_after_release(subs, effects));
                }
            }
        }
        output
    }

    fn apply_compaction_plan_for_test(
        coordinator: &mut CoordinationData,
        subs: &OutboxSubscriptions,
        plan: CompactionOperationPlan,
    ) -> CompactionTransition {
        let limits = ReqFilterLimits::from_relay_limits(&coordinator.limits);
        let mut transition = coordinator
            .compaction_data
            .apply_operation_plan_without_capacity_application(
                coordinator.current_generation,
                limits,
                take_available_subpasses(&mut coordinator.limits.sub_guardian),
                subs,
                plan,
            );
        return_subpasses(
            &mut coordinator.limits.sub_guardian,
            std::mem::take(&mut transition.returned_passes),
        );
        transition
    }

    fn open_coordinator(coordinator: &mut CoordinationData) {
        let subs = OutboxSubscriptions::default();
        let _ = coordinator.apply_websocket_opened(&subs, Duration::ZERO, 0);
    }

    fn set_max_size(
        coordinator: &mut CoordinationData,
        subs: &OutboxSubscriptions,
        negentropy_data: &mut NegentropyData,
        max_size: usize,
    ) -> CoordinationOutput {
        coordinator.set_limits(
            subs,
            negentropy_data.active_session_count(),
            RelayLimitations {
                maximum_subs: max_size,
                max_json_bytes: coordinator.current_limits().max_json_bytes,
            },
        )
    }

    fn try_initiate_negentropy(
        coordinator: &mut CoordinationData,
        negentropy_data: &mut NegentropyData,
        storage: impl FnOnce() -> NegentropyStorageVector,
        filter: Filter,
        owner_history_id: FullHistorySubId,
        relay_demand: ActiveSessionRelayDemand,
    ) -> NegentropyStartOutcome {
        match negentropy_start_readiness(coordinator, negentropy_data) {
            NegentropyStartReadiness::Ready => {}
            NegentropyStartReadiness::Retry => return NegentropyStartOutcome::Retry,
            NegentropyStartReadiness::Drop => return NegentropyStartOutcome::Drop,
        }

        let grant = match coordinator.reserve_full_history_negentropy_capacity() {
            Ok(grant) => grant,
            Err(NegentropyCapacityError::Retry) => return NegentropyStartOutcome::Retry,
            Err(NegentropyCapacityError::Drop) => return NegentropyStartOutcome::Drop,
        };

        let started = negentropy_data.try_start_full_history(
            grant.pass,
            storage,
            filter,
            owner_history_id,
            relay_demand,
        );

        match started {
            NegentropyStartResult::Started(msg) => {
                let mut frame_sink = RelayFrameSink::transport(Some(grant.generation));
                frame_sink.send(msg);
                if !frame_sink.into_frames().is_empty() {
                    return NegentropyStartOutcome::Started;
                }
            }
            NegentropyStartResult::Rejected(pass) => {
                coordinator.return_full_history_subpass(pass);
                return NegentropyStartOutcome::Drop;
            }
        }

        NegentropyStartOutcome::Drop
    }

    fn negentropy_start_readiness(
        coordinator: &CoordinationData,
        negentropy_data: &NegentropyData,
    ) -> NegentropyStartReadiness {
        if !coordinator.supports_relay_subscription_ids() {
            return NegentropyStartReadiness::Drop;
        }

        if negentropy_data.is_unsupported() {
            return NegentropyStartReadiness::Drop;
        }

        if coordinator.current_generation.is_none() {
            return NegentropyStartReadiness::Retry;
        }
        if coordinator.limits.sub_guardian.available_passes() == 0 {
            return NegentropyStartReadiness::Retry;
        }

        NegentropyStartReadiness::Ready
    }

    fn one_filter_req_json_limit(filters: &[Filter]) -> usize {
        let largest_filter_json = filters
            .iter()
            .map(|filter| ReqFilterLimits::filter_json_size(filter).expect("filter json"))
            .max()
            .expect("at least one filter");
        ReqFilterLimits::req_json_size(1, largest_filter_json)
    }

    /// Inserts a subscription with a distinct single filter for coordinator tests.
    fn insert_sub_with_policy(
        subs: &mut OutboxSubscriptions,
        id: OutboxSubId,
        policy: RelayRoutingPreference,
    ) {
        insert_sub_with_filters_and_policy(
            subs,
            id,
            policy,
            vec![Filter::new().kinds([(id.0 % 7) + 3]).limit(1).build()],
        );
    }

    /// Inserts a subscription with caller-provided filters for coordinator tests.
    fn insert_sub_with_filters_and_policy(
        subs: &mut OutboxSubscriptions,
        id: OutboxSubId,
        policy: RelayRoutingPreference,
        filters: Vec<Filter>,
    ) {
        subs.new_subscription(
            id,
            SubscribeTask {
                filters,
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        policy,
                    ),
                ),
            },
            false,
        );
    }

    fn update_sub_policy(
        subs: &mut OutboxSubscriptions,
        id: OutboxSubId,
        policy: RelayRoutingPreference,
    ) {
        let filters = subs
            .get(&id)
            .expect("subscription")
            .filters
            .get_filters()
            .clone();
        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters,
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        policy,
                    ),
                ),
            }),
        ));
    }

    /// Builds a filter large enough to exercise compaction JSON-limit behavior.
    fn bulky_filter(seed: u8) -> Filter {
        let authors = (0..10)
            .map(|offset| [seed.wrapping_add(offset); 32])
            .collect::<Vec<_>>();
        Filter::new()
            .authors(authors.iter())
            .kinds([1])
            .limit(1)
            .build()
    }

    /// Builds a filter too large for the protocol JSON constructor.
    fn oversized_negentropy_filter() -> Filter {
        let mut ids = Vec::new();
        for index in 0..18_000u64 {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&index.to_be_bytes());
            ids.push(id);
        }
        let filter = Filter::new_with_capacity(512).ids(ids.iter()).build();
        assert!(filter.json().is_err());
        filter
    }

    /// Negentropy start should leave storage unbuilt when the relay is not
    /// connected yet so the attempt can be retried later.
    #[test]
    fn try_initiate_negentropy_retries_when_websocket_is_not_connected() {
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 4,
            max_json_bytes: 256_000,
        });
        let mut negentropy_data = NegentropyData::default();
        let mut built_storage = false;

        let outcome = try_initiate_negentropy(
            &mut coordinator,
            &mut negentropy_data,
            || {
                built_storage = true;
                NegentropyStorageVector::new()
            },
            Filter::new().build(),
            FullHistorySubId(0),
            ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
        );

        assert!(matches!(outcome, NegentropyStartOutcome::Retry));
        assert!(!built_storage);
    }

    #[tokio::test]
    async fn try_initiate_negentropy_drops_unserializable_filter_without_consuming_pass() {
        let mut coordinator = coordinator_with_limit(1);
        let mut negentropy_data = NegentropyData::default();
        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");

        let outcome = try_initiate_negentropy(
            &mut coordinator,
            &mut negentropy_data,
            || storage,
            oversized_negentropy_filter(),
            FullHistorySubId(1),
            ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
        );

        assert!(matches!(outcome, NegentropyStartOutcome::Drop));
        assert_eq!(negentropy_data.active_session_count(), 0);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);
    }

    #[tokio::test]
    async fn limit_downgrade_revokes_active_negentropy_sessions() {
        let subs = OutboxSubscriptions::default();
        let mut coordinator = coordinator_with_limit(2);
        let mut negentropy_data = NegentropyData::default();

        for id in [FullHistorySubId(0), FullHistorySubId(1)] {
            let mut storage = NegentropyStorageVector::new();
            storage.seal().expect("seal empty negentropy storage");
            let filter = Filter::new().kinds([1]).build();

            let outcome = try_initiate_negentropy(
                &mut coordinator,
                &mut negentropy_data,
                || storage,
                filter,
                id,
                ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
            );

            assert!(matches!(outcome, NegentropyStartOutcome::Started));
        }
        assert_eq!(negentropy_data.active_session_count(), 2);

        let output = set_max_size(&mut coordinator, &subs, &mut negentropy_data, 0);
        let _output = apply_negentropy_effects_for_test(
            &mut coordinator,
            &subs,
            output,
            &mut negentropy_data,
        );

        assert_eq!(coordinator.current_limits().maximum_subs, 0);
        assert_eq!(negentropy_data.active_session_count(), 0);
    }

    #[tokio::test]
    async fn negentropy_capacity_release_places_required_transparent_queue_when_capacity_runs() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(25_001);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);
        let mut negentropy_data = NegentropyData::default();

        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");
        let outcome = try_initiate_negentropy(
            &mut coordinator,
            &mut negentropy_data,
            || storage,
            Filter::new().kinds([1]).build(),
            FullHistorySubId(25),
            ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
        );
        assert!(matches!(outcome, NegentropyStartOutcome::Started));
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        let _ = coordinator.queue_dedicated_retry(&subs, id_required);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            0
        );

        let session_id = negentropy_data
            .has_pending_work()
            .then(|| {
                negentropy_data
                    .first_active_session_id_for_test()
                    .expect("active negentropy session")
            })
            .expect("active negentropy work");
        let (effects, _frames) = run_negentropy_relay_for_test(
            coordinator.current_generation(),
            &mut negentropy_data,
            |relay| relay.handle_neg_err(&session_id, "closed: done"),
        );
        let _output = coordinator.apply_negentropy_effects_after_release(&subs, effects);

        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn negentropy_owner_cancel_places_required_transparent_queue_when_capacity_runs() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(25_002);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);
        let mut negentropy_data = NegentropyData::default();

        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");
        let outcome = try_initiate_negentropy(
            &mut coordinator,
            &mut negentropy_data,
            || storage,
            Filter::new().kinds([1]).build(),
            FullHistorySubId(26),
            ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
        );
        assert!(matches!(outcome, NegentropyStartOutcome::Started));

        let _ = coordinator.queue_dedicated_retry(&subs, id_required);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            0
        );

        let ingest = coordinator.cancel_negentropy_owner(FullHistorySubId(26));
        let mut ingest = apply_negentropy_effects_for_test(
            &mut coordinator,
            &subs,
            ingest,
            &mut negentropy_data,
        );
        ingest.extend(coordinator.apply_capacity_available(&subs));

        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
        assert!(ingest.facts.invalidated_sub_ids.contains(&id_required));
    }

    #[tokio::test]
    async fn negentropy_filter_cancel_places_required_transparent_queue_when_capacity_runs() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(25_003);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);
        let mut negentropy_data = NegentropyData::default();

        let filter = Filter::new().kinds([1]).build();
        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");
        let outcome = try_initiate_negentropy(
            &mut coordinator,
            &mut negentropy_data,
            || storage,
            filter.clone(),
            FullHistorySubId(27),
            ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
        );
        assert!(matches!(outcome, NegentropyStartOutcome::Started));

        let _ = coordinator.queue_dedicated_retry(&subs, id_required);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            0
        );

        let ingest = coordinator.cancel_negentropy_owner_filters(FullHistorySubId(27), &[filter]);
        let mut ingest = apply_negentropy_effects_for_test(
            &mut coordinator,
            &subs,
            ingest,
            &mut negentropy_data,
        );
        ingest.extend(coordinator.apply_capacity_available(&subs));

        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
        assert!(ingest.facts.invalidated_sub_ids.contains(&id_required));
    }

    #[tokio::test]
    async fn negentropy_disconnect_places_required_transparent_queue_when_capacity_runs() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(25_004);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);
        let mut negentropy_data = NegentropyData::default();

        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");
        let outcome = try_initiate_negentropy(
            &mut coordinator,
            &mut negentropy_data,
            || storage,
            Filter::new().kinds([1]).build(),
            FullHistorySubId(28),
            ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
        );
        assert!(matches!(outcome, NegentropyStartOutcome::Started));

        let _ = coordinator.queue_dedicated_retry(&subs, id_required);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            0
        );

        let ingest = coordinator.disconnect_websocket_leg_at();
        let mut ingest = apply_negentropy_effects_for_test(
            &mut coordinator,
            &subs,
            ingest,
            &mut negentropy_data,
        );
        ingest.extend(coordinator.apply_capacity_available(&subs));

        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
        assert!(ingest.facts.invalidated_sub_ids.contains(&id_required));
    }

    #[tokio::test]
    async fn notice_updates_active_negentropy_support_state_by_message() {
        for (notice, unsupported, active_sessions, available_passes) in [
            ("ERROR: bad msg: negentropy disabled", true, 0, 1),
            ("unknown message type: REQ", false, 1, 0),
        ] {
            let mut coordinator = coordinator_with_limit(1);
            let mut negentropy_data = NegentropyData::default();

            let mut storage = NegentropyStorageVector::new();
            storage.seal().expect("seal empty negentropy storage");
            let filter = Filter::new().kinds([1]).build();

            let outcome = try_initiate_negentropy(
                &mut coordinator,
                &mut negentropy_data,
                || storage,
                filter,
                FullHistorySubId(1),
                ActiveSessionRelayDemand::single(RelayDemandPriority::Important, 0),
            );

            assert!(matches!(outcome, NegentropyStartOutcome::Started));
            assert_eq!(negentropy_data.active_session_count(), 1);
            assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

            let subs = OutboxSubscriptions::default();
            let effects = NegentropyRelay::new(
                RelayFrameSink::transport(coordinator.current_generation()),
                &mut negentropy_data,
            )
            .handle_notice(notice);
            let _output = coordinator.apply_negentropy_effects_after_release(&subs, effects);

            assert_eq!(negentropy_data.is_unsupported(), unsupported);
            assert_eq!(negentropy_data.active_session_count(), active_sessions);
            assert_eq!(
                coordinator.limits.sub_guardian.available_passes(),
                available_passes
            );
        }
    }

    // ==================== RelayEoseDelta tests ====================

    #[tokio::test]
    async fn relay_eose_delta_default_empty() {
        let delta = RelayEoseDelta::default();
        assert!(delta.sub_ids.is_empty());
        assert!(delta.invalidated_sub_ids.is_empty());
    }

    #[tokio::test]
    async fn relay_eose_delta_normalize_drops_invalidated_stale_eose() {
        let keep = OutboxSubId(1);
        let overlap = OutboxSubId(2);
        let mut delta = RelayEoseDelta {
            sub_ids: HashSet::from([keep, overlap]),
            invalidated_sub_ids: HashSet::from([overlap]),
        };

        delta.normalize();

        assert_eq!(delta.sub_ids, HashSet::from([keep]));
        assert_eq!(delta.invalidated_sub_ids, HashSet::from([overlap]));
        assert!(delta.sub_ids.is_disjoint(&delta.invalidated_sub_ids));
    }

    #[tokio::test]
    async fn relay_eose_response_carries_completed_ids_directly() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(9);
        insert_sub_with_policy(&mut subs, id, RelayRoutingPreference::PreferDedicated);

        let mut coordinator = coordinator_with_limit(1);
        let mut invalidated_sub_ids = HashSet::new();
        let placed = coordinator.try_place_dedicated_route(&subs, id);
        invalidated_sub_ids.extend(placed.output.facts.invalidated_sub_ids);
        assert!(matches!(
            placed.result,
            Some(TransparentPlaceResult::Placed)
        ));

        let sid = coordinator
            .transparent_data
            .active_sid(&id)
            .expect("transparent route should have a live sid");

        let response = coordinator.apply_relay_eose(0, &sid.0);
        assert_eq!(
            response.output.facts.eose_delta.sub_ids,
            HashSet::from([id])
        );
        assert!(response
            .output
            .facts
            .eose_delta
            .invalidated_sub_ids
            .is_empty());
        assert_eq!(
            response.output.facts.status_changed_sub_ids,
            HashSet::from([id])
        );
    }

    #[tokio::test]
    async fn available_capacity_places_queued_transparent_retry() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(10_001);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        let mut coordinator = coordinator_with_limit(1);

        let _ = coordinator.queue_dedicated_retry(&subs, id_required);
        let output = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert!(output.facts.invalidated_sub_ids.contains(&id_required));
    }

    #[tokio::test]
    async fn unsupported_subscription_ids_return_invalidations_directly() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(10);
        insert_sub_with_policy(&mut subs, id, RelayRoutingPreference::RequireDedicated);

        let mut coordinator = coordinator_with_limit(1);
        let mut invalidated_sub_ids = HashSet::new();
        let placed = coordinator.try_place_dedicated_route(&subs, id);
        invalidated_sub_ids.extend(placed.output.facts.invalidated_sub_ids);
        assert!(matches!(
            placed.result,
            Some(TransparentPlaceResult::Placed)
        ));

        let sid = coordinator
            .transparent_data
            .active_sid(&id)
            .expect("transparent route should have a live sid");

        let unsupported_flush = coordinator.mark_subscription_id_length_unsupported(8);
        assert_eq!(
            unsupported_flush.facts.invalidated_sub_ids,
            HashSet::from([id])
        );

        let response = coordinator.apply_relay_eose(0, &sid.0);
        assert!(response.output.facts.eose_delta.sub_ids.is_empty());
        assert!(response
            .output
            .facts
            .eose_delta
            .invalidated_sub_ids
            .is_empty());
    }

    fn coordinator_with_limit(maximum_subs: usize) -> CoordinationData {
        let _relay = NormRelayUrl::new("wss://relay-coordinator-test.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);
        coordinator
    }

    #[tokio::test]
    async fn closed_compaction_cleanup_preserves_remaining_owner_closed_status() {
        let mut subs = OutboxSubscriptions::default();
        let live_id = OutboxSubId(31_200);
        let fetch_id = OutboxSubId(31_201);
        insert_sub_with_policy(&mut subs, live_id, RelayRoutingPreference::NoPreference);
        insert_sub_with_policy(&mut subs, fetch_id, RelayRoutingPreference::NoPreference);

        let mut coordinator = coordinator_with_limit(1);
        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(live_id);
        compaction_session.sub(fetch_id);
        let _transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let _ = coordinator.set_compaction_route(&subs, live_id);
        let _ = coordinator.set_compaction_route(&subs, fetch_id);

        let live_sid = coordinator
            .active_compaction_sid_for_test(&live_id)
            .expect("live id placed in compaction");
        let fetch_sid = coordinator
            .active_compaction_sid_for_test(&fetch_id)
            .expect("fetch id placed in compaction");
        assert_eq!(live_sid, fetch_sid);

        let closed = coordinator.apply_relay_closed(0, &fetch_sid.to_string());
        assert!(closed.output.frames.is_empty());
        assert_eq!(
            coordinator.req_status(&live_id),
            Some(RelayReqStatus::Closed)
        );
        assert_eq!(
            coordinator.req_status(&fetch_id),
            Some(RelayReqStatus::Closed)
        );

        subs.remove(&fetch_id);
        let cleanup = coordinator.remove_subscription_after_relay_closed(&subs, fetch_id);

        assert!(cleanup.frames.is_empty());
        assert_eq!(coordinator.route_type(&fetch_id), None);
        assert_eq!(coordinator.req_status(&fetch_id), None);
        assert_eq!(
            coordinator.route_type(&live_id),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.req_status(&live_id),
            Some(RelayReqStatus::Closed)
        );
        assert!(cleanup.facts.invalidated_sub_ids.contains(&fetch_id));
        assert!(!cleanup.facts.status_changed_sub_ids.contains(&live_id));
    }

    #[tokio::test]
    async fn apply_available_compaction_capacity_places_into_existing_req_without_free_pass() {
        let mut subs = OutboxSubscriptions::default();
        let id_seed = OutboxSubId(31_130);
        let id_queued = OutboxSubId(31_131);
        let seed_filter = Filter::new().kinds([1]).limit(1).build();
        let queued_initial_filter = bulky_filter(2);
        let queued_final_filter = Filter::new().kinds([2]).limit(1).build();
        let seed_filter_size = ReqFilterLimits::filter_json_size(&seed_filter).unwrap();
        let queued_initial_filter_size =
            ReqFilterLimits::filter_json_size(&queued_initial_filter).unwrap();
        let queued_final_filter_size =
            ReqFilterLimits::filter_json_size(&queued_final_filter).unwrap();
        let max_json_bytes =
            ReqFilterLimits::req_json_size(2, seed_filter_size + queued_final_filter_size);
        assert!(
            ReqFilterLimits::req_json_size(2, seed_filter_size + queued_initial_filter_size)
                > max_json_bytes,
            "initial queued filter must not fit beside seed filter"
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_seed,
            RelayRoutingPreference::NoPreference,
            vec![seed_filter],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued,
            RelayRoutingPreference::NoPreference,
            vec![queued_initial_filter],
        );

        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes,
        });
        let mut session = CompactionOperationPlan::default();
        session.sub(id_seed);
        let transition = apply_compaction_plan_for_test(&mut coordinator, &subs, session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_seed)
                .facts
                .invalidated_sub_ids,
        );

        let mut session = CompactionOperationPlan::default();
        session.sub(id_queued);
        let transition = apply_compaction_plan_for_test(&mut coordinator, &subs, session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_queued)
                .facts
                .invalidated_sub_ids,
        );

        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
        assert!(subs.ingest_task(
            &id_queued,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: vec![queued_final_filter],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        RelayRoutingPreference::NoPreference,
                    ),
                ),
            }),
        ));

        coordinator.apply_capacity_available(&subs);

        assert_eq!(
            coordinator.compaction_data.req_status(&id_queued),
            Some(RelayReqStatus::InitialQuery)
        );
        assert_eq!(coordinator.compaction_data.num_subs(), 1);
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 0);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn unrepresentable_transparent_request_does_not_reserve_compaction_pass() {
        let mut subs = OutboxSubscriptions::default();
        let id_compaction = OutboxSubId(31_132);
        let id_required = OutboxSubId(31_133);
        let required_filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&required_filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
            vec![Filter::new().kinds([3]).limit(1).build()],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            required_filters,
        );

        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: single_filter_json_limit,
        });
        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_compaction);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_compaction)
                .facts
                .invalidated_sub_ids,
        );

        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.compaction_data.req_status(&id_compaction),
            Some(RelayReqStatus::InitialQuery)
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            0
        );
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn queued_retry_refreshes_preserved_transparent_route_policy() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(31_134);
        let id_changed = OutboxSubId(31_135);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_changed,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(2);
        coordinator.subscribe(&subs, id_required);
        coordinator.subscribe(&subs, id_changed);
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_changed),
            1
        );

        update_sub_policy(&mut subs, id_changed, RelayRoutingPreference::NoPreference);
        let _ = coordinator.queue_dedicated_retry(&subs, id_changed);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 1);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1,
            "required route should keep the active transparent leg"
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_changed),
            0,
            "updated no-preference route should be selected before required routes"
        );
        assert_eq!(
            coordinator.route_type(&id_changed),
            Some(RelayType::Compaction)
        );
    }

    #[test]
    fn dedicated_req_queues_when_json_limit_exceeds_single_req() {
        let _relay = NormRelayUrl::new("wss://relay-coordinator-over-limit.example.com").unwrap();
        let id = OutboxSubId(100);
        let mut subs = OutboxSubscriptions::default();
        let filters = vec![
            Filter::new().kinds([1]).build(),
            Filter::new().kinds([2]).build(),
        ];
        let max_json_bytes = one_filter_req_json_limit(&filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id,
            RelayRoutingPreference::RequireDedicated,
            filters,
        );
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes,
        });
        open_coordinator(&mut coordinator);
        coordinator.apply_websocket_opened(&subs, Duration::from_secs(5), 0);

        let output = coordinator.subscribe(&subs, id);

        let frames = send_frame_jsons(output);
        assert!(
            frames.is_empty(),
            "queued over-limit route sent REQs: {frames:?}"
        );
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 0);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
    }

    #[test]
    fn limit_shrink_preserves_active_transparent_route_and_queues_retry() {
        let _relay = NormRelayUrl::new("wss://relay-coordinator-limit-shrink.example.com").unwrap();
        let id = OutboxSubId(101);
        let mut subs = OutboxSubscriptions::default();
        let filters = vec![
            Filter::new().kinds([1]).build(),
            Filter::new().kinds([2]).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id,
            RelayRoutingPreference::RequireDedicated,
            filters,
        );
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);
        coordinator.apply_websocket_opened(&subs, Duration::from_secs(5), 0);

        let output = coordinator.subscribe(&subs, id);

        let initial_frames = send_frame_jsons(output);
        assert_eq!(initial_frames.len(), 1);
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 1);

        let output = coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: single_filter_json_limit,
            },
        );

        let frames = send_frame_jsons(output);
        assert_eq!(req_filter_count(&initial_frames[0]), 2);
        assert_eq!(
            frames.len(),
            0,
            "limit shrink must not reissue over-limit REQs: {frames:?}"
        );
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 1);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
        assert_eq!(
            coordinator.transparent_data.req_status(&id),
            Some(RelayReqStatus::InitialQuery)
        );
    }

    #[tokio::test]
    async fn transparent_limit_growth_flushes_previously_over_limit_queue() {
        let id_over_limit = OutboxSubId(102);
        let id_queued = OutboxSubId(103);
        let mut subs = OutboxSubscriptions::default();
        let over_limit_filters = vec![
            Filter::new().kinds([1]).build(),
            Filter::new().kinds([2]).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&over_limit_filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_over_limit,
            RelayRoutingPreference::RequireDedicated,
            over_limit_filters,
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([3]).build()],
        );

        let _relay = NormRelayUrl::new("wss://relay-coordinator-transparent-growth.example.com")
            .expect("valid relay url");
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: single_filter_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_over_limit);
        assert_eq!(
            coordinator.route_type(&id_over_limit),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_over_limit),
            0
        );
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 2);

        coordinator.subscribe(&subs, id_queued);
        assert_eq!(
            coordinator.route_type(&id_queued),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_queued));
        assert_eq!(coordinator.transparent_data.active_leg_count(&id_queued), 1);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: 400_000,
            },
        );
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_over_limit),
            1,
            "relaxed filter limit should place the previously over-limit route"
        );
        assert_eq!(
            coordinator.route_type(&id_over_limit),
            Some(RelayType::Transparent)
        );
        assert!(
            coordinator.transparent_data.contains(&id_queued),
            "freed pass capacity should flush queued required transparent work"
        );
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    /// Finds IDs whose raw transparent storage order exposes the old
    /// active-repack priority bug.
    fn active_repack_no_preference_first_setup() -> (
        OutboxSubscriptions,
        CoordinationData,
        OutboxSubId,
        OutboxSubId,
        usize,
    ) {
        for offset in 0..512 {
            let id_no_preference = OutboxSubId(110 + offset);
            let id_required = OutboxSubId(10_110 + offset);
            let mut subs = OutboxSubscriptions::default();
            let no_preference_filters = vec![
                Filter::new().kinds([1]).build(),
                Filter::new().kinds([2]).build(),
            ];
            let required_filters = vec![
                Filter::new().kinds([3]).build(),
                Filter::new().kinds([4]).build(),
            ];
            let single_filter_json_limit = one_filter_req_json_limit(&required_filters);
            insert_sub_with_filters_and_policy(
                &mut subs,
                id_no_preference,
                RelayRoutingPreference::NoPreference,
                no_preference_filters,
            );
            insert_sub_with_filters_and_policy(
                &mut subs,
                id_required,
                RelayRoutingPreference::RequireDedicated,
                required_filters,
            );

            let mut coordinator = coordinator_with_limit(2);
            coordinator.subscribe(&subs, id_no_preference);
            coordinator.subscribe(&subs, id_required);

            if coordinator.transparent_data.request_ids().first() == Some(&id_no_preference) {
                return (
                    subs,
                    coordinator,
                    id_no_preference,
                    id_required,
                    single_filter_json_limit,
                );
            }
        }

        panic!("could not construct adversarial transparent repack order");
    }

    #[tokio::test]
    async fn transparent_repack_prioritizes_required_and_preserves_unplaced_no_preference() {
        let (subs, mut coordinator, id_no_preference, id_required, single_filter_json_limit) =
            active_repack_no_preference_first_setup();
        assert_eq!(
            coordinator.transparent_data.request_ids().first(),
            Some(&id_no_preference),
            "test setup must expose no-preference first raw transparent order"
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_no_preference),
            1
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 3,
                max_json_bytes: single_filter_json_limit,
            },
        );

        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1,
            "required active route must keep its existing transparent leg"
        );
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
        assert!(
            coordinator.transparent_data.contains(&id_no_preference),
            "no-preference active route must stay live when compaction cannot replace it"
        );
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_no_preference),
            1
        );
    }

    #[test]
    fn coordination_data_new_does_not_open_websocket() {
        let coordinator = CoordinationData::new(RelayLimitations::default());

        assert!(coordinator.current_generation.is_none());
    }

    #[tokio::test]
    async fn preferred_transparent_demotes_non_preferred_and_takes_freed_slot() {
        let mut subs = OutboxSubscriptions::default();
        let id_default = OutboxSubId(1);
        let id_preferred = OutboxSubId(2);
        let id_incoming = OutboxSubId(3);
        insert_sub_with_policy(&mut subs, id_default, RelayRoutingPreference::NoPreference);
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_incoming,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut coordinator = coordinator_with_limit(2);

        coordinator.subscribe(&subs, id_default);
        coordinator.subscribe(&subs, id_preferred);

        assert_eq!(
            coordinator.route_type(&id_default),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent)
        );

        coordinator.subscribe(&subs, id_incoming);

        assert_eq!(
            coordinator.route_type(&id_default),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_incoming),
            Some(RelayType::Transparent)
        );
        assert!(!coordinator.transparent_data.contains(&id_default));
        assert!(coordinator.transparent_data.contains(&id_preferred));
        assert!(coordinator.transparent_data.contains(&id_incoming));
        assert!(coordinator
            .compaction_data
            .req_status(&id_incoming)
            .is_none());
    }

    #[tokio::test]
    async fn preferred_transparent_does_not_demote_existing_preferred() {
        let mut subs = OutboxSubscriptions::default();
        let id_a = OutboxSubId(10);
        let id_b = OutboxSubId(11);
        insert_sub_with_policy(&mut subs, id_a, RelayRoutingPreference::PreferDedicated);
        insert_sub_with_policy(&mut subs, id_b, RelayRoutingPreference::PreferDedicated);

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_a);

        coordinator.subscribe(&subs, id_b);

        assert_eq!(coordinator.route_type(&id_a), Some(RelayType::Transparent));
        assert_eq!(coordinator.route_type(&id_b), Some(RelayType::Compaction));
        assert!(coordinator.transparent_data.contains(&id_a));
        assert!(!coordinator.transparent_data.contains(&id_b));
        assert!(coordinator.compaction_data.req_status(&id_a).is_none());
        assert!(!coordinator.transparent_data.contains(&id_b));
    }

    #[tokio::test]
    async fn older_preferred_compaction_route_keeps_priority_when_dedicated_slot_opens() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(12);
        let id_existing_preferred = OutboxSubId(13);
        let id_incoming_preferred = OutboxSubId(14);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_existing_preferred,
            RelayRoutingPreference::PreferDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_incoming_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_existing_preferred);

        coordinator.subscribe(&subs, id_incoming_preferred);

        coordinator.unsubscribe(&subs, id_required);
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.route_type(&id_required), None);
        assert_eq!(
            coordinator.route_type(&id_existing_preferred),
            Some(RelayType::Transparent),
            "the older preferred request should reclaim the freed slot before a newer preferred request"
        );
        assert_eq!(
            coordinator.route_type(&id_incoming_preferred),
            Some(RelayType::Compaction),
            "the newer preferred request should yield if an older preferred request was displaced from compaction"
        );
    }

    #[tokio::test]
    async fn preferred_compaction_route_beats_no_preference_when_dedicated_slot_opens() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(15);
        let id_no_preference = OutboxSubId(16);
        let id_preferred = OutboxSubId(17);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_no_preference);

        coordinator.subscribe(&subs, id_preferred);

        coordinator.unsubscribe(&subs, id_required);
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.route_type(&id_required), None);
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent),
            "a preferred request should reclaim the opened slot before queued no-preference compaction work"
        );
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_no_preference),
            None
        );
    }

    #[tokio::test]
    async fn incoming_preferred_request_reclaims_live_compaction_slot_from_no_preference() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(18);
        let id_no_preference = OutboxSubId(19);
        let id_incoming_preferred = OutboxSubId(20);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );
        insert_sub_with_policy(
            &mut subs,
            id_incoming_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_no_preference);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);
        coordinator.apply_capacity_available(&subs);
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_no_preference),
            Some(RelayReqStatus::InitialQuery),
            "increasing capacity should materialize the queued no-preference compaction request"
        );

        coordinator.subscribe(&subs, id_incoming_preferred);

        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_incoming_preferred),
            Some(RelayType::Transparent),
            "the incoming preferred request should reclaim the live compaction slot instead of falling behind no-preference work"
        );
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction),
            "the displaced no-preference request should return to compaction"
        );
    }

    #[tokio::test]
    async fn required_transparent_does_not_fallback_to_compaction_when_full() {
        let mut subs = OutboxSubscriptions::default();
        let id_a = OutboxSubId(20);
        let id_b = OutboxSubId(21);
        insert_sub_with_policy(&mut subs, id_a, RelayRoutingPreference::RequireDedicated);
        insert_sub_with_policy(&mut subs, id_b, RelayRoutingPreference::RequireDedicated);

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_a);

        coordinator.subscribe(&subs, id_b);

        assert_eq!(coordinator.route_type(&id_a), Some(RelayType::Transparent));
        assert_eq!(coordinator.route_type(&id_b), Some(RelayType::Transparent));
        assert!(coordinator.transparent_data.contains(&id_a));
        assert!(!coordinator.transparent_data.contains(&id_b));
        assert!(coordinator.compaction_data.req_status(&id_b).is_none());

        coordinator.unsubscribe(&subs, id_a);
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.route_type(&id_a), None);
        assert_eq!(coordinator.route_type(&id_b), Some(RelayType::Transparent));
        assert!(coordinator.transparent_data.contains(&id_b));
    }

    #[tokio::test]
    async fn required_transparent_can_demote_non_preferred_and_take_slot() {
        let mut subs = OutboxSubscriptions::default();
        let id_default = OutboxSubId(30);
        let id_required = OutboxSubId(31);
        insert_sub_with_policy(&mut subs, id_default, RelayRoutingPreference::NoPreference);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_default);

        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.route_type(&id_default),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert!(!coordinator.transparent_data.contains(&id_default));
        assert!(coordinator.transparent_data.contains(&id_required));
    }

    #[tokio::test]
    async fn required_transparent_demotes_entire_shared_non_preferred_leg() {
        let mut subs = OutboxSubscriptions::default();
        let id_shared_a = OutboxSubId(31_116);
        let id_shared_b = OutboxSubId(31_117);
        let id_required = OutboxSubId(31_118);
        let shared_filter = Filter::new().kinds([1]).limit(1).build();
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_shared_a,
            RelayRoutingPreference::NoPreference,
            vec![shared_filter.clone()],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_shared_b,
            RelayRoutingPreference::NoPreference,
            vec![shared_filter],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([2]).limit(1).build()],
        );

        let mut coordinator = coordinator_with_limit(1);
        coordinator.subscribe(&subs, id_shared_a);
        coordinator.subscribe(&subs, id_shared_b);

        let shared_sid = coordinator
            .transparent_data
            .active_sid(&id_shared_a)
            .expect("first owner should have a transparent sid");
        assert_eq!(
            coordinator.transparent_data.active_sid(&id_shared_b),
            Some(shared_sid.clone()),
            "test setup requires both owners to share one relay REQ"
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.route_type(&id_shared_a),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.route_type(&id_shared_b),
            Some(RelayType::Compaction),
            "demoting a shared transparent sid must reroute every owner"
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert!(!coordinator.transparent_data.contains(&id_shared_a));
        assert!(!coordinator.transparent_data.contains(&id_shared_b));
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
    }

    #[tokio::test]
    async fn required_transparent_does_not_partially_demote_shared_required_leg() {
        let mut subs = OutboxSubscriptions::default();
        let id_shared_default = OutboxSubId(31_119);
        let id_shared_required = OutboxSubId(31_120);
        let id_incoming_required = OutboxSubId(31_121);
        let shared_filter = Filter::new().kinds([1]).limit(1).build();
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_shared_default,
            RelayRoutingPreference::NoPreference,
            vec![shared_filter.clone()],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_shared_required,
            RelayRoutingPreference::RequireDedicated,
            vec![shared_filter],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_incoming_required,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([2]).limit(1).build()],
        );

        let mut coordinator = coordinator_with_limit(1);
        coordinator.subscribe(&subs, id_shared_default);
        coordinator.subscribe(&subs, id_shared_required);

        let shared_sid = coordinator
            .transparent_data
            .active_sid(&id_shared_default)
            .expect("first owner should have a transparent sid");
        assert_eq!(
            coordinator.transparent_data.active_sid(&id_shared_required),
            Some(shared_sid),
            "test setup requires required and non-required owners to share one relay REQ"
        );

        coordinator.subscribe(&subs, id_incoming_required);

        assert_eq!(
            coordinator.route_type(&id_shared_default),
            Some(RelayType::Transparent),
            "non-required owner must not be partially demoted while sharing a required leg"
        );
        assert_eq!(
            coordinator.route_type(&id_shared_required),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_shared_default));
        assert!(coordinator.transparent_data.contains(&id_shared_required));
        assert_eq!(
            coordinator.route_type(&id_incoming_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_incoming_required),
            0
        );
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
    }

    #[tokio::test]
    async fn active_required_growth_does_not_yield_pass_to_preferred_promotion() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(31_000);
        let id_preferred = OutboxSubId(31_001);
        let required_filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&required_filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([1]).limit(1).build()],
        );
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-active-growth-promotion.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: single_filter_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.subscribe(&subs, id_preferred);

        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_preferred));
        assert_eq!(coordinator.preferred_compaction_promotions.len(), 1);

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            required_filters,
        );
        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1,
            "required route must keep its existing leg while waiting for the additional pass"
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.transparent_data.queued_len_for_test(),
            1,
            "required route should be queued for a later resize"
        );
        assert!(
            !coordinator.transparent_data.contains(&id_preferred),
            "preferred route must not promote before required fallback finishes"
        );
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Compaction)
        );
        assert_eq!(coordinator.preferred_compaction_promotions.len(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn queued_active_required_resize_waits_for_full_extra_capacity() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(31_002);
        let required_filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
            Filter::new().kinds([3]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&required_filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([1]).limit(1).build()],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-queued-active-resize.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: single_filter_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            required_filters,
        );
        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1,
            "queued flush must not surrender the existing leg when only one of two extra passes is available"
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);
    }

    #[tokio::test]
    async fn queued_active_required_unrepresentable_resize_preserves_active_route() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(31_003);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([1]).build()],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-queued-unrepresentable.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: 1,
            },
        );

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1,
            "unrepresentable queued flush must not drop the existing active leg"
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);
    }

    #[tokio::test]
    async fn req_limit_repack_missing_transparent_sub_cleans_active_leg() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(31_004);
        insert_sub_with_policy(&mut subs, id, RelayRoutingPreference::RequireDedicated);

        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        coordinator.subscribe(&subs, id);
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 1);

        subs.remove(&id);
        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: 399_999,
            },
        );

        assert_eq!(coordinator.route_type(&id), None);
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 0);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);
    }

    #[tokio::test]
    async fn active_non_required_unrepresentable_resize_preserves_transparent_status() {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(31_006);
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-active-unrepresentable.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_no_preference);

        let sid = coordinator
            .transparent_data
            .active_sid(&id_no_preference)
            .expect("active transparent route should have a sid");
        coordinator
            .transparent_data
            .set_req_status(&sid.to_string(), RelayReqStatus::Eose);
        assert_eq!(
            coordinator.transparent_data.req_status(&id_no_preference),
            Some(RelayReqStatus::Eose)
        );

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: 1,
            },
        );

        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_no_preference),
            1
        );
        assert_eq!(
            coordinator.transparent_data.req_status(&id_no_preference),
            Some(RelayReqStatus::Eose),
            "unrepresentable limit shrink must preserve active transparent progress"
        );
        assert!(coordinator
            .compaction_data
            .req_status(&id_no_preference)
            .is_none());
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[tokio::test]
    async fn filter_replacement_unrepresentable_route_ignores_stale_transparent_eose() {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(31_010);
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-stale-filter-eose.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_no_preference);

        assert!(
            coordinator
                .transparent_data
                .active_sid(&id_no_preference)
                .is_some(),
            "active transparent route should have a sid"
        );
        coordinator.limits.max_json_bytes = 1;
        assert!(subs.ingest_task(
            &id_no_preference,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: vec![bulky_filter(0x77)],
                relays: RelayUrlPkgs::new(
                    hashbrown::HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        RelayDemandPriority::Important,
                        RelayRoutingPreference::NoPreference,
                    ),
                ),
            })
        ));

        let ingest = coordinator.replace_subscribe(&subs, id_no_preference);

        assert!(
            !ingest.facts.eose_delta.sub_ids.contains(&id_no_preference),
            "EOSE from the old transparent REQ must not satisfy the replacement filters"
        );
        assert!(
            ingest.facts.invalidated_sub_ids.contains(&id_no_preference),
            "filter replacement must invalidate the old transparent relay leg"
        );
        assert!(
            coordinator
                .transparent_data
                .active_sid(&id_no_preference)
                .is_none(),
            "filter replacement must drop stale transparent sid mappings before fallback"
        );
    }

    #[tokio::test]
    async fn active_non_required_unrepresentable_reconnect_invalidates_stale_status() {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(31_007);
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );

        let _relay = NormRelayUrl::new(
            "wss://relay-coordinator-active-unrepresentable-reconnect.example.com",
        )
        .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_no_preference);

        let sid = coordinator
            .transparent_data
            .active_sid(&id_no_preference)
            .expect("active transparent route should have a sid");
        coordinator
            .transparent_data
            .set_req_status(&sid.to_string(), RelayReqStatus::Eose);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: 1,
            },
        );

        coordinator.current_generation = Some(1);

        let output = coordinator.replay_after_websocket_open(&subs, true);

        assert!(output.facts.invalidated_sub_ids.contains(&id_no_preference));
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_no_preference),
            0
        );
        assert_eq!(
            coordinator.transparent_data.req_status(&id_no_preference),
            None,
            "blocked reconnect replay must not preserve stale transparent EOSE"
        );
        assert!(coordinator
            .compaction_data
            .req_status(&id_no_preference)
            .is_none());
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
    }

    #[tokio::test]
    async fn active_non_required_capacity_deficit_preserves_transparent_when_compaction_cannot_place(
    ) {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(31_008);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
            vec![
                Filter::new().kinds([1]).build(),
                Filter::new().kinds([2]).build(),
                Filter::new().kinds([3]).build(),
            ],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-active-capacity-deficit.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_no_preference);

        let sid = coordinator
            .transparent_data
            .active_sid(&id_no_preference)
            .expect("active transparent route should have a sid");
        coordinator
            .transparent_data
            .set_req_status(&sid.to_string(), RelayReqStatus::Eose);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: 400_000,
            },
        );

        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator
                .transparent_data
                .active_leg_count(&id_no_preference),
            1
        );
        assert_eq!(
            coordinator.transparent_data.req_status(&id_no_preference),
            Some(RelayReqStatus::Eose),
            "capacity-deficit fallback must not drop active transparent progress when compaction would queue"
        );
        assert!(coordinator
            .compaction_data
            .req_status(&id_no_preference)
            .is_none());
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn max_sub_shrink_preserves_non_required_transparent_when_compaction_replacement_would_queue(
    ) {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(31_009);
        let id_required = OutboxSubId(31_010);
        let id_compaction = OutboxSubId(31_011);
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-max-shrink-preserve.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_no_preference);
        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 3);
        coordinator.apply_capacity_available(&subs);
        assert_eq!(
            coordinator.compaction_data.req_status(&id_compaction),
            Some(RelayReqStatus::InitialQuery)
        );
        assert_eq!(
            coordinator
                .compaction_data
                .downgrade_revocation_costs(&subs)
                .len(),
            1,
            "test setup requires one active compaction candidate before the shrink"
        );
        assert_eq!(coordinator.current_limits().maximum_subs, 3);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        let sid = coordinator
            .transparent_data
            .active_sid(&id_no_preference)
            .expect("active transparent route should have a sid");
        coordinator
            .transparent_data
            .set_req_status(&sid.to_string(), RelayReqStatus::Eose);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);

        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.transparent_data.req_status(&id_no_preference),
            Some(RelayReqStatus::Eose),
            "max-sub shrink must not trade active transparent progress for queued compaction"
        );
        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn limit_downgrade_simulates_all_owners_for_shared_transparent_sid() {
        let mut subs = OutboxSubscriptions::default();
        let id_shared_a = OutboxSubId(31_012);
        let id_shared_b = OutboxSubId(31_013);
        let id_compaction = OutboxSubId(31_014);
        let shared_filter = Filter::new().kinds([1]).limit(1).build();
        let compaction_filter = Filter::new().kinds([2]).limit(1).build();
        let shared_filter_size =
            ReqFilterLimits::filter_json_size(&shared_filter).expect("shared filter json");
        let compaction_filter_size =
            ReqFilterLimits::filter_json_size(&compaction_filter).expect("compaction filter json");
        let two_filter_json_limit =
            ReqFilterLimits::req_json_size(2, shared_filter_size + compaction_filter_size);

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_shared_a,
            RelayRoutingPreference::NoPreference,
            vec![shared_filter.clone()],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_shared_b,
            RelayRoutingPreference::NoPreference,
            vec![shared_filter],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
            vec![compaction_filter],
        );

        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: two_filter_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_shared_a);
        coordinator.subscribe(&subs, id_shared_b);
        let shared_sid = coordinator
            .transparent_data
            .active_sid(&id_shared_a)
            .expect("first shared owner should have a transparent sid");
        assert_eq!(
            coordinator.transparent_data.active_sid(&id_shared_b),
            Some(shared_sid.clone()),
            "test setup requires both owners to share one relay REQ"
        );

        let mut compaction_plan = CompactionOperationPlan::default();
        compaction_plan.sub(id_compaction);
        let _ = apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_plan);
        let _ = coordinator.set_compaction_route(&subs, id_compaction);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
        let limits = ReqFilterLimits::from_relay_limits(&coordinator.limits);
        assert!(
            coordinator
                .compaction_data
                .can_place_subscribes_with_passes(&subs, [id_shared_a], limits, 0,),
            "test setup requires one shared owner to fit in existing compaction"
        );
        assert!(
            !coordinator
                .compaction_data
                .can_place_subscribes_with_passes(&subs, [id_shared_a, id_shared_b], limits, 0,),
            "test setup requires both shared owners to exceed existing compaction capacity"
        );

        assert!(
            !coordinator.limit_reduction_transparent_fallback_can_place(
                &subs,
                &TransparentLimitReductionCandidate {
                    id: id_shared_a,
                    sid: shared_sid,
                    owner_ids: vec![id_shared_a, id_shared_b],
                    preference: RelayRoutingPreference::NoPreference,
                },
                &HashSet::new(),
                &[],
            ),
            "max-sub shrink simulation must reserve fallback for every owner on the shared sid"
        );
    }

    #[tokio::test]
    async fn multi_revocation_shrink_reconsiders_skipped_no_preference_before_required() {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(31_016);
        let id_required = OutboxSubId(31_017);
        let id_compaction = OutboxSubId(31_018);
        let no_preference_filter = bulky_filter(40);
        let compaction_filter = bulky_filter(80);
        let single_bulky_req_bytes = ReqFilterLimits::req_json_size(
            1,
            ReqFilterLimits::filter_json_size(&no_preference_filter)
                .expect("serialize no-preference filter")
                .max(
                    ReqFilterLimits::filter_json_size(&compaction_filter)
                        .expect("serialize compaction filter"),
                ),
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
            vec![no_preference_filter],
        );
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
            vec![compaction_filter],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-multi-shrink-reconsider.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: single_bulky_req_bytes,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_no_preference);
        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 3);
        coordinator.apply_capacity_available(&subs);
        assert_eq!(
            coordinator.compaction_data.req_status(&id_compaction),
            Some(RelayReqStatus::InitialQuery)
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
        let no_preference_sid = coordinator
            .transparent_data
            .active_sid(&id_no_preference)
            .expect("no-preference route should be active before shrink");
        assert!(
            !coordinator.limit_reduction_transparent_fallback_can_place(
                &subs,
                &TransparentLimitReductionCandidate {
                    id: id_no_preference,
                    sid: no_preference_sid,
                    owner_ids: vec![id_no_preference],
                    preference: RelayRoutingPreference::NoPreference,
                },
                &HashSet::new(),
                &[],
            ),
            "test setup requires the no-preference route to be skipped while compaction is still a candidate"
        );
        let required_sid = coordinator
            .transparent_data
            .active_sid(&id_required)
            .expect("required route should be active before the shrink");
        coordinator
            .transparent_data
            .set_req_status(&required_sid.to_string(), RelayReqStatus::Eose);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 1);

        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent),
            "required transparent route must survive after compaction is exhausted"
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert_eq!(
            coordinator.transparent_data.req_status(&id_required),
            Some(RelayReqStatus::Eose),
            "required route status must not be lost by selecting it before the skipped no-preference candidate is reconsidered"
        );
        assert!(
            !coordinator.transparent_data.contains(&id_no_preference),
            "skipped no-preference candidate should be reconsidered for the second revocation"
        );
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
    }

    #[tokio::test]
    async fn pending_fallback_capacity_preserves_later_active_transparent_route() {
        let mut subs = OutboxSubscriptions::default();
        let id_active = OutboxSubId(31_012);
        let id_filler = OutboxSubId(31_013);
        let id_compaction_seed = OutboxSubId(31_014);
        let id_pending = OutboxSubId(31_015);
        let active_filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
        ];
        let pending_filters = vec![
            Filter::new().kinds([4]).limit(1).build(),
            Filter::new().kinds([5]).limit(1).build(),
            Filter::new().kinds([6]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&pending_filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_active,
            RelayRoutingPreference::NoPreference,
            vec![Filter::new().kinds([1]).limit(1).build()],
        );
        insert_sub_with_policy(
            &mut subs,
            id_filler,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_compaction_seed,
            RelayRoutingPreference::NoPreference,
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_pending,
            RelayRoutingPreference::NoPreference,
            pending_filters,
        );

        let _relay = NormRelayUrl::new("wss://relay-coordinator-test.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: single_filter_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_active);
        coordinator.subscribe(&subs, id_filler);

        coordinator.subscribe(&subs, id_compaction_seed);

        coordinator.unsubscribe(&subs, id_filler);
        coordinator.apply_capacity_available(&subs);

        assert_eq!(
            coordinator.compaction_data.req_status(&id_compaction_seed),
            Some(RelayReqStatus::InitialQuery)
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_active,
            RelayRoutingPreference::NoPreference,
            active_filters,
        );
        let mut fallback_compaction = CompactionOperationPlan::default();
        fallback_compaction.sub(id_pending);
        let mut demoted = HashSet::new();

        let result = coordinator.route_transparent_request_with_fallback(
            &subs,
            &mut fallback_compaction,
            &mut demoted,
            id_active,
        );

        assert!(matches!(
            result.outcome,
            FallbackTransparentRouteOutcome::Preserved
        ));
        assert_eq!(
            coordinator.route_type(&id_active),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.transparent_data.active_leg_count(&id_active), 1);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[tokio::test]
    async fn required_transparent_growth_without_enough_demotions_keeps_lower_priority_live() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(35);
        let id_default = OutboxSubId(36);
        let required_filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
            Filter::new().kinds([3]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&required_filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            vec![Filter::new().kinds([1]).limit(1).build()],
        );
        insert_sub_with_policy(&mut subs, id_default, RelayRoutingPreference::NoPreference);

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-transparent-deficit-negative.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: single_filter_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_required);
        coordinator.subscribe(&subs, id_default);

        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1
        );
        assert!(coordinator.transparent_data.contains(&id_default));
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
            required_filters,
        );
        coordinator.subscribe(&subs, id_required);

        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.transparent_data.active_leg_count(&id_required),
            1,
            "required route should keep its existing leg while the over-limit retry is queued"
        );
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
        assert_eq!(
            coordinator.route_type(&id_default),
            Some(RelayType::Transparent),
            "lower-priority route should not be partially demoted when it cannot satisfy the deficit"
        );
        assert!(coordinator.transparent_data.contains(&id_default));
        assert_eq!(coordinator.compaction_data.req_status(&id_default), None);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    #[tokio::test]
    async fn fallback_to_compaction_clears_stale_transparent_queue_entry() {
        let mut subs = OutboxSubscriptions::default();
        let id_existing = OutboxSubId(40);
        let id_incoming = OutboxSubId(41);
        insert_sub_with_policy(
            &mut subs,
            id_existing,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_incoming,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_existing);

        coordinator.subscribe(&subs, id_incoming);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);

        update_sub_policy(&mut subs, id_incoming, RelayRoutingPreference::NoPreference);

        coordinator.subscribe(&subs, id_incoming);

        assert_eq!(
            coordinator.route_type(&id_incoming),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_incoming));
        assert_eq!(
            coordinator.transparent_data.queued_len_for_test(),
            0,
            "fallback to compaction should cancel stale transparent retries"
        );
    }

    #[tokio::test]
    async fn limit_downgrade_prefers_compaction_revoke_over_preferred_transparent() {
        let mut subs = OutboxSubscriptions::default();
        let id_a = OutboxSubId(50);
        let id_b = OutboxSubId(51);
        let id_compaction = OutboxSubId(52);
        insert_sub_with_policy(&mut subs, id_a, RelayRoutingPreference::PreferDedicated);
        insert_sub_with_policy(&mut subs, id_b, RelayRoutingPreference::PreferDedicated);
        insert_sub_with_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
        );

        let mut coordinator = coordinator_with_limit(2);

        coordinator.subscribe(&subs, id_a);
        coordinator.subscribe(&subs, id_b);

        coordinator.subscribe(&subs, id_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 3);
        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);

        let transparent_ids = [
            coordinator.transparent_data.contains(&id_a),
            coordinator.transparent_data.contains(&id_b),
        ];
        assert_eq!(
            transparent_ids
                .into_iter()
                .filter(|is_active| *is_active)
                .count(),
            2
        );
        assert_eq!(
            [coordinator.route_type(&id_a), coordinator.route_type(&id_b)]
                .into_iter()
                .filter(|route| *route == Some(RelayType::Compaction))
                .count(),
            0
        );
        assert_eq!(
            coordinator.route_type(&id_compaction),
            Some(RelayType::Compaction)
        );
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
        assert_eq!(coordinator.compaction_data.num_subs(), 0);
    }

    #[tokio::test]
    async fn limit_downgrade_prefers_compaction_revoke_over_required_transparent() {
        let mut subs = OutboxSubscriptions::default();
        let id_a = OutboxSubId(60);
        let id_b = OutboxSubId(61);
        let id_compaction = OutboxSubId(62);
        insert_sub_with_policy(&mut subs, id_a, RelayRoutingPreference::RequireDedicated);
        insert_sub_with_policy(&mut subs, id_b, RelayRoutingPreference::RequireDedicated);
        insert_sub_with_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
        );

        let mut coordinator = coordinator_with_limit(2);

        coordinator.subscribe(&subs, id_a);
        coordinator.subscribe(&subs, id_b);

        coordinator.subscribe(&subs, id_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 3);
        coordinator.apply_capacity_available(&subs);
        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);

        assert_eq!(
            [coordinator.route_type(&id_a), coordinator.route_type(&id_b)]
                .into_iter()
                .filter(|route| *route == Some(RelayType::Transparent))
                .count(),
            2
        );
        assert_eq!(coordinator.compaction_data.num_subs(), 0);
        assert_eq!(coordinator.transparent_data.num_subs(), 2);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[tokio::test]
    async fn limit_downgrade_prefers_no_preference_transparent_over_required() {
        let mut subs = OutboxSubscriptions::default();
        let id_no_preference = OutboxSubId(63);
        let id_required = OutboxSubId(64);
        let id_compaction = OutboxSubId(65);
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_compaction,
            RelayRoutingPreference::NoPreference,
        );

        let mut coordinator = coordinator_with_limit(2);

        coordinator.subscribe(&subs, id_no_preference);
        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 3);
        coordinator.apply_capacity_available(&subs);
        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);

        assert_eq!(
            coordinator.route_type(&id_required),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_required));
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_no_preference));
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[tokio::test]
    async fn limit_downgrade_requeues_required_when_no_lower_cost_victim_exists() {
        let mut subs = OutboxSubscriptions::default();
        let id_a = OutboxSubId(66);
        let id_b = OutboxSubId(67);
        insert_sub_with_policy(&mut subs, id_a, RelayRoutingPreference::RequireDedicated);
        insert_sub_with_policy(&mut subs, id_b, RelayRoutingPreference::RequireDedicated);

        let mut coordinator = coordinator_with_limit(2);
        assert!(matches!(
            coordinator.try_place_dedicated_route(&subs, id_a).result,
            Some(TransparentPlaceResult::Placed)
        ));
        assert!(matches!(
            coordinator.try_place_dedicated_route(&subs, id_b).result,
            Some(TransparentPlaceResult::Placed)
        ));

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 1);

        assert_eq!(
            [coordinator.route_type(&id_a), coordinator.route_type(&id_b)]
                .into_iter()
                .filter(|route| *route == Some(RelayType::Transparent))
                .count(),
            2
        );
        assert_eq!(coordinator.transparent_data.num_subs(), 1);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
    }

    #[tokio::test]
    async fn preferred_compaction_route_promotes_when_dedicated_slot_opens() {
        let mut subs = OutboxSubscriptions::default();
        let id_transparent = OutboxSubId(70);
        let id_preferred = OutboxSubId(71);
        insert_sub_with_policy(
            &mut subs,
            id_transparent,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_transparent);

        coordinator.subscribe(&subs, id_preferred);

        coordinator.unsubscribe(&subs, id_transparent);
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.route_type(&id_transparent), None);
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_preferred));
        assert!(coordinator
            .compaction_data
            .req_status(&id_preferred)
            .is_none());
    }

    #[tokio::test]
    async fn preferred_compaction_promotion_missing_sub_cleans_compaction_state() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(31_105);
        insert_sub_with_policy(&mut subs, id, RelayRoutingPreference::PreferDedicated);

        let mut coordinator = coordinator_with_limit(2);
        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id)
                .facts
                .invalidated_sub_ids,
        );
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Compaction));
        assert_eq!(
            coordinator.compaction_data.req_status(&id),
            Some(RelayReqStatus::InitialQuery)
        );

        subs.remove(&id);
        let _ = coordinator.promote_preferred_compaction_routes(&subs);

        assert_eq!(coordinator.route_type(&id), None);
        assert_eq!(coordinator.compaction_data.req_status(&id), None);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 2);
    }

    #[tokio::test]
    async fn no_preference_compaction_route_does_not_promote_when_dedicated_slot_opens() {
        let mut subs = OutboxSubscriptions::default();
        let id_transparent = OutboxSubId(80);
        let id_no_preference = OutboxSubId(81);
        insert_sub_with_policy(
            &mut subs,
            id_transparent,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_no_preference,
            RelayRoutingPreference::NoPreference,
        );

        let mut coordinator = coordinator_with_limit(1);

        coordinator.subscribe(&subs, id_transparent);

        coordinator.subscribe(&subs, id_no_preference);

        coordinator.unsubscribe(&subs, id_transparent);

        assert_eq!(coordinator.route_type(&id_transparent), None);
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_no_preference));
        coordinator.apply_capacity_available(&subs);
        assert!(coordinator
            .compaction_data
            .req_status(&id_no_preference)
            .is_some());
    }

    #[tokio::test]
    async fn preferred_compaction_route_promotes_on_limit_increase() {
        let mut subs = OutboxSubscriptions::default();
        let id_preferred = OutboxSubId(90);
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let id_required = OutboxSubId(91);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );

        let mut coordinator = coordinator_with_limit(1);
        coordinator.subscribe(&subs, id_required);

        coordinator.subscribe(&subs, id_preferred);
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Compaction)
        );

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_preferred));
        assert!(coordinator
            .compaction_data
            .req_status(&id_preferred)
            .is_none());
    }

    #[tokio::test]
    async fn unrepresentable_preferred_compaction_route_stays_queued_for_later_promotion() {
        let mut subs = OutboxSubscriptions::default();
        let id_preferred = OutboxSubId(90_003);
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-unrepresentable-promotion.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_preferred);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_preferred)
                .facts
                .invalidated_sub_ids,
        );

        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Compaction)
        );
        assert!(
            coordinator
                .compaction_data
                .req_status(&id_preferred)
                .is_some(),
            "preferred route should be active on compaction before the shrink"
        );
        assert_eq!(coordinator.preferred_compaction_promotions.len(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: 1,
            },
        );

        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Compaction)
        );
        assert!(
            coordinator
                .compaction_data
                .req_status(&id_preferred)
                .is_none(),
            "unrepresentable compaction route must not keep an active relay REQ"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
        assert!(!coordinator.transparent_data.contains(&id_preferred));
        assert_eq!(coordinator.preferred_compaction_promotions.len(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 2);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: 400_000,
            },
        );
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_preferred));
        assert!(coordinator
            .compaction_data
            .req_status(&id_preferred)
            .is_none());
        assert_eq!(coordinator.preferred_compaction_promotions.len(), 0);
    }

    #[tokio::test]
    async fn relay_open_released_unrepresentable_compaction_queues_transparent_retry() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(90_004);
        let filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id,
            RelayRoutingPreference::PreferDedicated,
            filters,
        );

        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id);
        let _ = apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let _ = coordinator.set_compaction_route(&subs, id);

        assert_eq!(coordinator.route_type(&id), Some(RelayType::Compaction));
        assert!(coordinator.compaction_data.req_status(&id).is_some());
        assert!(coordinator.relay_demand_entries.contains_key(&id));

        let _ = coordinator.apply_websocket_closed(0);
        coordinator.limits.max_json_bytes = single_filter_json_limit;
        let _ = coordinator.apply_websocket_opened(&subs, Duration::ZERO, 1);

        assert_eq!(
            coordinator.route_type(&id),
            Some(RelayType::Transparent),
            "released stored subscription must remain coordinator-owned"
        );
        assert!(
            coordinator.compaction_data.req_status(&id).is_none(),
            "released compaction request must not remain active"
        );
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 0);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert!(coordinator.relay_demand_entries.contains_key(&id));

        coordinator.limits.max_json_bytes = 400_000;
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 1);
        assert!(coordinator.compaction_data.req_status(&id).is_none());
    }

    #[tokio::test]
    async fn reconnect_replay_does_not_emit_stale_compaction_req_after_close() {
        let mut subs = OutboxSubscriptions::default();
        let id_promoted = OutboxSubId(90_005);
        let id_released = OutboxSubId(90_006);
        let promoted_filter = Filter::new().kinds([11]).limit(1).build();
        let released_filter = bulky_filter(120);
        let promoted_filter_size =
            ReqFilterLimits::filter_json_size(&promoted_filter).expect("promoted filter json");
        let released_filter_size =
            ReqFilterLimits::filter_json_size(&released_filter).expect("released filter json");
        let initial_json_limit = ReqFilterLimits::req_json_size(1, released_filter_size);
        let shrink_json_limit = ReqFilterLimits::req_json_size(1, promoted_filter_size);

        assert!(
            ReqFilterLimits::req_json_size(2, promoted_filter_size + released_filter_size)
                > initial_json_limit,
            "test setup requires initial compaction routes to stay split"
        );
        assert!(
            ReqFilterLimits::req_json_size(1, released_filter_size) > shrink_json_limit,
            "test setup requires one compaction route to release on reconnect"
        );
        assert!(
            ReqFilterLimits::req_json_size(1, promoted_filter_size) <= shrink_json_limit,
            "test setup requires the preferred compaction route to remain sendable"
        );

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_promoted,
            RelayRoutingPreference::PreferDedicated,
            vec![promoted_filter],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_released,
            RelayRoutingPreference::NoPreference,
            vec![released_filter],
        );

        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: initial_json_limit,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_promoted);
        compaction_session.sub(id_released);
        let _ = apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let _ = coordinator.set_compaction_route(&subs, id_promoted);
        let _ = coordinator.set_compaction_route(&subs, id_released);

        assert_eq!(
            coordinator.compaction_data.num_subs(),
            2,
            "test setup requires separate compaction relay REQs"
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        let _ = coordinator.apply_websocket_closed(0);
        coordinator.limits.max_json_bytes = shrink_json_limit;

        let frames = send_frame_jsons(
            coordinator
                .apply_websocket_opened(&subs, Duration::ZERO, 1)
                .output,
        );

        let mut closed_sids = HashSet::new();
        for frame in &frames {
            let (kind, sid) = captured_frame_kind_and_sid(frame);
            if kind == "REQ" {
                assert!(
                    !closed_sids.contains(&sid),
                    "stale reconnect replay emitted REQ after CLOSE for {sid}: {frames:?}"
                );
            }
            if kind == "CLOSE" {
                closed_sids.insert(sid);
            }
        }
        assert_eq!(
            coordinator.route_type(&id_promoted),
            Some(RelayType::Transparent)
        );
        assert!(coordinator
            .compaction_data
            .req_status(&id_promoted)
            .is_none());
    }

    #[tokio::test]
    async fn json_limit_repack_queues_unrepresentable_compaction_route() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(31_041);
        let filters = vec![
            Filter::new().kinds([1]).limit(1).build(),
            Filter::new().kinds([2]).limit(1).build(),
        ];
        let single_filter_json_limit = one_filter_req_json_limit(&filters);
        insert_sub_with_filters_and_policy(
            &mut subs,
            id,
            RelayRoutingPreference::NoPreference,
            filters,
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-compaction-to-queue.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut session = CompactionOperationPlan::default();
        session.sub(id);
        let transition = apply_compaction_plan_for_test(&mut coordinator, &subs, session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id)
                .facts
                .invalidated_sub_ids,
        );

        assert_eq!(coordinator.route_type(&id), Some(RelayType::Compaction));
        assert_eq!(
            coordinator.compaction_data.req_status(&id),
            Some(RelayReqStatus::InitialQuery)
        );

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 3,
                max_json_bytes: single_filter_json_limit,
            },
        );

        assert_eq!(coordinator.route_type(&id), Some(RelayType::Compaction));
        assert_eq!(coordinator.compaction_data.req_status(&id), None);
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
        assert_eq!(coordinator.transparent_data.active_leg_count(&id), 0);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[tokio::test]
    async fn json_limit_repack_queues_unrepresentable_and_splits_representable_compaction_req() {
        let mut subs = OutboxSubscriptions::default();
        let id_queued = OutboxSubId(31_042);
        let id_split_a = OutboxSubId(31_043);
        let id_split_b = OutboxSubId(31_044);
        let queued_filter = bulky_filter(42);
        let split_filter_a = Filter::new().kinds([1]).limit(1).build();
        let split_filter_b = Filter::new().kinds([2]).limit(1).build();
        let queued_filter_size =
            ReqFilterLimits::filter_json_size(&queued_filter).expect("queued filter json");
        let split_filter_a_size =
            ReqFilterLimits::filter_json_size(&split_filter_a).expect("split filter a json");
        let split_filter_b_size =
            ReqFilterLimits::filter_json_size(&split_filter_b).expect("split filter b json");
        let initial_json_limit = ReqFilterLimits::req_json_size(1, queued_filter_size).max(
            ReqFilterLimits::req_json_size(2, split_filter_a_size + split_filter_b_size),
        ) + 8;
        let shrink_json_limit =
            ReqFilterLimits::req_json_size(1, split_filter_a_size.max(split_filter_b_size));

        assert!(
            ReqFilterLimits::req_json_size(2, queued_filter_size + split_filter_a_size)
                > initial_json_limit,
            "test setup requires queued and split filters to start on different REQs"
        );
        assert!(
            ReqFilterLimits::req_json_size(2, split_filter_a_size + split_filter_b_size)
                > shrink_json_limit,
            "test setup requires the split REQ to exceed the shrunken limit"
        );
        assert!(
            ReqFilterLimits::req_json_size(1, queued_filter_size) > shrink_json_limit,
            "test setup requires the queued request to become unrepresentable"
        );
        let initial_limits = ReqFilterLimits::new(200, initial_json_limit);
        assert_eq!(
            initial_limits
                .filters_fit_single_req(&[split_filter_a.clone(), split_filter_b.clone()]),
            Some(true),
            "test setup requires split filters to share one initial compaction REQ"
        );

        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued,
            RelayRoutingPreference::NoPreference,
            vec![queued_filter],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_split_a,
            RelayRoutingPreference::NoPreference,
            vec![split_filter_a],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_split_b,
            RelayRoutingPreference::NoPreference,
            vec![split_filter_b],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-scoped-compaction-preserve.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: initial_json_limit,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_queued);
        compaction_session.sub(id_split_a);
        compaction_session.sub(id_split_b);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_queued)
                .facts
                .invalidated_sub_ids,
        );
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_split_a)
                .facts
                .invalidated_sub_ids,
        );
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_split_b)
                .facts
                .invalidated_sub_ids,
        );
        assert_eq!(
            coordinator.compaction_data.num_subs(),
            2,
            "test setup requires queued and split groups on separate compaction REQs"
        );
        let output = coordinator.apply_capacity_available(&subs);
        invalidated_sub_ids.extend(output.facts.invalidated_sub_ids);
        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 3);
        assert_eq!(coordinator.current_limits().maximum_subs, 3);

        let ingest = coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 3,
                max_json_bytes: shrink_json_limit,
            },
        );

        assert_eq!(
            coordinator.route_type(&id_queued),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_queued),
            None,
            "unrepresentable compaction REQ should be queued, not kept active"
        );
        assert_eq!(
            coordinator.compaction_data.num_subs(),
            2,
            "representable unfit compaction REQ should be split"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
        assert!(ingest.facts.invalidated_sub_ids.contains(&id_split_a));
        assert!(ingest.facts.invalidated_sub_ids.contains(&id_split_b));
        assert!(ingest.facts.invalidated_sub_ids.contains(&id_queued));
    }

    #[tokio::test]
    async fn json_limit_repack_preserves_live_compaction_before_applying_queued_capacity() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(92);
        let id_live_compaction = OutboxSubId(93);
        let id_queued_compaction = OutboxSubId(94);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_live_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(1)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(32)],
        );

        let compaction_json_limit = subs
            .json_size(&id_live_compaction)
            .expect("live compaction size")
            + ReqFilterLimits::req_overhead()
            + 8;
        let _relay = NormRelayUrl::new("wss://relay-coordinator-repack.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: compaction_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_required);
        coordinator.subscribe(&subs, id_live_compaction);
        coordinator.subscribe(&subs, id_queued_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);
        coordinator.apply_capacity_available(&subs);
        let [active_before, queued_before] = if coordinator
            .compaction_data
            .req_status(&id_live_compaction)
            .is_some()
        {
            [id_live_compaction, id_queued_compaction]
        } else {
            [id_queued_compaction, id_live_compaction]
        };
        assert_eq!(
            coordinator.route_type(&active_before),
            Some(RelayType::Compaction)
        );
        assert!(
            coordinator
                .compaction_data
                .req_status(&active_before)
                .is_some(),
            "one queued no-preference request should materialize into the live compaction slot"
        );
        assert_eq!(
            coordinator.route_type(&queued_before),
            Some(RelayType::Compaction)
        );
        assert!(
            coordinator
                .compaction_data
                .req_status(&queued_before)
                .is_none(),
            "the second compaction request should stay queued before the JSON-limit repack"
        );

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: compaction_json_limit - 4,
            },
        );

        assert!(
            coordinator.compaction_data.req_status(&active_before).is_some(),
            "repacking live compaction routes for a smaller JSON limit should preserve the route that was already active"
        );
        assert!(
            coordinator
                .compaction_data
                .req_status(&queued_before)
                .is_none(),
            "queued compaction work should not steal the freed pass while the live route is being rebuilt"
        );
    }

    #[tokio::test]
    async fn unrepresentable_live_compaction_queues_on_limit_change_with_queued_work() {
        let mut subs = OutboxSubscriptions::default();
        let id_required = OutboxSubId(95);
        let id_live_compaction = OutboxSubId(96);
        let id_queued_compaction = OutboxSubId(97);
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_live_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(40)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(80)],
        );

        let compaction_json_limit = subs
            .json_size(&id_live_compaction)
            .expect("live compaction size")
            .max(
                subs.json_size(&id_queued_compaction)
                    .expect("queued compaction size"),
            )
            + ReqFilterLimits::req_overhead()
            + 8;
        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-unrepresentable-queued.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: compaction_json_limit,
        });
        open_coordinator(&mut coordinator);

        coordinator.subscribe(&subs, id_required);
        coordinator.subscribe(&subs, id_live_compaction);
        coordinator.subscribe(&subs, id_queued_compaction);

        set_max_size(&mut coordinator, &subs, &mut NegentropyData::default(), 2);
        coordinator.apply_capacity_available(&subs);
        let [active_before, queued_before] = if coordinator
            .compaction_data
            .req_status(&id_live_compaction)
            .is_some()
        {
            [id_live_compaction, id_queued_compaction]
        } else {
            [id_queued_compaction, id_live_compaction]
        };
        assert!(
            coordinator
                .compaction_data
                .req_status(&active_before)
                .is_some(),
            "test setup requires one live compaction request before the limit shrink"
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&queued_before),
            None,
            "test setup requires one queued compaction request before the limit shrink"
        );

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: 1,
            },
        );

        assert!(
            coordinator
                .compaction_data
                .req_status(&active_before)
                .is_none(),
            "unrepresentable active compaction must be queued instead of kept active"
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&queued_before),
            None,
            "queued compaction should remain queued"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 2);

        coordinator.apply_capacity_available(&subs);
        assert!(
            coordinator
                .compaction_data
                .req_status(&active_before)
                .is_none(),
            "later ingest queue drain must not activate unrepresentable compaction"
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&queued_before),
            None,
            "later ingest queue drain should leave queued compaction queued"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 2);
    }

    #[tokio::test]
    async fn transparent_repack_fallback_queues_unrepresentable_live_compaction() {
        let mut subs = OutboxSubscriptions::default();
        let id_live_compaction = OutboxSubId(98);
        let id_transparent = OutboxSubId(99);
        let transparent_filter_a = Filter::new().kinds([1]).limit(1).build();
        let transparent_filter_b = Filter::new().kinds([2]).limit(1).build();
        let transparent_json_limit = ReqFilterLimits::req_json_size(
            1,
            ReqFilterLimits::filter_json_size(&transparent_filter_a)
                .expect("transparent filter json")
                .max(
                    ReqFilterLimits::filter_json_size(&transparent_filter_b)
                        .expect("transparent filter json"),
                ),
        ) + 8;
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_live_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(96)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_transparent,
            RelayRoutingPreference::PreferDedicated,
            vec![transparent_filter_a, transparent_filter_b],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-transparent-fallback-preserve.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_live_compaction);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_live_compaction)
                .facts
                .invalidated_sub_ids,
        );

        coordinator.subscribe(&subs, id_transparent);
        assert!(
            coordinator
                .compaction_data
                .req_status(&id_live_compaction)
                .is_some(),
            "test setup requires active compaction before transparent repack"
        );
        assert_eq!(
            coordinator.route_type(&id_transparent),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: transparent_json_limit,
            },
        );

        assert!(
            coordinator
                .compaction_data
                .req_status(&id_live_compaction)
                .is_none(),
            "transparent fallback compaction must not keep an unrepresentable active REQ"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
    }

    #[tokio::test]
    async fn maximum_subs_shrink_does_not_preserve_unrepresentable_live_compaction() {
        let mut subs = OutboxSubscriptions::default();
        let id_live_compaction = OutboxSubId(100);
        let id_transparent = OutboxSubId(101);
        let transparent_filter = Filter::new().kinds([1]).limit(1).build();
        let transparent_json_limit = ReqFilterLimits::req_json_size(
            1,
            ReqFilterLimits::filter_json_size(&transparent_filter)
                .expect("transparent filter json"),
        ) + 8;
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_live_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(100)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_transparent,
            RelayRoutingPreference::NoPreference,
            vec![transparent_filter],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-unrepresentable-shrink.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_live_compaction);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_live_compaction)
                .facts
                .invalidated_sub_ids,
        );

        coordinator.subscribe(&subs, id_transparent);
        assert!(
            coordinator
                .compaction_data
                .req_status(&id_live_compaction)
                .is_some(),
            "test setup requires active compaction before max-sub shrink"
        );
        assert_eq!(
            coordinator.route_type(&id_transparent),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: transparent_json_limit,
            },
        );

        assert!(
            coordinator
                .compaction_data
                .req_status(&id_live_compaction)
                .is_none(),
            "max-sub shrink must not keep unrepresentable compaction active"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
        assert_eq!(
            coordinator.route_type(&id_transparent),
            Some(RelayType::Transparent),
            "non-required transparent route should stay active after unsendable compaction is queued"
        );
    }

    #[tokio::test]
    async fn unrepresentable_compaction_shrink_uses_authoritative_maximum_subs() {
        let mut subs = OutboxSubscriptions::default();
        let id_live_compaction = OutboxSubId(31_110);
        let id_second_compaction = OutboxSubId(31_111);
        let id_later = OutboxSubId(31_112);
        let small_json_limit = ReqFilterLimits::req_json_size(
            1,
            ReqFilterLimits::filter_json_size(&Filter::new().kinds([1]).limit(1).build())
                .expect("small filter json"),
        ) + 8;
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_live_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(110)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_second_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(111)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_later,
            RelayRoutingPreference::NoPreference,
            vec![Filter::new().kinds([1]).limit(1).build()],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-authoritative-max.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_live_compaction);
        compaction_session.sub(id_second_compaction);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_live_compaction)
                .facts
                .invalidated_sub_ids,
        );
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_second_compaction)
                .facts
                .invalidated_sub_ids,
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: small_json_limit,
            },
        );
        assert_eq!(
            coordinator.current_limits().maximum_subs,
            1,
            "current limits should report the relay's authoritative max"
        );
        assert_eq!(
            coordinator.limits.sub_guardian.total_passes(),
            1,
            "effective pass capacity must not exceed the authoritative max"
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_live_compaction),
            None,
            "unrepresentable compaction must not keep an active relay REQ"
        );
        assert_eq!(
            coordinator
                .compaction_data
                .req_status(&id_second_compaction),
            None
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 2);

        coordinator.unsubscribe(&subs, id_live_compaction);
        coordinator.subscribe(&subs, id_later);

        assert_eq!(
            coordinator.route_type(&id_later),
            Some(RelayType::Transparent),
            "new work may use the one real pass after unsendable compaction is queued"
        );
    }

    #[tokio::test]
    async fn req_limit_repack_flushes_required_retry_before_queued_compaction() {
        let mut subs = OutboxSubscriptions::default();
        let id_compaction_a = OutboxSubId(31_120);
        let id_compaction_b = OutboxSubId(31_121);
        let id_queued_required = OutboxSubId(31_122);
        let filter_a = Filter::new().kinds([1]).limit(1).build();
        let filter_b = Filter::new().kinds([2]).limit(1).build();
        let queued_filter = Filter::new().kinds([3]).limit(1).build();
        let largest_filter_json = [
            ReqFilterLimits::filter_json_size(&filter_a).expect("filter a json"),
            ReqFilterLimits::filter_json_size(&filter_b).expect("filter b json"),
            ReqFilterLimits::filter_json_size(&queued_filter).expect("queued filter json"),
        ]
        .into_iter()
        .max()
        .expect("filter sizes");
        let small_json_limit = ReqFilterLimits::req_json_size(1, largest_filter_json) - 1;
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_compaction_a,
            RelayRoutingPreference::NoPreference,
            vec![filter_a],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_compaction_b,
            RelayRoutingPreference::NoPreference,
            vec![filter_b],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued_required,
            RelayRoutingPreference::RequireDedicated,
            vec![queued_filter],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-repack-required-queue.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_compaction_a);
        compaction_session.sub(id_compaction_b);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_compaction_a)
                .facts
                .invalidated_sub_ids,
        );
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_compaction_b)
                .facts
                .invalidated_sub_ids,
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: small_json_limit,
            },
        );
        assert_eq!(coordinator.current_limits().maximum_subs, 1);
        assert_eq!(
            coordinator.limits.sub_guardian.total_passes(),
            1,
            "effective pass capacity must match the authoritative max"
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_compaction_a),
            None
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_compaction_b),
            None
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 2);

        let _ = coordinator.queue_dedicated_retry(&subs, id_queued_required);
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 1,
                max_json_bytes: 400_000,
            },
        );
        let _ = coordinator.apply_capacity_available(&subs);

        assert_eq!(coordinator.current_limits().maximum_subs, 1);
        assert_eq!(
            coordinator.limits.sub_guardian.total_passes(),
            1,
            "effective pass capacity must remain authoritative after repack"
        );
        assert!(
            coordinator
                .transparent_data
                .active_sid(&id_queued_required)
                .is_some(),
            "queued required transparent work should consume the one real available pass"
        );
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 2);
    }

    #[tokio::test]
    async fn transparent_queue_flush_fallback_queues_unrepresentable_live_compaction() {
        let mut subs = OutboxSubscriptions::default();
        let id_live_compaction = OutboxSubId(102);
        let id_demoted_transparent = OutboxSubId(103);
        let id_queued_required = OutboxSubId(104);
        let small_filter = Filter::new().kinds([1]).limit(1).build();
        let small_json_limit = ReqFilterLimits::req_json_size(
            1,
            ReqFilterLimits::filter_json_size(&small_filter).expect("small filter json"),
        ) + 8;
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_live_compaction,
            RelayRoutingPreference::NoPreference,
            vec![bulky_filter(104)],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_demoted_transparent,
            RelayRoutingPreference::NoPreference,
            vec![small_filter.clone()],
        );
        insert_sub_with_filters_and_policy(
            &mut subs,
            id_queued_required,
            RelayRoutingPreference::RequireDedicated,
            vec![small_filter],
        );

        let _relay =
            NormRelayUrl::new("wss://relay-coordinator-transparent-queue-preserve.example.com")
                .unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 2,
            max_json_bytes: 400_000,
        });
        open_coordinator(&mut coordinator);

        let mut compaction_session = CompactionOperationPlan::default();
        compaction_session.sub(id_live_compaction);
        let transition =
            apply_compaction_plan_for_test(&mut coordinator, &subs, compaction_session);
        let mut invalidated_sub_ids = transition.invalidated_sub_ids;
        invalidated_sub_ids.extend(
            coordinator
                .set_compaction_route(&subs, id_live_compaction)
                .facts
                .invalidated_sub_ids,
        );

        coordinator.subscribe(&subs, id_demoted_transparent);
        let _ = coordinator.queue_dedicated_retry(&subs, id_queued_required);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
        assert!(
            coordinator
                .compaction_data
                .req_status(&id_live_compaction)
                .is_some(),
            "test setup requires live compaction before transparent queue flush"
        );
        assert_eq!(
            coordinator.route_type(&id_demoted_transparent),
            Some(RelayType::Transparent)
        );
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        coordinator.set_limits(
            &subs,
            0,
            RelayLimitations {
                maximum_subs: 2,
                max_json_bytes: small_json_limit,
            },
        );

        assert!(
            coordinator
                .compaction_data
                .req_status(&id_live_compaction)
                .is_none(),
            "transparent queue fallback must not keep unrepresentable compaction active"
        );
        assert_eq!(coordinator.compaction_data.queued_len_for_test(), 1);
        assert_eq!(
            coordinator.route_type(&id_queued_required),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_demoted_transparent),
            Some(RelayType::Transparent)
        );
    }
}
