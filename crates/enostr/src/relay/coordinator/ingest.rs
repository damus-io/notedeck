use hashbrown::{HashMap, HashSet};

use crate::relay::{
    compaction::{CompactionData, CompactionRelay, CompactionSession},
    transparent::{TransparentData, TransparentRelay},
    OutboxSubId, OutboxSubscriptions, RelayReqId, RelayRoutingPreference, RelayType,
};

use super::{
    CoordinationData, CoordinationSession, CoordinationTask, FallbackTransparentRouteOutcome,
    ProbeTransparentRouteOutcome, RelayEoseDelta,
};

/// One-frame coordinator plan produced before side effects run.
#[derive(Default)]
pub(super) struct IngestPlan {
    route_ops: Vec<RouteOp>,
    transparent_unsub_ids: HashSet<OutboxSubId>,
    transparent_sub_requests: Vec<OutboxSubId>,
    compaction_session: CompactionSession,
    eose_delta: RelayEoseDelta,
}

/// One-time route mutation to apply before executing relay-engine actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum RouteOp {
    Remove(OutboxSubId),
}

/// Builds a pure route/action plan from coordinator snapshots and session intents.
pub(super) struct IngestPlanner<'a> {
    routes: &'a HashMap<OutboxSubId, RelayType>,
    compaction: &'a CompactionData,
    transparent: &'a TransparentData,
    pending_eose: Vec<RelayReqId>,
}

impl<'a> IngestPlanner<'a> {
    pub(super) fn new(
        routes: &'a HashMap<OutboxSubId, RelayType>,
        compaction: &'a CompactionData,
        transparent: &'a TransparentData,
        pending_eose: Vec<RelayReqId>,
    ) -> Self {
        Self {
            routes,
            compaction,
            transparent,
            pending_eose,
        }
    }

    /// Builds a complete ingest plan for one coordinator session.
    pub(super) fn plan(mut self, session: CoordinationSession) -> IngestPlan {
        let mut plan = IngestPlan::default();
        let mut required_transparent_requests = Vec::new();
        let mut preferred_transparent_requests = Vec::new();
        let mut no_preference_transparent_requests = Vec::new();

        self.stage_session_tasks(
            session,
            &mut plan,
            &mut required_transparent_requests,
            &mut preferred_transparent_requests,
            &mut no_preference_transparent_requests,
        );

        self.stage_pending_eose(&mut plan);
        // Require and preferred requests are attempted before no-preference requests.
        plan.transparent_sub_requests = required_transparent_requests;
        plan.transparent_sub_requests
            .extend(preferred_transparent_requests);
        plan.transparent_sub_requests
            .extend(no_preference_transparent_requests);
        plan
    }

    fn stage_session_tasks(
        &self,
        session: CoordinationSession,
        plan: &mut IngestPlan,
        required_transparent_requests: &mut Vec<OutboxSubId>,
        preferred_transparent_requests: &mut Vec<OutboxSubId>,
        no_preference_transparent_requests: &mut Vec<OutboxSubId>,
    ) {
        // Session tasks are translated into pure plan operations only.
        for (id, task) in session.tasks {
            match task {
                CoordinationTask::Subscribe(routing_preference) => match routing_preference {
                    RelayRoutingPreference::NoPreference => {
                        if self.routes.get(&id) == Some(&RelayType::Compaction) {
                            plan.compaction_session.unsub(id);
                        }
                        no_preference_transparent_requests.push(id);
                    }
                    RelayRoutingPreference::PreferDedicated => {
                        if self.routes.get(&id) == Some(&RelayType::Compaction) {
                            plan.compaction_session.unsub(id);
                        }
                        preferred_transparent_requests.push(id);
                    }
                    RelayRoutingPreference::RequireDedicated => {
                        if self.routes.get(&id) == Some(&RelayType::Compaction) {
                            plan.compaction_session.unsub(id);
                        }
                        required_transparent_requests.push(id);
                    }
                },
                CoordinationTask::Unsubscribe => {
                    let Some(current_route) = self.routes.get(&id).copied() else {
                        continue;
                    };

                    match current_route {
                        RelayType::Compaction => {
                            plan.compaction_session.unsub(id);
                        }
                        RelayType::Transparent => {
                            plan.transparent_unsub_ids.insert(id);
                        }
                    }
                    plan.route_ops.push(RouteOp::Remove(id));
                }
            }
        }
    }

    fn stage_pending_eose(&mut self, plan: &mut IngestPlan) {
        // EOSE handling is planned here so executor can apply all effects in one ordered pass.
        for sid in self.pending_eose.drain(..) {
            // Compaction can multiplex many outbox subs into one sid, so resolve it first.
            let Some(compaction_sub_ids) = self.compaction.ids(&sid) else {
                let Some(transparent_id) = self.transparent.id(&sid) else {
                    continue;
                };

                plan.eose_delta.sub_ids.insert(transparent_id);
                continue;
            };

            for id in compaction_sub_ids {
                plan.eose_delta.sub_ids.insert(*id);
            }
        }
    }
}

