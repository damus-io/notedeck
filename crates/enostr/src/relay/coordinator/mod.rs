use ewebsock::{WsEvent, WsMessage};
use hashbrown::{HashMap, HashSet};
use ingest::{IngestExecutor, IngestPlanner};
use negentropy::NegentropyStorageVector;
use nostrdb::Filter;
use std::time::{Duration, Instant};
use transparent_routing::TransparentRoutingState;

use crate::{
    relay::{
        compaction::{CompactionData, CompactionRelay, CompactionSession},
        indexed_queue::IndexedQueue,
        negentropy::{NegentropyData, NegentropyRelay},
        nip11::Nip11FetchLifecycle,
        transparent::{
            take_revoked_transparent_subs, TransparentData, TransparentPlaceResult,
            TransparentRelay,
        },
        BroadcastCache, BroadcastRelay, FullHistorySubId, NormRelayUrl, OutboxSubId,
        OutboxSubscriptions, RawEventData, RelayCoordinatorLimits, RelayImplType, RelayLimitations,
        RelayReqId, RelayReqStatus, RelayRoutingPreference, RelayType, SubPassGuardian,
        WebsocketRelay, WebsocketSlot,
    },
    EventClientMessage, RelayMessage, Wakeup,
};

mod ingest;
mod transparent_routing;

/// RelayCoordinator routes each Outbox subscription to either the compaction or
/// transparent relay engine and tracks their status.
pub struct CoordinationData {
    limits: RelayCoordinatorLimits,
    pub(crate) websocket: WebsocketSlot,
    coordination: HashMap<OutboxSubId, RelayType>,
    compaction_data: CompactionData,
    transparent_data: TransparentData, // for outbox subs that prefer to be transparent
    pub(crate) negentropy_data: NegentropyData,
    transparent_routing: TransparentRoutingState,
    preferred_compaction_promotions: IndexedQueue<OutboxSubId>,
    broadcast_cache: BroadcastCache,
    eose_queue: Vec<RelayReqId>,
    pending_tracker_invalidations: HashSet<OutboxSubId>,
    pub(crate) nip11: Nip11FetchLifecycle,
}

/// Outcome of attempting to start a relay-local negentropy session.
pub(crate) enum NegentropyStartOutcome {
    /// The session started and the NEG-OPEN message was sent.
    Started,
    /// The relay could not start yet; retry later with the caller-owned storage.
    Retry,
    /// The prepared storage/session is no longer usable and should be dropped.
    Drop,
}

/// Cheap preflight for whether a relay can start a new NIP-77 session now.
enum NegentropyStartReadiness {
    Ready,
    Retry,
    Drop,
}

/// Outcome for the transparent probe pass before fallback work is enabled.
pub(super) enum ProbeTransparentRouteOutcome {
    Placed,
    NeedsCapacity,
    Skipped,
}

/// Outcome for the fallback-enabled transparent routing pass.
pub(super) enum FallbackTransparentRouteOutcome {
    Placed,
    Fallback,
    Queued,
    Skipped,
}

/// Planned compaction-side work created while applying a max-subscription
/// downgrade.
struct LimitDowngradePlan {
    compaction_revocations: Vec<crate::relay::SubPassRevocation>,
    fallback_compaction: CompactionSession,
}

/// One possible pass revocation target during limit reduction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LimitReductionTarget {
    Negentropy,
    Transparent(TransparentLimitReductionCandidate),
    Compaction,
}

/// One transparent route that can release a pass during limit reduction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransparentLimitReductionCandidate {
    id: OutboxSubId,
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

impl LimitDowngradePlan {
    /// Creates a downgrade plan seeded with any compaction pass revocations.
    fn new(compaction_revocations: Vec<crate::relay::SubPassRevocation>) -> Self {
        Self {
            compaction_revocations,
            fallback_compaction: CompactionSession::default(),
        }
    }

    /// Returns true when the compaction engine has downgrade work to apply.
    fn has_compaction_work(&self) -> bool {
        !self.compaction_revocations.is_empty() || !self.fallback_compaction.is_empty()
    }
}

impl CoordinationData {
    /// Creates relay coordination state without opening a websocket.
    pub(crate) fn new(limits: RelayLimitations) -> Self {
        let limits = RelayCoordinatorLimits::new(limits);
        let compaction_data = CompactionData::default();
        Self {
            limits,
            websocket: WebsocketSlot::empty(),
            compaction_data,
            transparent_data: TransparentData::default(),
            negentropy_data: NegentropyData::default(),
            transparent_routing: TransparentRoutingState::default(),
            preferred_compaction_promotions: IndexedQueue::default(),
            coordination: Default::default(),
            broadcast_cache: Default::default(),
            eose_queue: Vec::new(),
            pending_tracker_invalidations: HashSet::new(),
            nip11: Nip11FetchLifecycle::default(),
        }
    }

    /// Ensures this relay has a websocket leg, making the runtime-dependent
    /// connection attempt explicit at the caller.
    pub(crate) fn connect_websocket<W>(
        &mut self,
        norm_url: &NormRelayUrl,
        wakeup: W,
        force: bool,
    ) -> bool
    where
        W: Wakeup,
    {
        self.websocket
            .try_restore_with_wakeup(norm_url.clone().into(), wakeup, force)
    }

    /// Current effective relay limits being enforced by this coordinator.
    pub fn current_limits(&self) -> RelayLimitations {
        RelayLimitations {
            maximum_subs: self.limits.sub_guardian.total_passes(),
            max_json_bytes: self.limits.max_json_bytes,
        }
    }

    /// Apply new effective relay limits to the coordinator.
    pub fn set_limits(&mut self, subs: &OutboxSubscriptions, limits: RelayLimitations) {
        let json_limit_shrunk = limits.max_json_bytes < self.limits.max_json_bytes;
        self.limits.max_json_bytes = limits.max_json_bytes;
        self.set_max_size(subs, limits.maximum_subs);

        if json_limit_shrunk {
            self.repack_compaction_for_new_json_limit(subs);
        }
    }

