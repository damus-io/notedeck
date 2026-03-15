use ewebsock::{WsEvent, WsMessage};
use hashbrown::{HashMap, HashSet};
use ingest::{IngestExecutor, IngestPlanner};
use transparent_routing::TransparentRoutingState;

use crate::{
    relay::{
        compaction::{CompactionData, CompactionRelay, CompactionSession},
        indexed_queue::IndexedQueue,
        nip11::Nip11FetchLifecycle,
        transparent::{
            take_revoked_transparent_subs, TransparentData, TransparentPlaceResult,
            TransparentRelay,
        },
        BroadcastCache, BroadcastRelay, NormRelayUrl, OutboxSubId, OutboxSubscriptions,
        RawEventData, RelayCoordinatorLimits, RelayImplType, RelayLimitations, RelayReqId,
        RelayReqStatus, RelayRoutingPreference, RelayType, SubPassGuardian, WebsocketRelay,
        WebsocketSlot,
    },
    EventClientMessage, RelayMessage, RelayStatus, Wakeup,
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
    transparent_routing: TransparentRoutingState,
    preferred_compaction_promotions: IndexedQueue<OutboxSubId>,
    broadcast_cache: BroadcastCache,
    eose_queue: Vec<RelayReqId>,
    pending_tracker_invalidations: HashSet<OutboxSubId>,
    pub(crate) nip11: Nip11FetchLifecycle,
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
    Transparent {
        id: OutboxSubId,
        preference: RelayRoutingPreference,
    },
    Compaction,
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
    pub fn new<W>(limits: RelayLimitations, norm_url: NormRelayUrl, wakeup: W) -> Self
    where
        W: Wakeup,
    {
        let websocket = WebsocketSlot::from_wakeup(norm_url.clone().into(), wakeup);
        let limits = RelayCoordinatorLimits::new(limits);
        let compaction_data = CompactionData::default();
        Self {
            limits,
            websocket,
            compaction_data,
            transparent_data: TransparentData::default(),
            transparent_routing: TransparentRoutingState::default(),
            preferred_compaction_promotions: IndexedQueue::default(),
            coordination: Default::default(),
            broadcast_cache: Default::default(),
            eose_queue: Vec::new(),
            pending_tracker_invalidations: HashSet::new(),
            nip11: Nip11FetchLifecycle::default(),
        }
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
            }
            return;
        };

        self.transparent_routing
            .rebuild_from_transparent(subs, &self.transparent_data);
        let (transparent_ids, transparent_revocations, compacts_revocations) =
            self.select_limit_reduction_targets(subs, revocations);

        let revoked_ids = take_revoked_transparent_subs(
            self.websocket.as_mut(),
            &mut self.transparent_data,
            transparent_ids,
            transparent_revocations,
        );
        let downgrade = self.plan_limit_downgrade(subs, revoked_ids, compacts_revocations);
        self.execute_limit_downgrade_compaction(subs, downgrade);
    }

    /// Selects exact transparent victims and compaction revocations for a relay
    /// limit decrease by choosing the least disruptive next target each time.
    fn select_limit_reduction_targets(
        &self,
        subs: &OutboxSubscriptions,
        revocations: Vec<crate::relay::SubPassRevocation>,
    ) -> (
        Vec<OutboxSubId>,
        Vec<crate::relay::SubPassRevocation>,
        Vec<crate::relay::SubPassRevocation>,
    ) {
        let mut transparent_candidates = self
            .transparent_routing
            .limit_reduction_candidates()
            .into_iter()
            .filter_map(|id| {
                let preference = subs.routing_preference(&id)?;
                Some(LimitReductionTarget::Transparent { id, preference })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        let mut compaction_costs = self
            .compaction_data
            .downgrade_revocation_costs(subs)
            .into_iter()
            .peekable();

        let mut transparent_ids = Vec::new();
        let mut transparent_revocations = Vec::new();
        let mut compaction_revocations = Vec::new();

        for revocation in revocations {
            match Self::next_limit_reduction_target(
                transparent_candidates.peek().copied(),
                compaction_costs.peek().copied(),
            ) {
                Some(LimitReductionTarget::Transparent { id, .. }) => {
                    transparent_candidates.next();
                    transparent_ids.push(id);
                    transparent_revocations.push(revocation);
                }
                Some(LimitReductionTarget::Compaction) => {
                    compaction_costs.next();
                    compaction_revocations.push(revocation);
                }
                None => {
                    debug_assert!(
                        false,
                        "limit decrease requested more revocations than active relay passes"
                    );
                    tracing::error!(
                        "limit decrease requested more revocations than active relay passes"
                    );
                    compaction_revocations.push(revocation);
                }
            }
        }

        (
            transparent_ids,
            transparent_revocations,
            compaction_revocations,
        )
    }

    /// Chooses the next least-disruptive limit-reduction target between the
    /// dedicated and compaction engines.
    fn next_limit_reduction_target(
        transparent: Option<LimitReductionTarget>,
        compaction_cost: Option<usize>,
    ) -> Option<LimitReductionTarget> {
        let Some(transparent) = transparent else {
            return compaction_cost.map(|_| LimitReductionTarget::Compaction);
        };

        let Some(compaction_cost) = compaction_cost else {
            return Some(transparent);
        };

        match transparent {
            LimitReductionTarget::Transparent {
                preference: RelayRoutingPreference::NoPreference,
                ..
            } => Some(transparent),
            LimitReductionTarget::Transparent { .. } => {
                let _ = compaction_cost;
                Some(LimitReductionTarget::Compaction)
            }
            LimitReductionTarget::Compaction => unreachable!(),
        }
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
                .extend(compaction.ingest_session(fallback_compaction));
        }
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
            .ingest_session(clear_session),
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
            .ingest_session(rebuild_session),
        );
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

        let mut eose_delta = IngestExecutor::new(self, subs).execute(plan);
        eose_delta.invalidated_sub_ids = self.drain_tracker_invalidations();
        IngestSessionResult {
            eose_delta,
            has_pending_eose: !self.eose_queue.is_empty(),
        }
    }

    /// Attempts dedicated placement during the first probe pass without
    /// demotion or compaction fallback.
    fn probe_transparent_request(
        &mut self,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> ProbeTransparentRouteOutcome {
        let Some(view) = subs.view(&id) else {
            return ProbeTransparentRouteOutcome::Skipped;
        };

        let placed = {
            let mut transparent = TransparentRelay::new(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                &mut self.limits.sub_guardian,
            );
            transparent.try_subscribe(view)
        };

        match placed {
            TransparentPlaceResult::Placed => {
                self.set_transparent_route(subs, id);
                ProbeTransparentRouteOutcome::Placed
            }
            TransparentPlaceResult::NoRoom => ProbeTransparentRouteOutcome::NeedsCapacity,
        }
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
        let Some(view) = subs.view(&id) else {
            return FallbackTransparentRouteOutcome::Skipped;
        };
        let policy = subs.routing_preference(&id).unwrap_or_default();

        let placed = {
            let mut transparent = TransparentRelay::new(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                &mut self.limits.sub_guardian,
            );
            transparent.try_subscribe(view)
        };

        match placed {
            TransparentPlaceResult::Placed => {
                self.set_transparent_route(subs, id);
                FallbackTransparentRouteOutcome::Placed
            }
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

                let Some(retry_view) = subs.view(&id) else {
                    self.clear_route(id);
                    return FallbackTransparentRouteOutcome::Skipped;
                };
                let placed_after_demotion = {
                    let mut transparent = TransparentRelay::new(
                        self.websocket.as_mut(),
                        &mut self.transparent_data,
                        &mut self.limits.sub_guardian,
                    );
                    transparent.try_subscribe(retry_view)
                };

                if matches!(placed_after_demotion, TransparentPlaceResult::Placed) {
                    self.set_transparent_route(subs, id);
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
            let Some(view) = subs.view(&id) else {
                self.clear_route(id);
                continue;
            };

            let placed = {
                let mut transparent = TransparentRelay::new(
                    self.websocket.as_mut(),
                    &mut self.transparent_data,
                    &mut self.limits.sub_guardian,
                );
                transparent.try_subscribe(view)
            };

            match placed {
                TransparentPlaceResult::Placed => self.set_transparent_route(subs, id),
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
            .ingest_session(restore_compaction),
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
    pub(crate) fn drain_tracker_invalidations(&mut self) -> HashSet<OutboxSubId> {
        std::mem::take(&mut self.pending_tracker_invalidations)
    }

    pub fn req_status(&self, id: &OutboxSubId) -> Option<RelayReqStatus> {
        match self.coordination.get(id)? {
            RelayType::Compaction => self.compaction_data.req_status(id),
            RelayType::Transparent => self.transparent_data.req_status(id),
        }
    }

    #[cfg(test)]
    pub(crate) fn effective_route(&self, id: &OutboxSubId) -> Option<RelayType> {
        self.coordination.get(id).copied()
    }

    fn url(&self) -> &str {
        let Some(websocket) = self.websocket.as_ref() else {
            return "";
        };
        websocket.conn.url.as_str()
    }

    // whether we received
    #[profiling::function]
    pub(crate) fn try_recv<F>(&mut self, subs: &OutboxSubscriptions, act: &mut F) -> RecvResponse
    where
        for<'a> F: FnMut(RawEventData<'a>),
    {
        let Some(websocket) = self.websocket.as_mut() else {
            return RecvResponse::default();
        };

        let event = {
            profiling::scope!("webscket try_recv");

            let Some(event) = websocket.conn.receiver.try_recv() else {
                return RecvResponse::default();
            };
            event
        };

        let msg = match &event {
            WsEvent::Opened => {
                websocket.conn.set_status(RelayStatus::Connected);
                websocket.reconnect_attempt = 0;
                websocket.retry_connect_after = WebsocketRelay::initial_reconnect_duration();
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
                websocket.conn.set_status(RelayStatus::Disconnected);
                None
            }
            WsEvent::Error(err) => {
                tracing::error!("relay {} error: {:?}", websocket.conn.url, err);
                websocket.conn.set_status(RelayStatus::Disconnected);
                None
            }
            WsEvent::Message(ws_message) => match ws_message {
                #[cfg(not(target_arch = "wasm32"))]
                WsMessage::Ping(bs) => {
                    websocket.conn.sender.send(WsMessage::Pong(bs.clone()));
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
            },
        };

        let mut resp = RecvResponse::received();
        let Some(msg) = msg else {
            return resp;
        };

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
                resp.event_was_nostr_note = true;
                act(RawEventData {
                    url: websocket.conn.url.as_str(),
                    event_json: ev,
                    relay_type: RelayImplType::Websocket,
                });
            }
            RelayMessage::Notice(msg) => {
                tracing::warn!("Notice from {}: {}", self.url(), msg)
            }
            RelayMessage::Closed(sid, _) => {
                tracing::trace!("Relay {} received CLOSED: {sid}", self.url());
                self.compaction_data
                    .set_req_status(sid, RelayReqStatus::Closed);
                self.transparent_data
                    .set_req_status(sid, RelayReqStatus::Closed);
            }
        }

        resp
    }
}

#[derive(Default)]
/// Non-blocking receive outcome for one `CoordinationData::try_recv` poll.
pub struct RecvResponse {
    /// At least one websocket event was consumed.
    pub received_event: bool,
    /// A consumed event was a Nostr note payload.
    pub event_was_nostr_note: bool,
    /// One or more relay EOSE markers were queued for ingest-time processing.
    pub eose_enqueued: bool,
}

impl RecvResponse {
    pub fn received() -> Self {
        RecvResponse {
            received_event: true,
            event_was_nostr_note: false,
            eose_enqueued: false,
        }
    }
}

/// Result returned after coordinator ingestion for one relay.
pub struct IngestSessionResult {
    pub eose_delta: RelayEoseDelta,
    pub has_pending_eose: bool,
}

#[derive(Default)]
pub struct RelayEoseDelta {
    pub sub_ids: HashSet<OutboxSubId>,
    pub invalidated_sub_ids: HashSet<OutboxSubId>,
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
    use crate::relay::test_utils::{insert_sub_with_policy, MockWakeup};

    /// Returns the task held for `id`, panicking when no matching task exists.
    #[track_caller]
    fn expect_task(session: &CoordinationSession, id: OutboxSubId) -> &CoordinationTask {
        session
            .tasks
            .get(&id)
            .unwrap_or_else(|| panic!("Expected task for {:?}", id))
    }

    // ==================== CoordinationSession tests ====================

    /// Newly created coordination sessions hold no tasks.
    #[test]
    fn coordination_session_default_empty() {
        let session = CoordinationSession::default();
        assert!(session.tasks.is_empty());
    }

    /// No-preference dedicated subscriptions should map to no-preference dedicated mode.
    #[test]
    fn coordination_session_subscribe_no_preference_dedicated() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::NoPreference);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::NoPreference)
        ));
    }

    /// Prefer-dedicated subscriptions should map to dedicated-preferred mode.
    #[test]
    fn coordination_session_subscribe_preferred_dedicated() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::PreferDedicated);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
    }

    /// Required-dedicated subscriptions should be recorded as required tasks.
    #[test]
    fn coordination_session_subscribe_required_dedicated() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::RequireDedicated);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::Subscribe(RelayRoutingPreference::RequireDedicated)
        ));
    }

    /// Unsubscribe should record an Unsubscribe task.
    #[test]
    fn coordination_session_unsubscribe() {
        let mut session = CoordinationSession::default();

        session.unsubscribe(OutboxSubId(42));

        assert!(matches!(
            expect_task(&session, OutboxSubId(42)),
            CoordinationTask::Unsubscribe
        ));
    }

    /// Subsequent subscribe calls should overwrite previous modes.
    #[test]
    fn coordination_session_subscribe_overwrites_previous() {
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

    /// Unsubscribe should override any prior subscribe entries.
    #[test]
    fn coordination_session_unsubscribe_overwrites_subscribe() {
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
    #[test]
    fn coordination_session_multiple_tasks() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), RelayRoutingPreference::PreferDedicated);
        session.subscribe(OutboxSubId(1), RelayRoutingPreference::RequireDedicated);
        session.unsubscribe(OutboxSubId(2));

        assert_eq!(session.tasks.len(), 3);
    }

    // ==================== RelayEoseDelta tests ====================

    #[test]
    fn relay_eose_delta_default_empty() {
        let delta = RelayEoseDelta::default();
        assert!(delta.sub_ids.is_empty());
        assert!(delta.invalidated_sub_ids.is_empty());
    }

    fn coordinator_with_limit(maximum_subs: usize) -> CoordinationData {
        CoordinationData::new(
            RelayLimitations {
                maximum_subs,
                max_json_bytes: 400_000,
            },
            NormRelayUrl::new("wss://relay-coordinator-test.example.com").unwrap(),
            MockWakeup::default(),
        )
    }

    fn seed_transparent_route(
        coordinator: &mut CoordinationData,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) {
        {
            let mut transparent = TransparentRelay::new(
                None,
                &mut coordinator.transparent_data,
                &mut coordinator.limits.sub_guardian,
            );
            assert!(matches!(
                transparent.try_subscribe(subs.view(&id).unwrap()),
                TransparentPlaceResult::Placed
            ));
        }
        coordinator.set_transparent_route(subs, id);
    }

    fn seed_compaction_route(
        coordinator: &mut CoordinationData,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) {
        {
            let mut compaction = CompactionRelay::new(
                None,
                &mut coordinator.compaction_data,
                coordinator.limits.max_json_bytes,
                &mut coordinator.limits.sub_guardian,
                subs,
            );
            assert!(matches!(
                compaction.subscribe(id),
                crate::relay::compaction::PlaceResult::Placed
            ));
        }
        coordinator.set_compaction_route(subs, id);
    }

    #[test]
    fn preferred_transparent_demotes_non_preferred_and_takes_freed_slot() {
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
            coordinator.effective_route(&id_default),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.effective_route(&id_preferred),
            Some(RelayType::Transparent)
        );

        let mut second = CoordinationSession::default();
        second.subscribe(id_incoming, RelayRoutingPreference::PreferDedicated);
        coordinator.ingest_session(&subs, second);

        assert_eq!(
            coordinator.effective_route(&id_default),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.effective_route(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.effective_route(&id_incoming),
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

    #[test]
    fn preferred_transparent_does_not_demote_existing_preferred() {
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

        assert_eq!(
            coordinator.effective_route(&id_a),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.effective_route(&id_b),
            Some(RelayType::Compaction)
        );
        assert!(coordinator.transparent_data.contains(&id_a));
        assert!(!coordinator.transparent_data.contains(&id_b));
        assert!(coordinator.compaction_data.req_status(&id_a).is_none());
        assert!(!coordinator.transparent_data.contains(&id_b));
    }

    #[test]
    fn required_transparent_does_not_fallback_to_compaction_when_full() {
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

        assert_eq!(
            coordinator.effective_route(&id_a),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.effective_route(&id_b),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_a));
        assert!(!coordinator.transparent_data.contains(&id_b));
        assert!(coordinator.compaction_data.req_status(&id_b).is_none());

        let mut third = CoordinationSession::default();
        third.unsubscribe(id_a);
        coordinator.ingest_session(&subs, third);

        assert_eq!(coordinator.effective_route(&id_a), None);
        assert_eq!(
            coordinator.effective_route(&id_b),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_b));
    }

    #[test]
    fn required_transparent_can_demote_non_preferred_and_take_slot() {
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
            coordinator.effective_route(&id_default),
            Some(RelayType::Compaction)
        );
        assert_eq!(
            coordinator.effective_route(&id_required),
            Some(RelayType::Transparent)
        );
        assert!(!coordinator.transparent_data.contains(&id_default));
        assert!(coordinator.transparent_data.contains(&id_required));
    }

    #[test]
    fn fallback_to_compaction_clears_stale_transparent_queue_entry() {
        let mut subs = OutboxSubscriptions::default();
        let id_existing = OutboxSubId(40);
        let id_incoming = OutboxSubId(41);
        insert_sub_with_policy(&mut subs, id_existing, RelayRoutingPreference::NoPreference);
        insert_sub_with_policy(&mut subs, id_incoming, RelayRoutingPreference::NoPreference);

        let mut coordinator = coordinator_with_limit(1);

        let mut seed = CoordinationSession::default();
        seed.subscribe(id_existing, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, seed);

        coordinator
            .transparent_data
            .queue_subscribe_for_test(id_incoming);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);

        let mut second = CoordinationSession::default();
        second.subscribe(id_incoming, RelayRoutingPreference::NoPreference);
        coordinator.ingest_session(&subs, second);

        assert_eq!(
            coordinator.effective_route(&id_incoming),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_incoming));
        assert_eq!(
            coordinator.transparent_data.queued_len_for_test(),
            0,
            "fallback to compaction should cancel stale transparent retries"
        );
    }

    #[test]
    fn limit_downgrade_prefers_compaction_revoke_over_preferred_transparent() {
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

        let mut coordinator = coordinator_with_limit(3);
        seed_transparent_route(&mut coordinator, &subs, id_a);
        seed_transparent_route(&mut coordinator, &subs, id_b);
        seed_compaction_route(&mut coordinator, &subs, id_compaction);

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
            [
                coordinator.effective_route(&id_a),
                coordinator.effective_route(&id_b)
            ]
            .into_iter()
            .filter(|route| *route == Some(RelayType::Compaction))
            .count(),
            0
        );
        assert_eq!(
            coordinator.effective_route(&id_compaction),
            Some(RelayType::Compaction)
        );
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
        assert_eq!(coordinator.compaction_data.num_subs(), 0);
    }

    #[test]
    fn limit_downgrade_prefers_compaction_revoke_over_required_transparent() {
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

        let mut coordinator = coordinator_with_limit(3);
        seed_transparent_route(&mut coordinator, &subs, id_a);
        seed_transparent_route(&mut coordinator, &subs, id_b);
        seed_compaction_route(&mut coordinator, &subs, id_compaction);

        coordinator.set_max_size(&subs, 2);

        assert_eq!(
            [
                coordinator.effective_route(&id_a),
                coordinator.effective_route(&id_b)
            ]
            .into_iter()
            .filter(|route| *route == Some(RelayType::Transparent))
            .count(),
            2
        );
        assert_eq!(coordinator.compaction_data.num_subs(), 0);
        assert_eq!(coordinator.transparent_data.num_subs(), 2);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[test]
    fn limit_downgrade_prefers_no_preference_transparent_over_required() {
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

        let mut coordinator = coordinator_with_limit(3);
        seed_transparent_route(&mut coordinator, &subs, id_no_preference);
        seed_transparent_route(&mut coordinator, &subs, id_required);
        seed_compaction_route(&mut coordinator, &subs, id_compaction);

        coordinator.set_max_size(&subs, 2);

        assert_eq!(
            coordinator.effective_route(&id_required),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_required));
        assert_eq!(
            coordinator.effective_route(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_no_preference));
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 0);
    }

    #[test]
    fn limit_downgrade_requeues_required_when_no_lower_cost_victim_exists() {
        let mut subs = OutboxSubscriptions::default();
        let id_a = OutboxSubId(66);
        let id_b = OutboxSubId(67);
        insert_sub_with_policy(&mut subs, id_a, RelayRoutingPreference::RequireDedicated);
        insert_sub_with_policy(&mut subs, id_b, RelayRoutingPreference::RequireDedicated);

        let mut coordinator = coordinator_with_limit(2);
        seed_transparent_route(&mut coordinator, &subs, id_a);
        seed_transparent_route(&mut coordinator, &subs, id_b);

        coordinator.set_max_size(&subs, 1);

        assert_eq!(
            [
                coordinator.effective_route(&id_a),
                coordinator.effective_route(&id_b)
            ]
            .into_iter()
            .filter(|route| *route == Some(RelayType::Transparent))
            .count(),
            2
        );
        assert_eq!(coordinator.transparent_data.num_subs(), 1);
        assert_eq!(coordinator.transparent_data.queued_len_for_test(), 1);
    }

    #[test]
    fn preferred_compaction_route_promotes_when_dedicated_slot_opens() {
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

        let mut coordinator = coordinator_with_limit(2);
        seed_transparent_route(&mut coordinator, &subs, id_transparent);
        seed_compaction_route(&mut coordinator, &subs, id_preferred);

        let mut session = CoordinationSession::default();
        session.unsubscribe(id_transparent);
        coordinator.ingest_session(&subs, session);

        assert_eq!(coordinator.effective_route(&id_transparent), None);
        assert_eq!(
            coordinator.effective_route(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_preferred));
        assert!(coordinator
            .compaction_data
            .req_status(&id_preferred)
            .is_none());
    }

    #[test]
    fn no_preference_compaction_route_does_not_promote_when_dedicated_slot_opens() {
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

        let mut coordinator = coordinator_with_limit(2);
        seed_transparent_route(&mut coordinator, &subs, id_transparent);
        seed_compaction_route(&mut coordinator, &subs, id_no_preference);

        let mut session = CoordinationSession::default();
        session.unsubscribe(id_transparent);
        coordinator.ingest_session(&subs, session);

        assert_eq!(coordinator.effective_route(&id_transparent), None);
        assert_eq!(
            coordinator.effective_route(&id_no_preference),
            Some(RelayType::Compaction)
        );
        assert!(!coordinator.transparent_data.contains(&id_no_preference));
        assert!(coordinator
            .compaction_data
            .req_status(&id_no_preference)
            .is_some());
    }

    #[test]
    fn preferred_compaction_route_promotes_on_limit_increase() {
        let mut subs = OutboxSubscriptions::default();
        let id_preferred = OutboxSubId(90);
        insert_sub_with_policy(
            &mut subs,
            id_preferred,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut coordinator = coordinator_with_limit(2);
        seed_compaction_route(&mut coordinator, &subs, id_preferred);
        assert_eq!(
            coordinator.effective_route(&id_preferred),
            Some(RelayType::Compaction)
        );

        coordinator.set_max_size(&subs, 3);

        assert_eq!(
            coordinator.effective_route(&id_preferred),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.transparent_data.contains(&id_preferred));
        assert!(coordinator
            .compaction_data
            .req_status(&id_preferred)
            .is_none());
    }
}