/// Applies a precomputed ingest plan to relay engines and coordinator state.
pub(super) struct IngestExecutor<'a> {
    coordinator: &'a mut CoordinationData,
    subs: &'a OutboxSubscriptions,
}

impl<'a> IngestExecutor<'a> {
    pub(super) fn new(
        coordinator: &'a mut CoordinationData,
        subs: &'a OutboxSubscriptions,
    ) -> Self {
        Self { coordinator, subs }
    }

    /// Applies the plan in a strict order to keep route and relay state coherent.
    pub(super) fn execute(mut self, mut plan: IngestPlan) -> RelayEoseDelta {
        // Execution order is policy-sensitive: route ownership first, engine side effects second.
        self.apply_route_ops(&plan.route_ops);
        self.execute_transparent_unsubscribes(std::mem::take(&mut plan.transparent_unsub_ids));
        self.execute_compaction_session(std::mem::take(&mut plan.compaction_session));
        self.route_transparent_requests(std::mem::take(&mut plan.transparent_sub_requests));
        self.flush_transparent_queue();
        self.coordinator
            .promote_preferred_compaction_routes(self.subs);
        self.drain_compaction_queue();
        self.log_sub_pass_usage();
        plan.eose_delta
    }

    fn apply_route_ops(&mut self, route_ops: &[RouteOp]) {
        for route_op in route_ops {
            match route_op {
                RouteOp::Remove(id) => {
                    self.coordinator.clear_route(*id);
                }
            }
        }
    }

    fn execute_transparent_unsubscribes(&mut self, transparent_unsub_ids: HashSet<OutboxSubId>) {
        if transparent_unsub_ids.is_empty() {
            return;
        }

        let unsubscribed = {
            let mut transparent = TransparentRelay::new(
                self.coordinator.websocket.as_mut(),
                &mut self.coordinator.transparent_data,
                &mut self.coordinator.limits.sub_guardian,
            );
            let mut unsubscribed = Vec::with_capacity(transparent_unsub_ids.len());
            for unsub_id in transparent_unsub_ids {
                transparent.unsubscribe(unsub_id);
                unsubscribed.push(unsub_id);
            }
            unsubscribed
        };

        for unsub_id in unsubscribed {
            self.coordinator
                .transparent_routing
                .note_transparent_unsubscribe(unsub_id);
        }
    }

    fn execute_compaction_session(&mut self, session: CompactionSession) {
        if session.is_empty() {
            return;
        }

        self.coordinator.pending_tracker_invalidations.extend(
            CompactionRelay::new(
                self.coordinator.websocket.as_mut(),
                &mut self.coordinator.compaction_data,
                self.coordinator.limits.max_json_bytes,
                &mut self.coordinator.limits.sub_guardian,
                self.subs,
            )
            .ingest_session_without_queue_drain(session),
        );
    }

    fn drain_compaction_queue(&mut self) {
        if !self.coordinator.compaction_data.has_queued_subs() {
            return;
        }

        self.coordinator.pending_tracker_invalidations.extend(
            CompactionRelay::new(
                self.coordinator.websocket.as_mut(),
                &mut self.coordinator.compaction_data,
                self.coordinator.limits.max_json_bytes,
                &mut self.coordinator.limits.sub_guardian,
                self.subs,
            )
            .drain_queue(),
        );
    }

    fn route_transparent_requests(&mut self, transparent_requests: Vec<OutboxSubId>) {
        let requested = transparent_requests.len();
        let available_before = self.coordinator.limits.sub_guardian.available_passes();
        let mut needs_capacity_ids = Vec::new();
        let mut fallback_compaction_session = CompactionSession::default();
        let mut demoted_in_this_pass = HashSet::new();
        let mut placed_count = 0usize;
        let mut fallback_count = 0usize;
        let mut queued_count = 0usize;
        let mut needs_capacity_count = 0usize;
        let mut skipped_count = 0usize;

        for id in transparent_requests {
            let outcome = self.coordinator.probe_transparent_request(self.subs, id);
            match outcome {
                ProbeTransparentRouteOutcome::Placed => placed_count += 1,
                ProbeTransparentRouteOutcome::NeedsCapacity => {
                    needs_capacity_count += 1;
                    needs_capacity_ids.push(id);
                }
                ProbeTransparentRouteOutcome::Skipped => skipped_count += 1,
            }
        }

        if !needs_capacity_ids.is_empty() {
            let mut reserve_session = CompactionSession::default();
            reserve_session.request_free_subs(needs_capacity_ids.len());
            self.execute_compaction_session(reserve_session);
            self.coordinator
                .promote_preferred_compaction_routes(self.subs);
        }

        for id in needs_capacity_ids {
            let outcome = self.coordinator.route_transparent_request_with_fallback(
                self.subs,
                &mut fallback_compaction_session,
                &mut demoted_in_this_pass,
                id,
            );
            match outcome {
                FallbackTransparentRouteOutcome::Placed => placed_count += 1,
                FallbackTransparentRouteOutcome::Fallback => fallback_count += 1,
                FallbackTransparentRouteOutcome::Queued => queued_count += 1,
                FallbackTransparentRouteOutcome::Skipped => skipped_count += 1,
            }
        }

        let demotion_count = demoted_in_this_pass.len();
        self.execute_compaction_session(fallback_compaction_session);

        tracing::trace!(
            requested,
            placed_count,
            needs_capacity_count,
            fallback_count,
            queued_count,
            skipped_count,
            demotion_count,
            available_before,
            available_after = self.coordinator.limits.sub_guardian.available_passes(),
            "transparent routing pass complete"
        );
    }