    /// Change if we found a new NIP-11 `max_subscriptions`
    pub fn set_max_size(&mut self, subs: &OutboxSubscriptions, max_size: usize) {
        let previous_total = self.limits.sub_guardian.total_passes();
        let Some(revocations) = self.limits.new_total(max_size) else {
            if max_size > previous_total {
                self.flush_transparent_queue(subs);
                self.promote_preferred_compaction_routes(subs);
                self.drain_compaction_queue(subs);
            }
            return;
        };

        self.transparent_routing
            .rebuild_from_transparent(subs, &self.transparent_data);
        let targets = self.select_limit_reduction_targets(subs, revocations);

        self.revocate_negentropy_sessions(targets.negentropy_revocations);
        let transparent_ids = targets
            .transparent_revocations
            .iter()
            .map(|target| target.id)
            .collect();
        let transparent_revocations = targets
            .transparent_revocations
            .into_iter()
            .map(|target| target.revocation)
            .collect();
        let revoked_ids = take_revoked_transparent_subs(
            self.websocket.as_mut(),
            &mut self.transparent_data,
            transparent_ids,
            transparent_revocations,
        );
        let downgrade =
            self.plan_limit_downgrade(subs, revoked_ids, targets.compaction_revocations);
        self.execute_limit_downgrade_compaction(subs, downgrade);
        self.drain_compaction_queue(subs);
    }

    /// Selects exact negentropy, transparent, and compaction victims for a relay
    /// limit decrease by choosing the least disruptive next target each time.
    fn select_limit_reduction_targets(
        &self,
        subs: &OutboxSubscriptions,
        revocations: Vec<crate::relay::SubPassRevocation>,
    ) -> LimitReductionTargets {
        let mut negentropy_session_count = self.negentropy_data.active_session_count();
        let mut transparent_candidates = self
            .transparent_routing
            .limit_reduction_candidates()
            .into_iter()
            .filter_map(|id| {
                let preference = subs.routing_preference(&id)?;
                Some(TransparentLimitReductionCandidate { id, preference })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        let mut compaction_costs = self
            .compaction_data
            .downgrade_revocation_costs(subs)
            .into_iter()
            .peekable();

        let mut targets = LimitReductionTargets::default();

        for revocation in revocations {
            match Self::next_limit_reduction_target(
                negentropy_session_count > 0,
                transparent_candidates.peek().copied(),
                compaction_costs.peek().is_some(),
            ) {
                Some(LimitReductionTarget::Negentropy) => {
                    negentropy_session_count -= 1;
                    targets.negentropy_revocations.push(revocation);
                }
                Some(LimitReductionTarget::Transparent(candidate)) => {
                    let _ = transparent_candidates.next();
                    targets
                        .transparent_revocations
                        .push(TransparentLimitReduction {
                            id: candidate.id,
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

    /// Chooses the next least-disruptive limit-reduction target.
    fn next_limit_reduction_target(
        has_negentropy_session: bool,
        transparent: Option<TransparentLimitReductionCandidate>,
        has_compaction_candidate: bool,
    ) -> Option<LimitReductionTarget> {
        if has_negentropy_session {
            return Some(LimitReductionTarget::Negentropy);
        }

        let Some(transparent) = transparent else {
            return has_compaction_candidate.then_some(LimitReductionTarget::Compaction);
        };

        if !has_compaction_candidate {
            return Some(LimitReductionTarget::Transparent(transparent));
        }

        match transparent.preference {
            RelayRoutingPreference::NoPreference => {
                Some(LimitReductionTarget::Transparent(transparent))
            }
            RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::RequireDedicated => {
                Some(LimitReductionTarget::Compaction)
            }
        }
    }

    /// Revocate passes held by active negentropy sessions after
    /// max-subscription limit reduction selects them.
    fn revocate_negentropy_sessions(&mut self, revocations: Vec<crate::relay::SubPassRevocation>) {
        if revocations.is_empty() {
            return;
        }

        NegentropyRelay::new(
            self.websocket.as_mut(),
            &mut self.negentropy_data,
            &mut self.limits.sub_guardian,
        )
        .revocate_sessions(revocations);
    }

    /// Applies policy-aware rerouting for dedicated subscriptions evicted by a
    /// max-subscription downgrade and returns any resulting compaction work.
    fn plan_limit_downgrade(
        &mut self,
        subs: &OutboxSubscriptions,
        revoked_ids: Vec<OutboxSubId>,
        compaction_revocations: Vec<crate::relay::SubPassRevocation>,
    ) -> LimitDowngradePlan {
        let mut downgrade = LimitDowngradePlan::new(compaction_revocations);
        for id in revoked_ids {
            if subs.view(&id).is_none() {
                self.transparent_routing
                    .clear_route(&mut self.coordination, id);
                continue;
            }

            match subs.routing_preference(&id).unwrap_or_default() {
                RelayRoutingPreference::RequireDedicated => {
                    self.queue_dedicated_retry(subs, id);
                }
                RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::NoPreference => {
                    self.set_compaction_route(subs, id);
                    downgrade.fallback_compaction.sub(id);
                }
            }
        }

        self.transparent_routing
            .rebuild_from_transparent(subs, &self.transparent_data);

        downgrade
    }

    /// Executes the compaction-side effects needed after a max-subscription
    /// downgrade.
    fn execute_limit_downgrade_compaction(
        &mut self,
        subs: &OutboxSubscriptions,
        downgrade: LimitDowngradePlan,
    ) {
        if !downgrade.has_compaction_work() {
            return;
        }

        let LimitDowngradePlan {
            compaction_revocations,
            fallback_compaction,
        } = downgrade;
        let mut compaction = CompactionRelay::new(
            self.websocket.as_mut(),
            &mut self.compaction_data,
            self.limits.max_json_bytes,
            &mut self.limits.sub_guardian,
            subs,
        );
        if !compaction_revocations.is_empty() {
            self.pending_tracker_invalidations
                .extend(compaction.revocate_all(compaction_revocations));
        }
        if !fallback_compaction.is_empty() {
            self.pending_tracker_invalidations
                .extend(compaction.ingest_session_without_queue_drain(fallback_compaction));
        }
    }

    /// Attempts to place any queued compaction work using the pass capacity that
    /// remains after coordinator-level policy decisions complete.
    fn drain_compaction_queue(&mut self, subs: &OutboxSubscriptions) {
        if !self.compaction_data.has_queued_subs() {
            return;
        }

        self.pending_tracker_invalidations.extend(
            CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            )
            .drain_queue(),
        );
    }

    fn repack_compaction_for_new_json_limit(&mut self, subs: &OutboxSubscriptions) {
        let active = self.compaction_data.request_ids();
        if active.is_empty() {
            return;
        }

        let mut clear_session = CompactionSession::default();
        for id in &active {
            clear_session.unsub(*id);
        }
        self.pending_tracker_invalidations.extend(
            CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            )
            .ingest_session_without_queue_drain(clear_session),
        );

        let mut rebuild_session = CompactionSession::default();
        for id in active {
            rebuild_session.sub(id);
        }
        self.pending_tracker_invalidations.extend(
            CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            )
            .ingest_session_without_queue_drain(rebuild_session),
        );

        self.drain_compaction_queue(subs);
    }

    #[profiling::function]
    pub fn ingest_session(
        &mut self,
        subs: &OutboxSubscriptions,
        session: CoordinationSession,
    ) -> IngestSessionResult {
        let pending_eose = std::mem::take(&mut self.eose_queue);
        let plan = IngestPlanner::new(
            &self.coordination,
            &self.compaction_data,
            &self.transparent_data,
            pending_eose,
        )
        .plan(session);

        let eose_delta = IngestExecutor::new(self, subs).execute(plan);
        self.finalize_pending_effects(eose_delta)
    }

    /// Flushes pending coordinator-side relay effects without any new session
    /// work, keeping queued EOSE and tracker invalidations reconciled inside
    /// the coordinator boundary.
    pub fn flush_pending_effects(&mut self, subs: &OutboxSubscriptions) -> IngestSessionResult {
        self.ingest_session(subs, CoordinationSession::default())
    }

    /// Attempts dedicated placement during the first probe pass without
    /// demotion or compaction fallback.
    fn probe_transparent_request(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> ProbeTransparentRouteOutcome {
        let Some(placed) = self.try_place_transparent_route(subs, id) else {
            return ProbeTransparentRouteOutcome::Skipped;
        };

        match placed {
            TransparentPlaceResult::Placed => ProbeTransparentRouteOutcome::Placed,
            TransparentPlaceResult::NoRoom => ProbeTransparentRouteOutcome::NeedsCapacity,
        }
    }

    /// Materializes one transparent route immediately through the production
    /// transparent relay path and updates coordinator ownership if placed.
    fn try_place_transparent_route(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> Option<TransparentPlaceResult> {
        let view = subs.view(&id)?;
        let placed = {
            let mut transparent = TransparentRelay::new(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                &mut self.limits.sub_guardian,
            );
            transparent.try_subscribe(view)
        };

        if matches!(placed, TransparentPlaceResult::Placed) {
            self.set_transparent_route(subs, id);
        }

        Some(placed)
    }

    /// Attempts dedicated placement during the fallback-enabled pass. When
    /// necessary, one existing dedicated route may be demoted to compaction.
    fn route_transparent_request_with_fallback(
        &mut self,
        subs: &OutboxSubscriptions,
        fallback_compaction: &mut CompactionSession,
        demoted_in_current_pass: &mut HashSet<OutboxSubId>,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteOutcome {
        let Some(_) = subs.view(&id) else {
            return FallbackTransparentRouteOutcome::Skipped;
        };
        let policy = subs.routing_preference(&id).unwrap_or_default();
        let placed = self
            .try_place_transparent_route(subs, id)
            .expect("checked view above");

        match placed {
            TransparentPlaceResult::Placed => FallbackTransparentRouteOutcome::Placed,
            TransparentPlaceResult::NoRoom => {
                let Some(demoted) =
                    self.pick_demotion_candidate(policy, subs, id, demoted_in_current_pass)
                else {
                    return self.handle_unplaced_transparent_request(
                        policy,
                        subs,
                        fallback_compaction,
                        id,
                    );
                };

                {
                    let mut transparent = TransparentRelay::new(
                        self.websocket.as_mut(),
                        &mut self.transparent_data,
                        &mut self.limits.sub_guardian,
                    );
                    transparent.unsubscribe(demoted);
                }
                self.set_compaction_route(subs, demoted);
                fallback_compaction.sub(demoted);
                demoted_in_current_pass.insert(demoted);

                let Some(placed_after_demotion) = self.try_place_transparent_route(subs, id) else {
                    self.clear_route(id);
                    return FallbackTransparentRouteOutcome::Skipped;
                };

                if matches!(placed_after_demotion, TransparentPlaceResult::Placed) {
                    return FallbackTransparentRouteOutcome::Placed;
                }

                self.handle_unplaced_transparent_request(policy, subs, fallback_compaction, id)
            }
        }
    }

    fn pick_demotion_candidate(
        &mut self,
        policy: RelayRoutingPreference,
        subs: &OutboxSubscriptions,
        incoming: OutboxSubId,
        demoted_in_current_pass: &HashSet<OutboxSubId>,
    ) -> Option<OutboxSubId> {
        match policy {
            RelayRoutingPreference::RequireDedicated => {
                self.transparent_routing
                    .pick_demotable(subs, incoming, demoted_in_current_pass)
            }
            RelayRoutingPreference::PreferDedicated => {
                self.transparent_routing
                    .pick_non_preferred(subs, incoming, demoted_in_current_pass)
            }
            RelayRoutingPreference::NoPreference => None,
        }
    }

    fn handle_unplaced_transparent_request(
        &mut self,
        policy: RelayRoutingPreference,
        subs: &OutboxSubscriptions,
        fallback_compaction: &mut CompactionSession,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteOutcome {
        match policy {
            RelayRoutingPreference::RequireDedicated => self.queue_dedicated_retry(subs, id),
            RelayRoutingPreference::PreferDedicated | RelayRoutingPreference::NoPreference => {
                // Dedicated routing is best-effort for non-required requests; when saturated,
                // fallback this request to compaction.
                self.cancel_transparent_retry(id);
                self.set_compaction_route(subs, id);
                fallback_compaction.sub(id);
                FallbackTransparentRouteOutcome::Fallback
            }
        }
    }

    /// Queues a dedicated request for retry on the transparent engine without compaction fallback.
    fn queue_dedicated_retry(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> FallbackTransparentRouteOutcome {
        {
            let mut transparent = TransparentRelay::new(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                &mut self.limits.sub_guardian,
            );
            transparent.queue_subscribe(id);
        }
        self.set_transparent_route(subs, id);
        FallbackTransparentRouteOutcome::Queued
    }

    /// Removes any pending transparent retry for `id`.
    fn cancel_transparent_retry(&mut self, id: OutboxSubId) {
        let mut transparent = TransparentRelay::new(
            self.websocket.as_mut(),
            &mut self.transparent_data,
            &mut self.limits.sub_guardian,
        );
        transparent.unsubscribe(id);
    }

    /// Sends one outbound event message through this relay's broadcast path.
    pub fn send_event(&mut self, msg: EventClientMessage) {
        BroadcastRelay::websocket(self.websocket.as_mut(), &mut self.broadcast_cache)
            .broadcast(msg);
    }

    /// Marks `id` as transparently routed and clears any pending compaction
    /// promotion candidate.
    fn set_transparent_route(&mut self, subs: &OutboxSubscriptions, id: OutboxSubId) {
        self.preferred_compaction_promotions.remove(id);
        self.note_tracker_invalidation(id);
        self.transparent_routing
            .set_transparent_route(&mut self.coordination, subs, id);
    }

    /// Marks `id` as compaction-routed and indexes it for future promotion when
    /// its preference is `PreferDedicated`.
    fn set_compaction_route(&mut self, subs: &OutboxSubscriptions, id: OutboxSubId) {
        self.note_tracker_invalidation(id);
        self.transparent_routing
            .set_compaction_route(&mut self.coordination, id);

        if subs.routing_preference(&id) == Some(RelayRoutingPreference::PreferDedicated) {
            self.preferred_compaction_promotions
                .push_back_if_missing(id);
        } else {
            self.preferred_compaction_promotions.remove(id);
        }
    }

    /// Removes all coordinator ownership for `id` and clears promotion state.
    fn clear_route(&mut self, id: OutboxSubId) {
        self.preferred_compaction_promotions.remove(id);
        self.transparent_routing
            .clear_route(&mut self.coordination, id);
    }

    /// Records one relay leg whose durable EOSE state must be cleared because
    /// coordinator logic reset or rerouted it internally.
    fn note_tracker_invalidation(&mut self, id: OutboxSubId) {
        self.pending_tracker_invalidations.insert(id);
    }

    /// Returns the oldest still-valid preferred compaction candidate.
    fn pop_preferred_compaction_candidate(
        &mut self,
        subs: &OutboxSubscriptions,
    ) -> Option<OutboxSubId> {
        while let Some(id) = self.preferred_compaction_promotions.pop_front() {
            if self.coordination.get(&id) != Some(&RelayType::Compaction) {
                continue;
            }

            if subs.view(&id).is_none() {
                self.clear_route(id);
                continue;
            }

            if subs.routing_preference(&id) != Some(RelayRoutingPreference::PreferDedicated) {
                continue;
            }

            return Some(id);
        }

        None
    }

    /// Promotes compaction-routed preferred subscriptions into dedicated slots
    /// using any leftover pass capacity after current-session work completes.
    #[profiling::function]
    fn promote_preferred_compaction_routes(&mut self, subs: &OutboxSubscriptions) {
        let mut available = self.limits.sub_guardian.available_passes();
        if available == 0 {
            return;
        }

        let mut candidates = Vec::new();
        while available > 0 {
            let Some(id) = self.pop_preferred_compaction_candidate(subs) else {
                break;
            };
            candidates.push(id);
            available -= 1;
        }

        if candidates.is_empty() {
            return;
        }

        self.pending_tracker_invalidations.extend({
            let mut compaction = CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            );
            for id in &candidates {
                compaction.unsubscribe(*id);
            }
            compaction.take_session_invalidations()
        });

        let mut restore_compaction = CompactionSession::default();
        for id in candidates {
            let Some(_) = subs.view(&id) else {
                self.clear_route(id);
                continue;
            };

            let placed = self
                .try_place_transparent_route(subs, id)
                .expect("checked view above");

            match placed {
                TransparentPlaceResult::Placed => {}
                TransparentPlaceResult::NoRoom => {
                    self.set_compaction_route(subs, id);
                    restore_compaction.sub(id);
                }
            }
        }

        if restore_compaction.is_empty() {
            return;
        }

        self.pending_tracker_invalidations.extend(
            CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            )
            .ingest_session_without_queue_drain(restore_compaction),
        );
    }

    /// Flushes queued dedicated retries using the currently available pass pool.
    #[profiling::function]
    fn flush_transparent_queue(&mut self, subs: &OutboxSubscriptions) {
        let placed = {
            let mut transparent = TransparentRelay::new(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                &mut self.limits.sub_guardian,
            );
            transparent.try_flush_queue(subs)
        };

        for id in placed {
            self.set_transparent_route(subs, id);
        }
    }

    /// Returns and clears coordinator-reported relay-leg invalidations.
    fn drain_tracker_invalidations(&mut self) -> HashSet<OutboxSubId> {
        std::mem::take(&mut self.pending_tracker_invalidations)
    }

    /// Returns whether this relay still has unresolved queued EOSE or tracker
    /// invalidation work that must be ingested by outbox.
    pub(crate) fn has_pending_effects(&self) -> bool {
        !self.eose_queue.is_empty() || !self.pending_tracker_invalidations.is_empty()
    }

    /// Normalizes one relay's queued EOSE and invalidation effects into a
    /// single resolved delta for outbox to apply.
    fn finalize_pending_effects(&mut self, mut eose_delta: RelayEoseDelta) -> IngestSessionResult {
        eose_delta.invalidated_sub_ids = self.drain_tracker_invalidations();
        eose_delta.normalize();
        IngestSessionResult {
            eose_delta,
            has_pending_effects: self.has_pending_effects(),
        }
    }

    /// Returns the current request status for `id` if this coordinator still
    /// owns a relay leg for that subscription.
    pub fn req_status(&self, id: &OutboxSubId) -> Option<RelayReqStatus> {
        match self.coordination.get(id)? {
            RelayType::Compaction => self.compaction_data.req_status(id),
            RelayType::Transparent => self.transparent_data.req_status(id),
        }
    }

    /// Returns which relay engine currently owns this subscription, if any.
    pub(crate) fn route_type(&self, id: &OutboxSubId) -> Option<RelayType> {
        self.coordination.get(id).copied()
    }

    /// Returns how many transparent relay legs are currently active.
    #[cfg(test)]
    pub(crate) fn transparent_live_len_for_test(&self) -> usize {
        self.transparent_data.num_subs()
    }

    /// Returns whether one request currently owns an active transparent leg.
    #[cfg(test)]
    pub(crate) fn transparent_contains_for_test(&self, id: &OutboxSubId) -> bool {
        self.transparent_data.contains(id)
    }

    /// Returns how many transparent retry entries are currently queued.
    #[cfg(test)]
    pub(crate) fn transparent_queue_len_for_test(&self) -> usize {
        self.transparent_data.queued_len_for_test()
    }

    fn url(&self) -> &str {
        let Some(websocket) = self.websocket.as_ref() else {
            return "";
        };
        websocket.conn.url.as_str()
    }

    /// Tear down the current websocket leg and requeue any relay-local
    /// negentropy work that was in flight on it.
    pub(crate) fn disconnect_websocket_leg(&mut self) {
        let Some(websocket) = self.websocket.as_mut() else {
            return;
        };

        websocket.set_disconnected_now();
        NegentropyRelay::new(
            Some(websocket),
            &mut self.negentropy_data,
            &mut self.limits.sub_guardian,
        )
        .handle_relay_disconnect();
    }

    // whether we received
    #[profiling::function]
    pub(crate) fn try_recv<F>(
        &mut self,
        subs: &OutboxSubscriptions,
        reconnect_delay: Duration,
        act: &mut F,
    ) -> RecvResponse
    where
        for<'a> F: FnMut(RawEventData<'a>),
    {
        let event = {
            profiling::scope!("webscket try_recv");
            let Some(websocket) = self.websocket.as_mut() else {
                return RecvResponse::default();
            };
            let Some(event) = websocket.conn.receiver.try_recv() else {
                return RecvResponse::default();
            };
            event
        };

        let msg = match &event {
            WsEvent::Opened => {
                let Some(websocket) = self.websocket.as_mut() else {
                    return RecvResponse::received();
                };
                websocket.set_connected(reconnect_delay);
                self.pending_tracker_invalidations.extend(handle_relay_open(
                    websocket,
                    &mut self.broadcast_cache,
                    &mut self.compaction_data,
                    &mut self.transparent_data,
                    self.limits.max_json_bytes,
                    &mut self.limits.sub_guardian,
                    subs,
                ));
                None
            }
            WsEvent::Closed => {
                self.disconnect_websocket_leg();
                None
            }
            WsEvent::Error(err) => {
                tracing::error!("relay {} error: {:?}", self.url(), err);
                self.disconnect_websocket_leg();
                None
            }
            WsEvent::Message(ws_message) => {
                let Some(websocket) = self.websocket.as_mut() else {
                    return RecvResponse::received();
                };
                handle_websocket_message(websocket, ws_message)
            }
        };

        let resp = RecvResponse::received();
        let Some(msg) = msg else {
            return resp;
        };

        self.handle_relay_message(msg, act)
    }

    fn handle_relay_message<'a, F>(&'a mut self, msg: RelayMessage<'a>, act: &mut F) -> RecvResponse
    where
        F: FnMut(RawEventData<'a>),
    {
        let mut resp = RecvResponse::received();

        match msg {
            RelayMessage::OK(cr) => tracing::info!("OK {:?}", cr),
            RelayMessage::Eose(sid) => {
                tracing::debug!("Relay {} received EOSE for subscription: {sid}", self.url());
                self.compaction_data
                    .set_req_status(sid, RelayReqStatus::Eose);
                self.transparent_data
                    .set_req_status(sid, RelayReqStatus::Eose);
                self.eose_queue.push(RelayReqId(sid.to_string()));
                resp.eose_enqueued = true;
            }
            RelayMessage::Event(_, ev) => {
                profiling::scope!("ingest event");
                act(RawEventData {
                    url: self.url(),
                    event_json: ev,
                    relay_type: RelayImplType::Websocket,
                });
            }
            RelayMessage::Notice(msg) => tracing::warn!("Notice from {}: {}", self.url(), msg),
            RelayMessage::Closed(sid, _) => {
                tracing::trace!("Relay {} received CLOSED: {sid}", self.url());
                self.compaction_data
                    .set_req_status(sid, RelayReqStatus::Closed);
                self.transparent_data
                    .set_req_status(sid, RelayReqStatus::Closed);
            }
            RelayMessage::NegMsg(sub_id, payload) => {
                let message = {
                    let mut neg_relay = NegentropyRelay::new(
                        self.websocket.as_mut(),
                        &mut self.negentropy_data,
                        &mut self.limits.sub_guardian,
                    );
                    neg_relay.handle_neg_msg(sub_id.as_ref(), payload.as_ref())
                };
                if let Some(message) = message {
                    if let Some(relay) = self.websocket.as_mut() {
                        if relay.is_connected() {
                            relay.conn.send(&message);
                        }
                    }
                }
            }
            RelayMessage::NegErr(sub_id, reason) => {
                let mut neg_relay = NegentropyRelay::new(
                    self.websocket.as_mut(),
                    &mut self.negentropy_data,
                    &mut self.limits.sub_guardian,
                );
                neg_relay.handle_neg_err(sub_id.as_ref(), reason.as_ref());
            }
        }

        resp
    }

    /// Attempt to initiate a negentropy session after cheap relay capacity
    /// checks pass. `storage` is only evaluated once this relay can actually
    /// attempt `NEG-OPEN`.
    pub(crate) fn try_initiate_negentropy(
        &mut self,
        storage: impl FnOnce() -> NegentropyStorageVector,
        filter: Filter,
        owner_history_id: FullHistorySubId,
    ) -> NegentropyStartOutcome {
        match self.negentropy_start_readiness() {
            NegentropyStartReadiness::Ready => {}
            NegentropyStartReadiness::Retry => return NegentropyStartOutcome::Retry,
            NegentropyStartReadiness::Drop => return NegentropyStartOutcome::Drop,
        }

        let filter_json = match filter.json() {
            Ok(filter_json) => filter_json,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    filter_elements = filter.num_elements(),
                    "negentropy could not serialize NEG-OPEN filter"
                );
                return NegentropyStartOutcome::Drop;
            }
        };

        let storage = storage();
        let started = {
            let mut neg_relay = NegentropyRelay::new(
                self.websocket.as_mut(),
                &mut self.negentropy_data,
                &mut self.limits.sub_guardian,
            );
            neg_relay.try_initiate(storage, filter, filter_json, owner_history_id)
        };

        if let Some(msg) = started {
            if let Some(relay) = self.websocket.as_mut() {
                if relay.is_connected() {
                    relay.conn.send(&msg);
                    return NegentropyStartOutcome::Started;
                }
            }
        }

        NegentropyStartOutcome::Drop
    }

    fn negentropy_start_readiness(&self) -> NegentropyStartReadiness {
        if self.negentropy_data.is_unsupported() {
            return NegentropyStartReadiness::Drop;
        }

        let Some(relay) = self.websocket.as_ref() else {
            return NegentropyStartReadiness::Retry;
        };
        if !relay.is_connected() || self.limits.sub_guardian.available_passes() == 0 {
            return NegentropyStartReadiness::Retry;
        }

        NegentropyStartReadiness::Ready
    }

    /// Earliest active negentropy session timeout deadline for this relay.
    pub(crate) fn next_negentropy_deadline(&self) -> Option<Instant> {
        self.negentropy_data.next_timeout_deadline()
    }

    /// Whether this relay already has an active full-history negentropy session
    /// for the same owner/filter pair.
    pub(crate) fn has_active_negentropy_for_full_history(
        &self,
        owner_history_id: FullHistorySubId,
        filter: &Filter,
    ) -> bool {
        self.negentropy_data
            .has_active_session_for_owner_filter(owner_history_id, filter)
    }

    /// Poll sidecar negentropy state for relay-local timeouts.
    pub(crate) fn poll_negentropy_timeout(&mut self, now: Instant) {
        NegentropyRelay::new(
            self.websocket.as_mut(),
            &mut self.negentropy_data,
            &mut self.limits.sub_guardian,
        )
        .handle_timeout(now);
    }

    /// Cancel relay-local negentropy work owned by one durable subscription.
    pub(crate) fn cancel_negentropy_owner(&mut self, owner_history_id: FullHistorySubId) {
        NegentropyRelay::new(
            self.websocket.as_mut(),
            &mut self.negentropy_data,
            &mut self.limits.sub_guardian,
        )
        .cancel_owner(owner_history_id);
    }

    /// Cancel relay-local negentropy work owned by one sub for the given filters.
    pub(crate) fn cancel_negentropy_owner_filters(
        &mut self,
        owner_history_id: FullHistorySubId,
        filters: &[Filter],
    ) {
        NegentropyRelay::new(
            self.websocket.as_mut(),
            &mut self.negentropy_data,
            &mut self.limits.sub_guardian,
        )
        .cancel_owner_filters(owner_history_id, filters);
    }
}

/// Handles one raw websocket frame and returns a decoded relay message when the
/// frame carries Nostr payload data.
fn handle_websocket_message<'a>(
    websocket: &mut WebsocketRelay,
    ws_message: &'a WsMessage,
) -> Option<RelayMessage<'a>> {
    match ws_message {
        #[cfg(not(target_arch = "wasm32"))]
        WsMessage::Ping(bs) => {
            websocket.conn.sender.send(WsMessage::Pong(bs.clone()));
            None
        }
        WsMessage::Pong(_) => {
            websocket.last_pong = Instant::now();
            None
        }
        WsMessage::Text(text) => {
            tracing::trace!("relay {} received text: {}", websocket.conn.url, text);
            match RelayMessage::from_json(text) {
                Ok(msg) => Some(msg),
                Err(err) => {
                    tracing::error!(
                        "relay {} message decode error: {:?}",
                        websocket.conn.url,
                        err
                    );
                    None
                }
            }
        }
        _ => None,
    }
}

#[derive(Default)]
/// Non-blocking receive outcome for one `CoordinationData::try_recv` poll.
pub struct RecvResponse {
    /// At least one websocket event was consumed.
    pub received_event: bool,
    /// One or more relay EOSE markers were queued for ingest-time processing.
    pub eose_enqueued: bool,
}

impl RecvResponse {
    /// Returns the baseline outcome for a poll that consumed one websocket
    /// frame but has not yet classified any relay-side effects.
    pub fn received() -> Self {
        RecvResponse {
            received_event: true,
            eose_enqueued: false,
        }
    }
}

/// Result returned after coordinator ingestion for one relay.
pub struct IngestSessionResult {
    /// Resolved post-ingest relay effects for durable outbox tracking.
    pub eose_delta: RelayEoseDelta,
    /// Whether this relay still has unresolved queued effects after ingest.
    pub has_pending_effects: bool,
}

#[derive(Default)]
pub struct RelayEoseDelta {
    /// Subscriptions that reached EOSE for the current relay-query epoch.
    pub sub_ids: HashSet<OutboxSubId>,
    /// Subscriptions whose prior relay-query epoch was reset during this ingest.
    ///
    /// Invalidation wins over any stale queued EOSE resolved earlier in the same
    /// ingest, so this set must remain disjoint from `sub_ids`.
    pub invalidated_sub_ids: HashSet<OutboxSubId>,
}

impl RelayEoseDelta {
    /// Removes stale queued EOSE completions for subscriptions invalidated in
    /// the same coordinator ingest.
    fn normalize(&mut self) {
        self.sub_ids
            .retain(|id| !self.invalidated_sub_ids.contains(id));
        debug_assert!(
            self.sub_ids.is_disjoint(&self.invalidated_sub_ids),
            "RelayEoseDelta must not contain overlapping EOSE and invalidation IDs"
        );
    }
}

fn handle_relay_open(
    websocket: &mut WebsocketRelay,
    broadcast_cache: &mut BroadcastCache,
    compaction: &mut CompactionData,
    transparent: &mut TransparentData,
    max_json: usize,
    guardian: &mut SubPassGuardian,
    subs: &OutboxSubscriptions,
) -> HashSet<OutboxSubId> {
    BroadcastRelay::websocket(Some(websocket), broadcast_cache).try_flush_queue();
    let mut transparent = TransparentRelay::new(Some(websocket), transparent, guardian);
    let mut invalidated = transparent.handle_relay_open(subs);
    let mut compaction =
        CompactionRelay::new(Some(websocket), compaction, max_json, guardian, subs);
    invalidated.extend(compaction.handle_relay_open());
    invalidated
}

#[derive(Default)]
/// Batched per-subscription coordinator tasks for one relay frame.
pub struct CoordinationSession {
    pub tasks: HashMap<OutboxSubId, CoordinationTask>,
}

/// Per-subscription coordinator action staged for one relay.
pub enum CoordinationTask {
    /// Request routing for this subscription according to the provided preference.
    Subscribe(RelayRoutingPreference),
    /// Remove this subscription from whichever engine currently owns it.
    Unsubscribe,
}

impl CoordinationSession {
    /// Stage a subscription for dedicated routing with the given preference.
    pub fn subscribe(&mut self, id: OutboxSubId, routing_preference: RelayRoutingPreference) {
        self.tasks
            .insert(id, CoordinationTask::Subscribe(routing_preference));
    }

    /// Stage subscription removal for this relay.
    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        self.tasks.insert(id, CoordinationTask::Unsubscribe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{
        test_utils::{insert_sub_with_policy, MockWakeup},
        RelayUrlPkgs, SubscribeTask, WebsocketConn,
    };
    use nostrdb::Filter;

    /// Returns the task held for `id`, panicking when no matching task exists.
    #[track_caller]
    fn expect_task(session: &CoordinationSession, id: OutboxSubId) -> &CoordinationTask {
        session
            .tasks
            .get(&id)
            .unwrap_or_else(|| panic!("Expected task for {:?}", id))
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
                relays: RelayUrlPkgs::with_preference(HashSet::new(), policy),
            },
            false,
        );
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

    // ==================== CoordinationSession tests ====================

    /// Newly created coordination sessions hold no tasks.
    #[tokio::test]
    async fn coordination_session_default_empty() {
        let session = CoordinationSession::default();
        assert!(session.tasks.is_empty());
    }

    /// No-preference dedicated subscriptions should map to no-preference dedicated mode.
    #[tokio::test]
    async fn coordination_session_subscribe_no_preference_dedicated() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::NoPreference);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::NoPreference)
        ));
    }

    /// Prefer-dedicated subscriptions should map to dedicated-preferred mode.
    #[tokio::test]
    async fn coordination_session_subscribe_preferred_dedicated() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::PreferDedicated);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
    }

    /// Required-dedicated subscriptions should be recorded as required tasks.
    #[tokio::test]
    async fn coordination_session_subscribe_required_dedicated() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::RequireDedicated);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::RequireDedicated)
        ));
    }

    /// Unsubscribe should record an Unsubscribe task.
    #[tokio::test]
    async fn coordination_session_unsubscribe() {
        let mut session = CoordinationSession::default();

        session.unsubscribe(OutboxSubId(42));

        assert!(matches!(
            expect_task(&session, OutboxSubId(42)),
            CoordinationTask::Unsubscribe
        ));
    }

    /// Subsequent subscribe calls should overwrite previous modes.
    #[tokio::test]
    async fn coordination_session_subscribe_overwrites_previous() {
        let mut session = CoordinationSession::default();

        // First subscribe with a dedicated route.
        session.subscribe(OutboxSubId(0), RelayRoutingPreference::NoPreference);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::NoPreference)
        ));

        // Then upgrade to required dedicated routing.
        session.subscribe(OutboxSubId(0), RelayRoutingPreference::RequireDedicated);

        // Should reflect the latest dedicated intent.
        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::RequireDedicated)
        ));
    }

    /// Negentropy start should leave storage unbuilt when the relay is not
    /// connected yet so the attempt can be retried later.
    #[test]
    fn try_initiate_negentropy_retries_when_websocket_is_not_connected() {
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 4,
            max_json_bytes: 256_000,
        });
        let mut built_storage = false;

        let outcome = coordinator.try_initiate_negentropy(
            || {
                built_storage = true;
                NegentropyStorageVector::new()
            },
            Filter::new().build(),
            FullHistorySubId(0),
        );

        assert!(matches!(outcome, NegentropyStartOutcome::Retry));
        assert!(!built_storage);
    }

    #[tokio::test]
    async fn try_initiate_negentropy_drops_unserializable_filter_without_consuming_pass() {
        let mut coordinator = coordinator_with_limit(1);
        coordinator
            .websocket
            .as_mut()
            .expect("test websocket")
            .set_connected(WebsocketRelay::initial_reconnect_duration());
        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");

        let outcome = coordinator.try_initiate_negentropy(
            || storage,
            oversized_negentropy_filter(),
            FullHistorySubId(1),
        );

        assert!(matches!(outcome, NegentropyStartOutcome::Drop));
        assert_eq!(coordinator.negentropy_data.active_session_count(), 0);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 1);
    }

    #[tokio::test]
    async fn limit_downgrade_revokes_active_negentropy_sessions() {
        let subs = OutboxSubscriptions::default();
        let mut coordinator = coordinator_with_limit(2);
        coordinator
            .websocket
            .as_mut()
            .expect("test websocket")
            .set_connected(WebsocketRelay::initial_reconnect_duration());

        for id in [FullHistorySubId(0), FullHistorySubId(1)] {
            let mut storage = NegentropyStorageVector::new();
            storage.seal().expect("seal empty negentropy storage");
            let filter = Filter::new().kinds([1]).build();

            let outcome = coordinator.try_initiate_negentropy(|| storage, filter, id);

            assert!(matches!(outcome, NegentropyStartOutcome::Started));
        }
        assert_eq!(coordinator.negentropy_data.active_session_count(), 2);

        coordinator.set_max_size(&subs, 0);

        assert_eq!(coordinator.current_limits().maximum_subs, 0);
        assert_eq!(coordinator.negentropy_data.active_session_count(), 0);
        assert_eq!(coordinator.negentropy_data.drain_retry_neg_sets().len(), 2);
    }

    #[tokio::test]
    async fn notice_does_not_mark_active_negentropy_unsupported() {
        let mut coordinator = coordinator_with_limit(1);
        coordinator
            .websocket
            .as_mut()
            .expect("test websocket")
            .set_connected(WebsocketRelay::initial_reconnect_duration());

        let mut storage = NegentropyStorageVector::new();
        storage.seal().expect("seal empty negentropy storage");
        let filter = Filter::new().kinds([1]).build();

        let outcome = coordinator.try_initiate_negentropy(|| storage, filter, FullHistorySubId(1));

        assert!(matches!(outcome, NegentropyStartOutcome::Started));
        assert_eq!(coordinator.negentropy_data.active_session_count(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);

        let response = coordinator.handle_relay_message(
            RelayMessage::Notice("ERROR: bad msg: negentropy disabled"),
            &mut |_| {},
        );

        assert!(response.received_event);
        assert!(!coordinator.negentropy_data.is_unsupported());
        assert_eq!(coordinator.negentropy_data.active_session_count(), 1);
        assert_eq!(coordinator.limits.sub_guardian.available_passes(), 0);
    }

    /// Unsubscribe should override any prior subscribe entries.
    #[tokio::test]
    async fn coordination_session_unsubscribe_overwrites_subscribe() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::NoPreference);
        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::NoPreference)
        ));
        session.unsubscribe(OutboxSubId(0));

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Unsubscribe
        ));
    }

    /// Multiple tasks can be recorded in a single session.
    #[tokio::test]
    async fn coordination_session_multiple_tasks() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::PreferDedicated);
        session.subscribe(OutboxSubId(1), RelayRoutingPreference::RequireDedicated);
        session.unsubscribe(OutboxSubId(2));

        assert_eq!(session.tasks.len(), 3);
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
    async fn flush_pending_effects_normalizes_stale_queued_eose_against_invalidations() {
        let mut subs = OutboxSubscriptions::default();
        let id = OutboxSubId(9);
        insert_sub_with_policy(&mut subs, id, RelayRoutingPreference::PreferDedicated);

        let mut coordinator = coordinator_with_limit(1);
        assert!(matches!(
            coordinator.try_place_transparent_route(&subs, id),
            Some(TransparentPlaceResult::Placed)
        ));

        let sid = coordinator
            .transparent_data
            .active_sid(&id)
            .expect("transparent route should have a live sid");
        coordinator.eose_queue.push(sid);
        coordinator.pending_tracker_invalidations.insert(id);

        let ingest = coordinator.flush_pending_effects(&subs);
        assert!(ingest.eose_delta.sub_ids.is_empty());
        assert_eq!(ingest.eose_delta.invalidated_sub_ids, HashSet::from([id]));
        assert!(!ingest.has_pending_effects);
    }

    fn coordinator_with_limit(maximum_subs: usize) -> CoordinationData {
        let relay = NormRelayUrl::new("wss://relay-coordinator-test.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs,
            max_json_bytes: 400_000,
        });
        coordinator.connect_websocket(&relay, MockWakeup::default(), true);
        coordinator
    }

    #[test]
    fn coordination_data_new_does_not_open_websocket() {
        let coordinator = CoordinationData::new(RelayLimitations::default());

        assert!(coordinator.websocket.as_ref().is_none());
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

        let mut seed = CoordinationSession::default();
        seed.subscribe(id_default, RelayRoutingPreference::NoPreference);
        seed.subscribe(id_preferred, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, seed);

        assert_eq!(
            coordinator.route_type(&id_default),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Transparent)
        );

        let mut second = CoordinationSession::default();
        second.subscribe(id_incoming, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, second);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_a, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_b, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, second);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(
            id_existing_preferred,
            RelayRoutingPreference::PreferDedicated,
        );
        coordinator.ingest_session(&subs, second);

        let mut third = CoordinationSession::default();
        third.subscribe(
            id_incoming_preferred,
            RelayRoutingPreference::PreferDedicated,
        );
        coordinator.ingest_session(&subs, third);

        let mut fourth = CoordinationSession::default();
        fourth.unsubscribe(id_required);
        coordinator.ingest_session(&subs, fourth);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_no_preference, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        let mut third = CoordinationSession::default();
        third.subscribe(id_preferred, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, third);

        let mut fourth = CoordinationSession::default();
        fourth.unsubscribe(id_required);
        coordinator.ingest_session(&subs, fourth);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_no_preference, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        coordinator.set_max_size(&subs, 2);
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.compaction_data.req_status(&id_no_preference),
            Some(RelayReqStatus::InitialQuery),
            "increasing capacity should materialize the queued no-preference compaction request"
        );

        let mut third = CoordinationSession::default();
        third.subscribe(
            id_incoming_preferred,
            RelayRoutingPreference::PreferDedicated,
        );
        coordinator.ingest_session(&subs, third);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_a, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_b, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, second);

        assert_eq!(coordinator.route_type(&id_a), Some(RelayType::Transparent));
        assert_eq!(coordinator.route_type(&id_b), Some(RelayType::Transparent));
        assert!(coordinator.transparent_data.contains(&id_a));
        assert!(!coordinator.transparent_data.contains(&id_b));
        assert!(coordinator.compaction_data.req_status(&id_b).is_none());

        let mut third = CoordinationSession::default();
        third.unsubscribe(id_a);
        coordinator.ingest_session(&subs, third);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_default, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, second);

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

        let mut seed = CoordinationSession::default();
        seed.subscribe(id_existing, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, seed);

        let mut first_incoming = CoordinationSession::default();
        first_incoming.subscribe(id_incoming, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first_incoming);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);

        subs.get_mut(&id_incoming)
            .expect("incoming subscription metadata")
            .routing_preference = RelayRoutingPreference::NoPreference;

        let mut second = CoordinationSession::default();
        second.subscribe(id_incoming, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_a, RelayRoutingPreference::PreferDedicated);
        first.subscribe(id_b, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_compaction, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        coordinator.set_max_size(&subs, 3);
        coordinator.set_max_size(&subs, 2);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_a, RelayRoutingPreference::RequireDedicated);
        first.subscribe(id_b, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_compaction, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        coordinator.set_max_size(&subs, 3);
        coordinator.set_max_size(&subs, 2);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_no_preference, RelayRoutingPreference::NoPreference);
        first.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_compaction, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        coordinator.set_max_size(&subs, 3);
        coordinator.set_max_size(&subs, 2);

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
            coordinator.try_place_transparent_route(&subs, id_a),
            Some(TransparentPlaceResult::Placed)
        ));
        assert!(matches!(
            coordinator.try_place_transparent_route(&subs, id_b),
            Some(TransparentPlaceResult::Placed)
        ));

        coordinator.set_max_size(&subs, 1);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_transparent, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_preferred, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, second);

        let mut session = CoordinationSession::default();
        session.unsubscribe(id_transparent);
        coordinator.ingest_session(&subs, session);

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

        let mut first = CoordinationSession::default();
        first.subscribe(id_transparent, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_no_preference, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        let mut session = CoordinationSession::default();
        session.unsubscribe(id_transparent);
        coordinator.ingest_session(&subs, session);

        assert_eq!(coordinator.route_type(&id_transparent), None);
        assert_eq!(
            coordinator.route_type(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_no_preference));
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
        let mut first = CoordinationSession::default();
        first.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        coordinator.ingest_session(&subs, first);

        let mut second = CoordinationSession::default();
        second.subscribe(id_preferred, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, second);
        assert_eq!(
            coordinator.route_type(&id_preferred),
            Some(RelayType::Compaction)
        );

        coordinator.set_max_size(&subs, 2);

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
    async fn json_limit_repack_preserves_live_compaction_before_draining_queue() {
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
            + 8;
        let relay = NormRelayUrl::new("wss://relay-coordinator-repack.example.com").unwrap();
        let mut coordinator = CoordinationData::new(RelayLimitations {
            maximum_subs: 1,
            max_json_bytes: compaction_json_limit,
        });
        coordinator.connect_websocket(&relay, MockWakeup::default(), true);

        let mut initial = CoordinationSession::default();
        initial.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        initial.subscribe(id_live_compaction, RelayRoutingPreference::NoPreference);
        initial.subscribe(id_queued_compaction, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, initial);

        coordinator.set_max_size(&subs, 2);
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
    async fn websocket_pong_refreshes_last_pong() {
        let mut websocket = WebsocketRelay::new(
            WebsocketConn::from_wakeup(
                nostr::RelayUrl::parse("wss://relay-coordinator-pong.example.com").unwrap(),
                MockWakeup::default(),
            )
            .unwrap(),
        );
        websocket.last_pong = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let before = websocket.last_pong;

        let pong = WsMessage::Pong(vec![]);
        let msg = handle_websocket_message(&mut websocket, &pong);

        assert!(msg.is_none());
        assert!(websocket.last_pong > before);
    }
}