    fn flush_transparent_queue(&mut self) {
        self.coordinator.flush_transparent_queue(self.subs);
    }

    fn log_sub_pass_usage(&self) {
        tracing::trace!(
            "Using {} of {} subs",
            self.coordinator.limits.sub_guardian.total_passes()
                - self.coordinator.limits.sub_guardian.available_passes(),
            self.coordinator.limits.sub_guardian.total_passes()
        );
    }
}

#[cfg(test)]
mod tests {
    use hashbrown::{HashMap, HashSet};

    use crate::relay::{test_utils::insert_sub_with_policy, RelayRoutingPreference};

    use super::*;

    /// Required and preferred transparent requests are staged ahead of no-preference requests.
    #[test]
    fn planner_orders_required_then_preferred_then_no_preference() {
        let id_required = OutboxSubId(1);
        let id_preferred_1 = OutboxSubId(2);
        let id_default = OutboxSubId(3);
        let id_preferred_2 = OutboxSubId(4);

        let mut subs = OutboxSubscriptions::default();
        insert_sub_with_policy(
            &mut subs,
            id_required,
            RelayRoutingPreference::RequireDedicated,
        );
        insert_sub_with_policy(
            &mut subs,
            id_preferred_1,
            RelayRoutingPreference::PreferDedicated,
        );
        insert_sub_with_policy(&mut subs, id_default, RelayRoutingPreference::NoPreference);
        insert_sub_with_policy(
            &mut subs,
            id_preferred_2,
            RelayRoutingPreference::PreferDedicated,
        );

        let mut session = CoordinationSession::default();
        session.subscribe(id_default, RelayRoutingPreference::NoPreference);
        session.subscribe(id_preferred_1, RelayRoutingPreference::PreferDedicated);
        session.subscribe(id_required, RelayRoutingPreference::RequireDedicated);
        session.subscribe(id_preferred_2, RelayRoutingPreference::PreferDedicated);

        let plan = IngestPlanner::new(
            &HashMap::new(),
            &CompactionData::default(),
            &TransparentData::default(),
            Vec::new(),
        )
        .plan(session);

        let mut preferred = HashSet::new();
        preferred.insert(id_preferred_1);
        preferred.insert(id_preferred_2);
        let mut seen_no_preference = false;
        let mut seen_required = false;

        for id in plan.transparent_sub_requests {
            if id == id_required {
                assert!(!seen_required, "required request appeared more than once");
                assert!(
                    !seen_no_preference,
                    "required request appeared after no-preference"
                );
                seen_required = true;
                continue;
            }
            if preferred.contains(&id) {
                assert!(
                    !seen_no_preference,
                    "preferred request appeared after no-preference"
                );
                continue;
            }
            seen_no_preference = true;
        }
    }

    /// Planner stage should emit cleanup work from current routes without mutating snapshots.
    #[test]
    fn planner_emits_cleanup_from_current_routes() {
        let id_transparent = OutboxSubId(11);
        let id_compaction = OutboxSubId(12);

        let mut routes = HashMap::new();
        routes.insert(id_transparent, RelayType::Transparent);
        routes.insert(id_compaction, RelayType::Compaction);

        let mut session = CoordinationSession::default();
        session.unsubscribe(id_transparent);
        session.unsubscribe(id_compaction);

        let plan = IngestPlanner::new(
            &routes,
            &CompactionData::default(),
            &TransparentData::default(),
            Vec::new(),
        )
        .plan(session);

        assert!(plan.transparent_unsub_ids.contains(&id_transparent));
        assert!(matches!(
            plan.compaction_session.task_for_test(&id_compaction),
            Some(crate::relay::RelayTask::Unsubscribe)
        ));
        assert!(plan.route_ops.contains(&RouteOp::Remove(id_transparent)));
        assert!(plan.route_ops.contains(&RouteOp::Remove(id_compaction)));
        assert_eq!(routes.get(&id_transparent), Some(&RelayType::Transparent));
        assert_eq!(routes.get(&id_compaction), Some(&RelayType::Compaction));
    }
}
