use std::collections::{hash_map::Entry, HashMap};

use hashbrown::HashSet;
use nostrdb::Filter;

use crate::{
    relay::{
        frame::QueuedRelayFrame, limits::IndexedFilter, OutboxSubId, OutboxSubscriptions,
        QueuedTasks, RelayReqId, RelayReqStatus, RelayTask, ReqFilterLimits, SubPass,
        SubPassRevocation,
    },
    ClientMessage,
};

/// CompactionData tracks every compaction REQ on a relay along with the
/// Outbox sub ids routed into it.
#[derive(Default)]
pub struct CompactionData {
    request_to_sid: HashMap<OutboxSubId, RelayReqId>, // we never split outbox subs over multiple REQs
    relay_subs: HashMap<RelayReqId, RelaySubData>,    // UUID
    queue: QueuedTasks,
}

impl CompactionData {
    #[cfg(test)]
    pub fn num_subs(&self) -> usize {
        self.relay_subs.len()
    }

    /// Returns the status of the compacted relay request carrying `id`.
    pub fn req_status(&self, id: &OutboxSubId) -> Option<RelayReqStatus> {
        let sid = self.request_to_sid.get(id)?;
        Some(self.relay_subs.get(sid)?.status)
    }

    #[cfg(test)]
    pub fn has_eose(&self, id: &OutboxSubId) -> bool {
        self.req_status(id) == Some(RelayReqStatus::Eose)
    }

    /// Outbox subscription IDs currently placed in compaction requests.
    pub fn request_ids(&self) -> Vec<OutboxSubId> {
        self.request_to_sid.keys().copied().collect()
    }

    /// Applies an EOSE status update to one compacted relay REQ.
    pub(crate) fn apply_eose(&mut self, sid: &RelayReqId) -> CompactionTransition {
        self.apply_req_status(sid, RelayReqStatus::Eose, true)
    }

    /// Applies a CLOSED status update to one compacted relay REQ.
    pub(crate) fn apply_closed(&mut self, sid: &RelayReqId) -> CompactionTransition {
        self.apply_req_status(sid, RelayReqStatus::Closed, false)
    }

    fn apply_req_status(
        &mut self,
        sid: &RelayReqId,
        status: RelayReqStatus,
        is_eose: bool,
    ) -> CompactionTransition {
        let Some(data) = self.relay_subs.get_mut(sid) else {
            return CompactionTransition::default();
        };

        data.status = status;
        let status_changed_sub_ids = data.requests.requests.clone();
        let eose_sub_ids = if is_eose {
            status_changed_sub_ids.clone()
        } else {
            HashSet::new()
        };
        CompactionTransition {
            invalidated_sub_ids: HashSet::new(),
            status_changed_sub_ids,
            eose_sub_ids,
            frames: Vec::new(),
            returned_passes: Vec::new(),
            frame_indexes: HashMap::new(),
        }
    }

    /// Returns true when compaction has queued subscribe work waiting for capacity.
    pub fn has_queued_subs(&self) -> bool {
        !self.queue.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn queued_len_for_test(&self) -> usize {
        self.queue.len()
    }

    /// Returns compaction REQ costs ordered from cheapest to most expensive for
    /// limit-downgrade planning.
    pub(crate) fn downgrade_revocation_costs(&self, subs: &OutboxSubscriptions) -> Vec<usize> {
        let mut costs = self
            .relay_subs
            .values()
            .map(|data| data.json_size(subs))
            .collect::<Vec<_>>();
        costs.sort_unstable();
        costs
    }

    /// Returns whether every `id` can be placed in order with the current
    /// compaction requests plus `available_passes` new relay REQs.
    pub(crate) fn can_place_subscribes_with_passes(
        &self,
        subs: &OutboxSubscriptions,
        ids: impl IntoIterator<Item = OutboxSubId>,
        limits: ReqFilterLimits,
        available_passes: usize,
    ) -> bool {
        let mut subscribe_filter_sets = Vec::new();
        for id in ids {
            let Some(filters) = subs.filters_for_compaction(&id) else {
                return false;
            };
            let Some(request_filters) = compaction_filters_for_single_req(id, filters, limits)
            else {
                return false;
            };
            subscribe_filter_sets.push(request_filters);
        }

        let existing = self
            .relay_subs
            .iter()
            .map(|(relay_id, relay_data)| (relay_id.clone(), relay_data.requests.clone()));
        plan_compaction_placements(
            existing,
            subs,
            subscribe_filter_sets,
            limits,
            available_passes,
        )
        .is_some()
    }

    fn insert_request_sid(&mut self, id: OutboxSubId, sid: RelayReqId) {
        match self.request_to_sid.entry(id) {
            Entry::Occupied(entry) => {
                assert_eq!(
                    entry.get(),
                    &sid,
                    "compaction request id {id:?} must not be placed on multiple relay REQs"
                );
            }
            Entry::Vacant(entry) => {
                entry.insert(sid);
            }
        }
    }

    fn remove_request_sid(&mut self, id: &OutboxSubId, sid: &RelayReqId) {
        let Some(existing_sid) = self.request_to_sid.get(id) else {
            return;
        };

        if existing_sid == sid {
            self.request_to_sid.remove(id);
        }
    }

    fn remove_request_sid_for_id(&mut self, id: &OutboxSubId) -> Option<RelayReqId> {
        self.request_to_sid.remove(id)
    }

    /// Clears all compaction REQ state without sending `CLOSE` frames.
    pub(crate) fn clear_without_closing(&mut self) -> CompactionTransition {
        let mut affected = self.request_to_sid.keys().copied().collect::<HashSet<_>>();
        self.request_to_sid.clear();
        let mut returned_passes = Vec::new();

        for (_, sub_data) in self.relay_subs.drain() {
            affected.extend(sub_data.requests.requests);
            returned_passes.push(sub_data.sub_pass);
        }

        while let Some(id) = self.queue.pop() {
            affected.insert(id);
        }

        CompactionTransition {
            invalidated_sub_ids: affected,
            status_changed_sub_ids: HashSet::new(),
            eose_sub_ids: HashSet::new(),
            frames: Vec::new(),
            returned_passes,
            frame_indexes: HashMap::new(),
        }
    }

    #[profiling::function]
    pub(crate) fn apply_operation_plan(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
        plan: CompactionOperationPlan,
    ) -> CompactionTransition {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .apply_operation_plan(plan)
    }

    /// Applies an explicit compaction plan without applying leftover granted
    /// capacity to queued compaction work afterward.
    #[profiling::function]
    pub(crate) fn apply_operation_plan_without_capacity_application(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
        plan: CompactionOperationPlan,
    ) -> CompactionTransition {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .apply_operation_plan_without_capacity_application(plan)
    }

    /// Applies currently granted capacity to queued compaction work.
    #[profiling::function]
    pub(crate) fn apply_granted_capacity(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
    ) -> CompactionCapacityOutcome {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .apply_granted_capacity()
    }

    /// Rebuilds all active compaction REQs under current limits.
    #[profiling::function]
    pub(crate) fn repack_active_for_current_limits(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
    ) -> CompactionTransition {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .repack_active_for_current_limits()
    }

    #[profiling::function]
    pub(crate) fn unsubscribe(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) -> CompactionTransition {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .unsubscribe(id)
    }

    #[profiling::function]
    pub(crate) fn handle_relay_open(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
    ) -> CompactionReplayOutcome {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .handle_relay_open()
    }

    pub(crate) fn revocate_all(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &OutboxSubscriptions,
        revocations: Vec<SubPassRevocation>,
    ) -> CompactionTransition {
        CompactionTransitionState::new_limited(
            current_generation,
            self,
            limits,
            granted_passes,
            subs,
        )
        .revocate_all(revocations)
    }
}

/// Ensures `max_subs` REQ to the websocket relay by "compacting" subscriptions (combining multiple requests into one)
struct CompactionTransitionState<'a> {
    current_generation: Option<u64>,
    data: &'a mut CompactionData,
    transition: CompactionTransition,
    granted_passes: Vec<SubPass>,
    limits: ReqFilterLimits,
    subs: &'a OutboxSubscriptions,
}

/// Explicit output produced by one compaction transition.
#[derive(Default)]
pub(crate) struct CompactionTransition {
    pub(crate) invalidated_sub_ids: HashSet<OutboxSubId>,
    pub(crate) status_changed_sub_ids: HashSet<OutboxSubId>,
    pub(crate) eose_sub_ids: HashSet<OutboxSubId>,
    pub(crate) frames: Vec<QueuedRelayFrame>,
    pub(crate) returned_passes: Vec<SubPass>,
    frame_indexes: HashMap<RelayReqId, usize>,
}

impl CompactionTransition {
    fn extend(&mut self, next: CompactionTransition) {
        self.invalidated_sub_ids.extend(next.invalidated_sub_ids);
        self.status_changed_sub_ids
            .extend(next.status_changed_sub_ids);
        self.eose_sub_ids.extend(next.eose_sub_ids);
        self.frames.extend(next.frames);
        self.returned_passes.extend(next.returned_passes);
    }

    fn push_coalesced_frame(&mut self, sid: RelayReqId, generation: u64, message: ClientMessage) {
        let frame = (generation, message);
        if let Some(index) = self.frame_indexes.get(&sid).copied() {
            self.frames[index] = frame;
            return;
        }

        let index = self.frames.len();
        self.frame_indexes.insert(sid, index);
        self.frames.push(frame);
    }
}

/// Result of replaying compaction demand after a websocket reopen.
pub(crate) struct CompactionReplayOutcome {
    pub(crate) invalidated_sub_ids: HashSet<OutboxSubId>,
    pub(crate) frames: Vec<QueuedRelayFrame>,
    pub(crate) returned_passes: Vec<SubPass>,
    pub(crate) released_ids: HashSet<OutboxSubId>,
}

/// Result of applying granted capacity to queued compaction work.
pub(crate) struct CompactionCapacityOutcome {
    pub(crate) transition: CompactionTransition,
    pub(crate) still_queued: bool,
    pub(crate) made_progress: bool,
    pub(crate) blocked_reason: Option<CompactionBlockedReason>,
}

/// Stable reason queued compaction work could not continue while applying capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactionBlockedReason {
    NoPlacementProgress,
}

#[derive(Default)]
struct CompactionCapacityState {
    made_progress: bool,
    blocked_reason: Option<CompactionBlockedReason>,
}

/// CompactionTransitionState ensures multiple Outbox subscriptions are packed into as few
/// REQs as possible, respecting per-relay limits.
impl<'a> CompactionTransitionState<'a> {
    fn new_limited(
        current_generation: Option<u64>,
        data: &'a mut CompactionData,
        limits: ReqFilterLimits,
        granted_passes: Vec<SubPass>,
        subs: &'a OutboxSubscriptions,
    ) -> Self {
        Self {
            current_generation,
            data,
            transition: CompactionTransition::default(),
            granted_passes,
            limits,
            subs,
        }
    }

    #[profiling::function]
    pub fn apply_operation_plan(mut self, plan: CompactionOperationPlan) -> CompactionTransition {
        self.execute_operation_plan(plan, true);
        self.finish_transition()
    }

    /// Applies an explicit compaction plan without applying leftover granted
    /// capacity to queued compaction work afterward.
    #[profiling::function]
    pub(crate) fn apply_operation_plan_without_capacity_application(
        mut self,
        plan: CompactionOperationPlan,
    ) -> CompactionTransition {
        self.execute_operation_plan(plan, false);
        self.finish_transition()
    }

    /// Applies currently granted capacity to queued compaction work.
    #[profiling::function]
    pub(crate) fn apply_granted_capacity(self) -> CompactionCapacityOutcome {
        let mut this = self;
        let state = this.place_queued_with_granted_capacity();
        let transition = this.finish_transition();
        CompactionCapacityOutcome {
            transition,
            still_queued: this.data.has_queued_subs(),
            made_progress: state.made_progress,
            blocked_reason: state.blocked_reason,
        }
    }

    /// Rebuilds all active compaction REQs under current limits.
    #[profiling::function]
    pub(crate) fn repack_active_for_current_limits(mut self) -> CompactionTransition {
        let active = self.data.relay_subs.keys().cloned().collect::<Vec<_>>();
        for relay_id in active {
            let Some(request_filters) = self.remove_relay_sub_for_repack(relay_id) else {
                continue;
            };
            self.repack_request_filters(request_filters);
        }
        self.finish_transition()
    }

    fn execute_operation_plan(
        &mut self,
        plan: CompactionOperationPlan,
        apply_granted_capacity: bool,
    ) {
        let request_free = plan.request_free;
        let mut reserved: Vec<SubPass> = Vec::new();

        // Reserve passes - take from guardian or compact to free them
        while reserved.len() < request_free {
            if let Some(pass) = self.granted_passes.pop() {
                reserved.push(pass);
            } else {
                let Some(ejected_pass) = self.compact() else {
                    break;
                };
                reserved.push(ejected_pass);
            }
        }

        self.apply_operation_plan_inner(plan);
        self.repack_unfit_reqs();

        if apply_granted_capacity {
            self.place_queued_with_granted_capacity();
        }

        // Return reserved passes
        for pass in reserved {
            self.transition.returned_passes.push(pass);
        }
    }

    fn place_queued_with_granted_capacity(&mut self) -> CompactionCapacityState {
        profiling::scope!("apply granted compaction capacity");
        let mut state = CompactionCapacityState::default();
        let mut attempted = HashSet::new();
        while let Some(id) = self.data.queue.pop() {
            if !attempted.insert(id) {
                self.data.queue.enqueue(id);
                state.blocked_reason = Some(CompactionBlockedReason::NoPlacementProgress);
                break;
            }

            let before_queued = self.data.has_queued_subs();
            let result = self.subscribe(id);
            if matches!(result, PlaceResult::Placed) || before_queued != self.data.has_queued_subs()
            {
                state.made_progress = true;
            }

            if result.is_queued() && attempted.len() >= self.data.queue.len().saturating_add(1) {
                state.blocked_reason = Some(CompactionBlockedReason::NoPlacementProgress);
            }
        }

        if self.data.has_queued_subs() && !state.made_progress && state.blocked_reason.is_none() {
            state.blocked_reason = Some(CompactionBlockedReason::NoPlacementProgress);
        }

        state
    }

    #[profiling::function]
    fn apply_operation_plan_inner(&mut self, plan: CompactionOperationPlan) {
        let CompactionOperationPlan {
            request_free: _,
            mut tasks,
            subscribe_order,
        } = plan;

        for (id, task) in &tasks {
            match task {
                RelayTask::Unsubscribe => {
                    self.unsubscribe_inner(*id);
                }
                RelayTask::Subscribe => {}
            }
        }

        for id in subscribe_order {
            if tasks.remove(&id) == Some(RelayTask::Subscribe) {
                self.subscribe(id);
            }
        }

        for (id, task) in tasks {
            if task == RelayTask::Subscribe {
                self.subscribe(id);
            }
        }
    }

    #[profiling::function]
    pub fn handle_relay_open(&mut self) -> CompactionReplayOutcome {
        let Some(generation) = self.current_generation else {
            return CompactionReplayOutcome {
                invalidated_sub_ids: HashSet::new(),
                frames: Vec::new(),
                returned_passes: Vec::new(),
                released_ids: HashSet::new(),
            };
        };

        let mut released_ids = HashSet::new();
        released_ids.extend(
            self.retire_unfit_relay_subs(
                UnfitRelaySubDisposition::Release,
                UnfitRelaySubScope::All,
            ),
        );

        let mut invalidated = std::mem::take(&mut self.transition.invalidated_sub_ids);

        let mut frames = Vec::new();
        for (sid, sub_data) in &mut self.data.relay_subs {
            let filters = sub_data.filters_for_compaction(self.subs);
            if are_filters_empty(&filters) || !sub_data.fits(self.subs, self.limits) {
                continue;
            }

            sub_data.status = RelayReqStatus::InitialQuery;
            frames.push((generation, ClientMessage::req(sid.to_string(), filters)));
            invalidated.extend(sub_data.requests.requests.iter().copied());
        }

        CompactionReplayOutcome {
            invalidated_sub_ids: invalidated,
            frames,
            returned_passes: self.take_passes_for_output(),
            released_ids,
        }
    }

    pub fn revocate(&mut self, mut revocation: SubPassRevocation) -> CompactionTransition {
        let Some(pass) = self.compact() else {
            // this shouldn't be possible
            return CompactionTransition::default();
        };

        revocation.revocate(pass);
        self.finish_transition()
    }

    pub fn revocate_all(&mut self, revocations: Vec<SubPassRevocation>) -> CompactionTransition {
        let mut transition = CompactionTransition::default();
        for revocation in revocations {
            transition.extend(self.revocate(revocation));
        }
        transition
    }

    #[profiling::function]
    fn compact(&mut self) -> Option<SubPass> {
        let (removed_relay_id, smallest, request_filters) = {
            let (removed_relay_id, smallest) =
                take_smallest_sub_reqs(self.subs, &mut self.data.relay_subs)?;
            let request_filters = smallest.request_filters(self.subs);

            self.mark_relay_sub_removed(
                removed_relay_id.clone(),
                smallest.requests.requests.iter().copied(),
            );
            for request_id in &smallest.requests.requests {
                self.data.remove_request_sid(request_id, &removed_relay_id);
            }

            (removed_relay_id, smallest, request_filters)
        };

        self.repack_request_filters(request_filters);

        tracing::debug!("Compacted relay request {removed_relay_id:?}");
        Some(smallest.sub_pass)
    }

    #[profiling::function]
    fn new_sub(&mut self, id: OutboxSubId) -> PlaceResult {
        let Some(request_filters) = self.request_filters_for_sub(id) else {
            self.data.queue.enqueue(id);
            return PlaceResult::Queued;
        };

        if !self.granted_passes.is_empty() {
            self.apply_placement(PlannedPlacement::New {
                relay_id: RelayReqId::default(),
                filters: request_filters,
            });
            return PlaceResult::Placed;
        }

        let result = self.place_request_filters(request_filters);
        if result.is_queued() {
            self.data.queue.enqueue(id);
        }
        result
    }

    #[profiling::function]
    fn subscribe(&mut self, id: OutboxSubId) -> PlaceResult {
        let was_active = self.data.request_to_sid.contains_key(&id);
        if was_active {
            self.unsubscribe_inner(id);
        }

        self.new_sub(id)
    }

    #[profiling::function]
    pub fn unsubscribe(mut self, id: OutboxSubId) -> CompactionTransition {
        self.unsubscribe_inner(id);
        self.finish_transition()
    }

    #[profiling::function]
    fn unsubscribe_inner(&mut self, id: OutboxSubId) {
        let Some(relay_id) = self.data.remove_request_sid_for_id(&id) else {
            self.data.queue.cancel(id);
            return;
        };
        self.transition.invalidated_sub_ids.insert(id);

        let Some(data) = self.data.relay_subs.get_mut(&relay_id) else {
            self.data.queue.cancel(id);
            return;
        };

        data.status = RelayReqStatus::InitialQuery;

        if !data.requests.remove(&id) {
            return;
        }

        if !data.requests.is_empty() {
            self.mark_relay_sub_touched(relay_id.clone());
            return;
        }

        let Some(data) = self.data.relay_subs.remove(&relay_id) else {
            return;
        };

        self.granted_passes.push(data.sub_pass);
        tracing::debug!("Unsubed from last internal id in REQ, returning pass");
        self.mark_relay_sub_removed(relay_id, std::iter::once(id));
    }

    fn request_filters_for_sub(&mut self, id: OutboxSubId) -> Option<RequestFilters> {
        let filters = self.subs.filters_for_compaction(&id)?;
        compaction_filters_for_single_req(id, filters, self.limits)
    }

    fn place_request_filters(&mut self, request_filters: RequestFilters) -> PlaceResult {
        let available_passes = self.granted_passes.len();
        let existing = self
            .data
            .relay_subs
            .iter()
            .map(|(relay_id, relay_data)| (relay_id.clone(), relay_data.requests.clone()))
            .collect::<Vec<_>>();

        let Some(placement) = plan_compaction_placement(
            existing,
            self.subs,
            request_filters,
            self.limits,
            available_passes,
        ) else {
            return PlaceResult::Queued;
        };

        self.apply_placement(placement);
        PlaceResult::Placed
    }

    fn apply_placement(&mut self, placement: PlannedPlacement) {
        match placement {
            PlannedPlacement::Existing { relay_id, filters } => {
                let id = filters.id;
                let Some(relay_data) = self.data.relay_subs.get_mut(&relay_id) else {
                    return;
                };

                relay_data.requests.add_filters(filters.id, filters.filters);
                relay_data.status = RelayReqStatus::InitialQuery;
                self.data.insert_request_sid(id, relay_id.clone());
                self.mark_relay_sub_touched(relay_id);
            }
            PlannedPlacement::New { relay_id, filters } => {
                let id = filters.id;
                let new_pass = self
                    .granted_passes
                    .pop()
                    .expect("planned compaction placement requires an available pass");
                let mut requests = SubRequests::default();
                requests.add_filters(filters.id, filters.filters);

                self.data.relay_subs.insert(
                    relay_id.clone(),
                    RelaySubData {
                        requests,
                        status: RelayReqStatus::InitialQuery,
                        sub_pass: new_pass,
                    },
                );
                self.data.insert_request_sid(id, relay_id.clone());
                self.mark_relay_sub_new(relay_id);
                tracing::debug!("Placed {id:?} on a new compacted subscription");
            }
        }
    }

    fn repack_unfit_reqs(&mut self) {
        while let Some(relay_id) = self.next_unfit_req() {
            let Some(request_filters) = self.remove_relay_sub_for_repack(relay_id) else {
                continue;
            };

            self.repack_request_filters(request_filters);
        }
    }

    fn next_unfit_req(&mut self) -> Option<RelayReqId> {
        self.data
            .relay_subs
            .iter()
            .find_map(|(relay_id, relay_data)| {
                (!relay_data.fits(self.subs, self.limits)).then(|| relay_id.clone())
            })
    }

    fn remove_relay_sub_for_repack(&mut self, relay_id: RelayReqId) -> Option<Vec<RequestFilters>> {
        let requests = self.remove_relay_sub(&relay_id)?;
        Some(requests.request_filters(self.subs))
    }

    fn retire_unfit_relay_subs(
        &mut self,
        disposition: UnfitRelaySubDisposition,
        scope: UnfitRelaySubScope,
    ) -> HashSet<OutboxSubId> {
        let unfit = {
            let limits = self.limits;

            let candidates = match scope {
                UnfitRelaySubScope::All => self.data.relay_subs.keys().cloned().collect::<Vec<_>>(),
            };

            candidates
                .into_iter()
                .filter(|relay_id| {
                    self.data
                        .relay_subs
                        .get(relay_id)
                        .is_some_and(|relay_data| !relay_data.fits(self.subs, limits))
                })
                .collect::<Vec<_>>()
        };

        let mut retired = HashSet::new();
        for relay_id in unfit {
            let Some(request_ids) = self.remove_relay_sub_for_retirement(&relay_id) else {
                continue;
            };
            retired.extend(request_ids.iter().copied());

            if disposition == UnfitRelaySubDisposition::Queue {
                for id in &request_ids {
                    self.data.queue.enqueue(*id);
                }
            }
        }

        retired
    }

    fn remove_relay_sub_for_retirement(
        &mut self,
        relay_id: &RelayReqId,
    ) -> Option<HashSet<OutboxSubId>> {
        let requests = self.remove_relay_sub(relay_id)?;
        Some(requests.requests)
    }

    fn remove_relay_sub(&mut self, relay_id: &RelayReqId) -> Option<SubRequests> {
        let relay_data = self.data.relay_subs.remove(relay_id)?;

        self.mark_relay_sub_removed(
            relay_id.clone(),
            relay_data.requests.requests.iter().copied(),
        );
        for request_id in &relay_data.requests.requests {
            self.data.remove_request_sid(request_id, relay_id);
        }

        self.granted_passes.push(relay_data.sub_pass);
        Some(relay_data.requests)
    }

    fn repack_request_filters(&mut self, request_filters: Vec<RequestFilters>) {
        for request_filters in request_filters {
            let Some(projected_filters) = request_filters.projected_filters(self.subs) else {
                self.unsubscribe_inner(request_filters.id);
                self.data.queue.enqueue(request_filters.id);
                continue;
            };
            let Some(request_filters) = compaction_indexed_filters_for_single_req(
                request_filters.id,
                projected_filters
                    .into_iter()
                    .map(|entry| (entry.source_index, entry.filter)),
                self.limits,
            ) else {
                self.unsubscribe_inner(request_filters.id);
                self.data.queue.enqueue(request_filters.id);
                continue;
            };

            let request_filters_id = request_filters.id;
            if self.place_request_filters(request_filters).is_queued() {
                self.unsubscribe_inner(request_filters_id);
                self.data.queue.enqueue(request_filters_id);
            }
        }
    }

    pub(crate) fn finish_transition(&mut self) -> CompactionTransition {
        let mut transition = std::mem::take(&mut self.transition);
        transition
            .returned_passes
            .extend(self.take_granted_passes());
        transition.frame_indexes.clear();
        transition
    }

    fn take_passes_for_output(&mut self) -> Vec<SubPass> {
        let mut returned = std::mem::take(&mut self.transition.returned_passes);
        returned.extend(self.take_granted_passes());
        returned
    }

    fn take_granted_passes(&mut self) -> Vec<SubPass> {
        std::mem::take(&mut self.granted_passes)
    }

    fn mark_relay_sub_new(&mut self, relay_id: RelayReqId) {
        self.emit_relay_sub_req(relay_id);
    }

    fn mark_relay_sub_touched(&mut self, relay_id: RelayReqId) {
        self.emit_relay_sub_req(relay_id);
    }

    fn mark_relay_sub_removed(
        &mut self,
        relay_id: RelayReqId,
        ids: impl IntoIterator<Item = OutboxSubId>,
    ) {
        self.transition.invalidated_sub_ids.extend(ids);
        let Some(generation) = self.current_generation else {
            return;
        };
        self.transition.push_coalesced_frame(
            relay_id.clone(),
            generation,
            ClientMessage::close(relay_id.0),
        );
    }

    fn emit_relay_sub_req(&mut self, relay_id: RelayReqId) {
        let Some((request_ids, filters)) = self.relay_sub_req_frame_data(&relay_id) else {
            return;
        };

        self.transition.invalidated_sub_ids.extend(request_ids);
        let Some(generation) = self.current_generation else {
            return;
        };

        let Some(data) = self.data.relay_subs.get_mut(&relay_id) else {
            return;
        };
        data.status = RelayReqStatus::InitialQuery;
        self.transition.push_coalesced_frame(
            relay_id.clone(),
            generation,
            ClientMessage::req(relay_id.0, filters),
        );
    }

    fn relay_sub_req_frame_data(
        &mut self,
        relay_id: &RelayReqId,
    ) -> Option<(HashSet<OutboxSubId>, Vec<Filter>)> {
        let (request_ids, filters, fits) = {
            let data = self.data.relay_subs.get(relay_id)?;
            (
                data.requests.requests.clone(),
                data.filters_for_compaction(self.subs),
                data.fits(self.subs, self.limits),
            )
        };

        if are_filters_empty(&filters) || !fits {
            let Some(retired_ids) = self.remove_relay_sub_for_retirement(relay_id) else {
                return Some((request_ids, Vec::new()));
            };
            for id in retired_ids {
                self.data.queue.enqueue(id);
            }
            return None;
        }

        Some((request_ids, filters))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PlaceResult {
    Placed,
    Queued,
}

impl PlaceResult {
    fn is_queued(&self) -> bool {
        matches!(self, PlaceResult::Queued)
    }
}

enum PlannedPlacement {
    Existing {
        relay_id: RelayReqId,
        filters: RequestFilters,
    },
    New {
        relay_id: RelayReqId,
        filters: RequestFilters,
    },
}

fn plan_compaction_placement(
    existing: impl IntoIterator<Item = (RelayReqId, SubRequests)>,
    subs: &OutboxSubscriptions,
    request_filters: RequestFilters,
    limits: ReqFilterLimits,
    available_passes: usize,
) -> Option<PlannedPlacement> {
    for (relay_id, requests) in existing {
        if requests.can_fit_filters(subs, &request_filters, limits) {
            return Some(PlannedPlacement::Existing {
                relay_id,
                filters: request_filters,
            });
        }
    }

    if available_passes == 0 {
        return None;
    }

    let empty = SubRequests::default();
    if !empty.can_fit_filters(subs, &request_filters, limits) {
        return None;
    }

    Some(PlannedPlacement::New {
        relay_id: RelayReqId::default(),
        filters: request_filters,
    })
}

fn plan_compaction_placements(
    existing: impl IntoIterator<Item = (RelayReqId, SubRequests)>,
    subs: &OutboxSubscriptions,
    subscribe_filter_sets: impl IntoIterator<Item = RequestFilters>,
    limits: ReqFilterLimits,
    available_passes: usize,
) -> Option<Vec<PlannedPlacement>> {
    let mut simulated_requests = existing.into_iter().collect::<Vec<_>>();
    let mut new_passes_needed = 0usize;
    let mut placements = Vec::new();

    for request_filters in subscribe_filter_sets {
        if let Some((relay_id, requests)) = simulated_requests
            .iter_mut()
            .find(|(_, requests)| requests.can_fit_filters(subs, &request_filters, limits))
        {
            requests.add_filters(request_filters.id, request_filters.filters.clone());
            placements.push(PlannedPlacement::Existing {
                relay_id: relay_id.clone(),
                filters: request_filters,
            });
            continue;
        }

        if new_passes_needed >= available_passes {
            return None;
        }

        let relay_id = RelayReqId::default();
        let mut requests = SubRequests::default();
        if !requests.can_fit_filters(subs, &request_filters, limits) {
            return None;
        }
        requests.add_filters(request_filters.id, request_filters.filters.clone());
        simulated_requests.push((relay_id.clone(), requests));
        new_passes_needed += 1;
        placements.push(PlannedPlacement::New {
            relay_id,
            filters: request_filters,
        });
    }

    Some(placements)
}

fn take_smallest_sub_reqs(
    subs: &OutboxSubscriptions,
    data: &mut HashMap<RelayReqId, RelaySubData>,
) -> Option<(RelayReqId, RelaySubData)> {
    let mut smallest = usize::MAX;
    let mut res = None;

    for (id, d) in data.iter() {
        let cur_size = d.json_size(subs);
        if cur_size < smallest {
            smallest = cur_size;
            res = Some(id.clone());
        }
    }

    let id = res?;

    data.remove(&id).map(|r| (id, r))
}

fn are_filters_empty(filters: &[Filter]) -> bool {
    filters.iter().all(|filter| filter.num_elements() == 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnfitRelaySubDisposition {
    Release,
    Queue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnfitRelaySubScope {
    All,
}

/// Represents a singular REQ to a relay
struct RelaySubData {
    requests: SubRequests,
    status: RelayReqStatus,
    sub_pass: SubPass,
}

impl RelaySubData {
    fn json_size(&self, subs: &OutboxSubscriptions) -> usize {
        self.requests.json_size(subs)
    }

    fn fits(&self, subs: &OutboxSubscriptions, limits: ReqFilterLimits) -> bool {
        self.requests.fits(subs, limits)
    }

    fn filters_for_compaction(&self, subs: &OutboxSubscriptions) -> Vec<Filter> {
        self.requests.filters_for_compaction(subs)
    }

    fn request_filters(&self, subs: &OutboxSubscriptions) -> Vec<RequestFilters> {
        self.requests.request_filters(subs)
    }
}

#[derive(Clone)]
struct RequestFilters {
    id: OutboxSubId,
    filters: Vec<IndexedFilter>,
}

impl RequestFilters {
    fn projected_filters(&self, subs: &OutboxSubscriptions) -> Option<Vec<IndexedFilter>> {
        projected_filters_for_compaction(subs, self.id, &self.filters)
    }
}

#[derive(Clone, Default)]
struct SubRequests {
    pub requests: HashSet<OutboxSubId>,
    filters: HashMap<OutboxSubId, Vec<IndexedFilter>>,
}

impl SubRequests {
    #[profiling::function]
    fn add_filters(&mut self, id: OutboxSubId, filters: Vec<IndexedFilter>) {
        self.requests.insert(id);
        self.filters.insert(id, filters);
    }

    pub fn remove(&mut self, id: &OutboxSubId) -> bool {
        self.filters.remove(id);
        self.requests.remove(id)
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    fn can_fit_filters(
        &self,
        subs: &OutboxSubscriptions,
        request_filters: &RequestFilters,
        limits: ReqFilterLimits,
    ) -> bool {
        if self.requests.contains(&request_filters.id) {
            return false;
        }

        let Some(new_filters) = request_filters.projected_filters(subs) else {
            return false;
        };
        let new_filter_count = new_filters.len();
        let new_size = indexed_filter_json_size_sum(&new_filters);

        limits.can_fit(
            self.filter_count_inner(),
            new_filter_count,
            self.json_size_inner(subs),
            new_size,
        )
    }

    fn filter_count_inner(&self) -> usize {
        self.filters.values().map(Vec::len).sum()
    }

    fn json_size(&self, subs: &OutboxSubscriptions) -> usize {
        self.json_size_inner(subs)
    }

    fn json_size_inner(&self, subs: &OutboxSubscriptions) -> usize {
        self.filters
            .iter()
            .filter_map(|(id, filters)| projected_filters_for_compaction(subs, *id, filters))
            .map(|filters| indexed_filter_json_size_sum(&filters))
            .fold(0usize, usize::saturating_add)
    }

    fn fits(&self, subs: &OutboxSubscriptions, limits: ReqFilterLimits) -> bool {
        limits.can_fit(0, self.filter_count_inner(), 0, self.json_size_inner(subs))
    }

    fn filters_for_compaction(&self, subs: &OutboxSubscriptions) -> Vec<Filter> {
        self.filters
            .iter()
            .filter_map(|(id, filters)| projected_filters_for_compaction(subs, *id, filters))
            .flat_map(concrete_filters)
            .collect()
    }

    fn request_filters(&self, subs: &OutboxSubscriptions) -> Vec<RequestFilters> {
        self.filters
            .iter()
            .filter_map(|(id, filters)| {
                Some(RequestFilters {
                    id: *id,
                    filters: projected_filters_for_compaction(subs, *id, filters)?,
                })
            })
            .collect()
    }
}

fn projected_filters_for_compaction(
    subs: &OutboxSubscriptions,
    id: OutboxSubId,
    filters: &[IndexedFilter],
) -> Option<Vec<IndexedFilter>> {
    let projected = subs.filters_for_compaction(&id)?;
    filters
        .iter()
        .map(|entry| {
            project_indexed_filter_for_compaction(entry, projected.get(entry.source_index))
        })
        .collect()
}

fn project_indexed_filter_for_compaction(
    entry: &IndexedFilter,
    source: Option<&Filter>,
) -> Option<IndexedFilter> {
    let Some(since) = source.and_then(Filter::since) else {
        return Some(entry.clone());
    };

    if entry.filter.since() == Some(since) {
        return Some(entry.clone());
    }

    let filter = entry.filter.clone().since_mut(since);
    let json_size = ReqFilterLimits::filter_json_size(&filter)?;
    Some(IndexedFilter {
        source_index: entry.source_index,
        filter,
        json_size,
    })
}

fn concrete_filters(filters: Vec<IndexedFilter>) -> Vec<Filter> {
    filters.into_iter().map(|entry| entry.filter).collect()
}

fn indexed_filter_json_size_sum(filters: &[IndexedFilter]) -> usize {
    filters.iter().map(|entry| entry.json_size).sum()
}

fn compaction_filters_for_single_req(
    id: OutboxSubId,
    filters: Vec<Filter>,
    limits: ReqFilterLimits,
) -> Option<RequestFilters> {
    compaction_indexed_filters_for_single_req(id, filters.into_iter().enumerate(), limits)
}

fn compaction_indexed_filters_for_single_req(
    id: OutboxSubId,
    filters: impl IntoIterator<Item = (usize, Filter)>,
    limits: ReqFilterLimits,
) -> Option<RequestFilters> {
    Some(RequestFilters {
        id,
        filters: limits.indexed_filters_for_single_req(filters)?,
    })
}

#[derive(Default)]
pub struct CompactionOperationPlan {
    // Number of subs which should be free after ingestion. Subs will compact enough to free up that number of subs
    // OR as much as possible without dropping any existing subs
    request_free: usize,
    tasks: HashMap<OutboxSubId, RelayTask>,
    subscribe_order: Vec<OutboxSubId>,
}

impl CompactionOperationPlan {
    pub fn request_free_subs(&mut self, num_free: usize) {
        self.request_free = num_free;
    }

    pub fn unsub(&mut self, unsub: OutboxSubId) {
        self.tasks.insert(unsub, RelayTask::Unsubscribe);
        self.subscribe_order.retain(|id| *id != unsub);
    }

    pub fn sub(&mut self, id: OutboxSubId) {
        let was_subscribe = self.tasks.get(&id) == Some(&RelayTask::Subscribe);
        self.tasks.insert(id, RelayTask::Subscribe);
        if !was_subscribe {
            self.subscribe_order.push(id);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.request_free == 0
    }

    pub(crate) fn subscribe_ids(&self) -> impl Iterator<Item = OutboxSubId> + '_ {
        self.subscribe_order.iter().copied().filter(|id| {
            self.tasks
                .get(id)
                .is_some_and(|task| *task == RelayTask::Subscribe)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        relay::{frame::QueuedRelayFrame, RelayUrlPkgs, SubPassGuardian, SubscribeTask},
        Pubkey,
    };
    use hashbrown::HashSet;

    fn add_sub_request_filters(
        requests: &mut SubRequests,
        subs: &OutboxSubscriptions,
        id: OutboxSubId,
    ) {
        requests.add_filters(
            id,
            subs.filters_for_compaction(&id)
                .expect("subscription filters")
                .into_iter()
                .enumerate()
                .map(|(source_index, filter)| {
                    let json_size =
                        ReqFilterLimits::filter_json_size(&filter).expect("test filter json");
                    IndexedFilter {
                        source_index,
                        filter,
                        json_size,
                    }
                })
                .collect(),
        );
    }

    fn add_test_sub_request(requests: &mut SubRequests, id: OutboxSubId) {
        let filter = Filter::new().kinds([1]).build();
        let json_size = ReqFilterLimits::filter_json_size(&filter).expect("test filter json");
        requests.add_filters(
            id,
            vec![IndexedFilter {
                source_index: 0,
                filter,
                json_size,
            }],
        );
    }

    fn sub_requests_can_fit_candidate(
        requests: &SubRequests,
        subs: &OutboxSubscriptions,
        new: &OutboxSubId,
        limits: ReqFilterLimits,
    ) -> bool {
        let Some(filters) = subs.filters_for_compaction(new) else {
            return false;
        };
        let Some(request_filters) = compaction_filters_for_single_req(*new, filters, limits) else {
            return false;
        };

        requests.can_fit_filters(subs, &request_filters, limits)
    }

    fn apply_operation_plan_for_test(
        current_generation: Option<u64>,
        data: &mut CompactionData,
        json_limit: usize,
        sub_guardian: &mut SubPassGuardian,
        subs: &OutboxSubscriptions,
        plan: CompactionOperationPlan,
    ) -> CompactionTransition {
        apply_operation_plan_with_limits_for_test(
            current_generation,
            data,
            ReqFilterLimits::new(usize::MAX, json_limit),
            sub_guardian,
            subs,
            plan,
        )
    }

    fn apply_operation_plan_with_limits_for_test(
        current_generation: Option<u64>,
        data: &mut CompactionData,
        limits: ReqFilterLimits,
        sub_guardian: &mut SubPassGuardian,
        subs: &OutboxSubscriptions,
        plan: CompactionOperationPlan,
    ) -> CompactionTransition {
        let transition = data.apply_operation_plan(
            current_generation,
            limits,
            take_all_passes(sub_guardian),
            subs,
            plan,
        );
        finish_test_transition(sub_guardian, transition)
    }

    fn handle_relay_open_for_test(
        current_generation: Option<u64>,
        data: &mut CompactionData,
        json_limit: usize,
        sub_guardian: &mut SubPassGuardian,
        subs: &OutboxSubscriptions,
    ) -> CompactionReplayOutcome {
        let replay = data.handle_relay_open(
            current_generation,
            ReqFilterLimits::new(usize::MAX, json_limit),
            take_all_passes(sub_guardian),
            subs,
        );
        finish_test_replay(sub_guardian, replay)
    }

    fn take_all_passes(guardian: &mut SubPassGuardian) -> Vec<SubPass> {
        let mut granted_passes = Vec::new();
        while let Some(pass) = guardian.take_pass() {
            granted_passes.push(pass);
        }
        granted_passes
    }

    fn finish_test_transition(
        guardian: &mut SubPassGuardian,
        mut transition: CompactionTransition,
    ) -> CompactionTransition {
        for pass in std::mem::take(&mut transition.returned_passes) {
            guardian.return_pass(pass);
        }
        transition
    }

    fn finish_test_replay(
        guardian: &mut SubPassGuardian,
        mut replay: CompactionReplayOutcome,
    ) -> CompactionReplayOutcome {
        for pass in std::mem::take(&mut replay.returned_passes) {
            guardian.return_pass(pass);
        }
        replay
    }

    fn pubkey(index: u8) -> Pubkey {
        let mut bytes = [0u8; 32];
        bytes[31] = index;
        Pubkey::new(bytes)
    }

    fn captured_frame_sid(frame: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(frame).expect("parse captured frame");
        value
            .get(1)
            .and_then(serde_json::Value::as_str)
            .expect("captured frame sid")
            .to_owned()
    }

    fn captured_frame_kind(frame: &str) -> &str {
        let value: serde_json::Value = serde_json::from_str(frame).expect("parse captured frame");
        match value.get(0).and_then(serde_json::Value::as_str) {
            Some("REQ") => "REQ",
            Some("CLOSE") => "CLOSE",
            other => panic!("unexpected captured frame kind {other:?}: {frame}"),
        }
    }

    fn frame_jsons(frames: &[QueuedRelayFrame]) -> Vec<String> {
        frames
            .iter()
            .map(|(_, message)| {
                message
                    .to_json()
                    .expect("captured message should serialize")
            })
            .collect()
    }

    // ==================== CompactionData tests ====================

    #[test]
    fn compaction_relay_returns_req_frame() {
        let id = OutboxSubId(42);
        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let generation = Some(7);
        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let mut session = CompactionOperationPlan::default();
        session.sub(id);

        let transition = data.apply_operation_plan(
            generation,
            ReqFilterLimits::new(usize::MAX, usize::MAX),
            take_all_passes(&mut guardian),
            &subs,
            session,
        );
        let transition = finish_test_transition(&mut guardian, transition);

        let frames = frame_jsons(&transition.frames);
        assert_eq!(frames.len(), 1);
        assert_eq!(captured_frame_kind(&frames[0]), "REQ");
    }

    #[test]
    fn compaction_data_default_empty() {
        let data = CompactionData::default();
        assert_eq!(data.num_subs(), 0);
    }

    #[test]
    fn compaction_data_req_status_none_for_unknown() {
        let data = CompactionData::default();
        assert!(data.req_status(&OutboxSubId(999)).is_none());
    }

    #[test]
    fn compaction_data_has_eose_false_for_unknown() {
        let data = CompactionData::default();
        assert!(!data.has_eose(&OutboxSubId(999)));
    }

    #[test]
    fn compaction_data_req_status_returns_single_relay_req_status() {
        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let id = OutboxSubId(7);
        let sid = RelayReqId::from("single-sid");
        let mut requests = SubRequests::default();
        add_test_sub_request(&mut requests, id);
        data.relay_subs.insert(
            sid.clone(),
            RelaySubData {
                requests,
                status: RelayReqStatus::Closed,
                sub_pass: guardian.take_pass().expect("available relay pass"),
            },
        );
        data.insert_request_sid(id, sid);

        assert_eq!(data.req_status(&id), Some(RelayReqStatus::Closed));
    }

    #[test]
    fn compaction_data_apply_eose_ignores_unknown_sid() {
        let mut data = CompactionData::default();
        let transition = data.apply_eose(&RelayReqId::from("unknown-sid"));
        assert!(transition.invalidated_sub_ids.is_empty());
        assert!(transition.status_changed_sub_ids.is_empty());
        assert!(transition.eose_sub_ids.is_empty());
    }

    #[test]
    fn compaction_data_apply_eose_updates_status_and_returns_affected_ids() {
        let mut data = CompactionData::default();

        let relay_id = RelayReqId::from("test-sid");
        let id = OutboxSubId(7);
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();
        let mut requests = SubRequests::default();
        add_test_sub_request(&mut requests, id);

        data.relay_subs.insert(
            relay_id.clone(),
            RelaySubData {
                requests,
                status: RelayReqStatus::InitialQuery,
                sub_pass: pass,
            },
        );
        data.insert_request_sid(id, relay_id.clone());

        let transition = data.apply_eose(&relay_id);

        let sub_data = data.relay_subs.get(&relay_id).unwrap();
        assert_eq!(sub_data.status, RelayReqStatus::Eose);
        assert_eq!(transition.status_changed_sub_ids, HashSet::from([id]));
        assert_eq!(transition.eose_sub_ids, HashSet::from([id]));
    }

    // ==================== SubRequests tests ====================

    /// can_fit returns true when combined JSON size is under the limit.
    #[test]
    fn sub_requests_can_fit() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let requests = SubRequests::default();
        let roomy_limits = ReqFilterLimits::new(25, 1_000_000);
        let tight_limits = ReqFilterLimits::new(25, 5);

        assert!(sub_requests_can_fit_candidate(
            &requests,
            &subs,
            &OutboxSubId(0),
            roomy_limits
        ));
        assert!(!sub_requests_can_fit_candidate(
            &requests,
            &subs,
            &OutboxSubId(0),
            tight_limits
        ));
    }

    #[test]
    fn sub_requests_can_fit_respects_filters_per_req_limit() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut requests = SubRequests::default();
        add_sub_request_filters(&mut requests, &subs, OutboxSubId(0));

        assert!(!sub_requests_can_fit_candidate(
            &requests,
            &subs,
            &OutboxSubId(1),
            ReqFilterLimits::new(1, 1_000_000)
        ));
    }

    // ==================== CompactionOperationPlan tests ====================

    #[test]
    fn compaction_operation_plan_default() {
        let session = CompactionOperationPlan::default();
        assert_eq!(session.request_free, 0);
        assert!(session.tasks.is_empty());
    }

    #[test]
    fn compaction_operation_plan_unsub() {
        let mut session = CompactionOperationPlan::default();
        session.unsub(OutboxSubId(42));

        assert!(session.tasks.contains_key(&OutboxSubId(42)));
        match session.tasks.get(&OutboxSubId(42)) {
            Some(RelayTask::Unsubscribe) => (),
            _ => panic!("Expected Unsubscribe task"),
        }
    }

    #[test]
    fn compaction_operation_plan_sub() {
        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(1));

        assert!(session.tasks.contains_key(&OutboxSubId(1)));
        assert!(matches!(
            session.tasks.get(&OutboxSubId(1)),
            Some(RelayTask::Subscribe)
        ));
    }

    // ==================== take_smallest_sub_reqs tests ====================

    #[test]
    fn take_smallest_returns_none_for_empty() {
        let subs = OutboxSubscriptions::default();
        let mut data: HashMap<RelayReqId, RelaySubData> = HashMap::new();
        assert!(take_smallest_sub_reqs(&subs, &mut data).is_none());
    }

    /// Returns the relay sub with the smallest combined JSON size.
    #[test]
    fn take_smallest_returns_smallest_by_json_size() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        // Register subscriptions with different JSON sizes
        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new()
                    .kinds(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
                    .build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut guardian = SubPassGuardian::new(2);

        // Small relay sub contains id 0
        let mut small_requests = SubRequests::default();
        add_sub_request_filters(&mut small_requests, &subs, OutboxSubId(0));

        // Large relay sub contains id 1
        let mut large_requests = SubRequests::default();
        add_sub_request_filters(&mut large_requests, &subs, OutboxSubId(1));

        let mut data: HashMap<RelayReqId, RelaySubData> = HashMap::new();
        data.insert(
            RelayReqId::from("small"),
            RelaySubData {
                requests: small_requests,
                status: RelayReqStatus::InitialQuery,
                sub_pass: guardian.take_pass().unwrap(),
            },
        );
        data.insert(
            RelayReqId::from("large"),
            RelaySubData {
                requests: large_requests,
                status: RelayReqStatus::InitialQuery,
                sub_pass: guardian.take_pass().unwrap(),
            },
        );

        let (id, _) = take_smallest_sub_reqs(&subs, &mut data).unwrap();
        assert_eq!(id.0, "small");
        assert_eq!(data.len(), 1);
    }

    #[test]
    fn take_smallest_removes_from_map() {
        let subs = OutboxSubscriptions::default();
        let mut data: HashMap<RelayReqId, RelaySubData> = HashMap::new();
        let mut guardian = SubPassGuardian::new(1);

        data.insert(
            RelayReqId::from("only"),
            RelaySubData {
                requests: SubRequests::default(),
                status: RelayReqStatus::InitialQuery,
                sub_pass: guardian.take_pass().unwrap(),
            },
        );

        let result = take_smallest_sub_reqs(&subs, &mut data);
        assert!(result.is_some());
        assert!(data.is_empty());
    }

    // ==================== CompactionData transition tests ====================

    /// Requesting free subs when there's nothing to compact has no effect.
    #[test]
    fn compact_returns_none_when_no_subs() {
        let subs = OutboxSubscriptions::default();
        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(5);
        let json_limit = 100000;

        let initial_passes = guardian.available_passes();

        let mut session = CompactionOperationPlan::default();
        session.request_free_subs(1);
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(guardian.available_passes(), initial_passes);
    }

    /// Compacting frees a pass and redistributes requests to remaining subs.
    #[test]
    fn compact_frees_pass_and_redistributes() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new()
                    .kinds(vec![2, 3, 4, 5, 6, 7, 8, 9, 10])
                    .build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(5);
        let json_limit = 100000;

        // Create 2 relay subs
        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(data.relay_subs.len(), 2);
        assert_eq!(guardian.available_passes(), 3); // 5 - 2

        // Request 4 free passes - must compact 1
        let mut session = CompactionOperationPlan::default();
        session.request_free_subs(4);
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(data.relay_subs.len(), 1);
        assert_eq!(guardian.available_passes(), 4);

        let remaining = data.relay_subs.values().next().unwrap();
        assert_eq!(remaining.requests.requests.len(), 2);
    }

    /// When compaction redistributes a request but the remaining sub
    /// doesn't have room, the request goes to the queue.
    #[test]
    fn place_queues_when_no_room() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        // Set limit so combined filters won't fit in one REQ
        let size0 = subs.json_size(&OutboxSubId(0)).unwrap();
        let size1 = subs.json_size(&OutboxSubId(1)).unwrap();
        let json_limit = size0 + size1 + ReqFilterLimits::req_overhead() - 1;

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(2);

        // Create 2 relay subs at capacity
        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(data.relay_subs.len(), 2);
        assert!(data.queue.is_empty());

        // Compact 1 - redistributed request won't fit
        let mut session = CompactionOperationPlan::default();
        session.request_free_subs(1);
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(data.relay_subs.len(), 1);
        assert!(!data.queue.is_empty());
    }

    #[test]
    fn place_queues_when_filters_per_req_limit_is_full() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            ReqFilterLimits::new(1, 100_000),
            &mut guardian,
            &subs,
            session,
        );

        assert_eq!(data.relay_subs.len(), 1);
        let sub = data.relay_subs.values().next().unwrap();
        assert_eq!(sub.requests.filter_count_inner(), 1);
        assert_eq!(data.queue.len(), 1);
    }

    #[test]
    fn new_sub_queues_single_request_that_exceeds_single_req_even_when_passes_exist() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let id = OutboxSubId(0);
        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: vec![
                    Filter::new().kinds(vec![1]).build(),
                    Filter::new().kinds(vec![2]).build(),
                ],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut session = CompactionOperationPlan::default();
        session.sub(id);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            ReqFilterLimits::new(1, 100_000),
            &mut guardian,
            &subs,
            session,
        );

        assert!(
            data.relay_subs.is_empty(),
            "compaction must not spread one logical sub across multiple REQs"
        );
        assert_eq!(data.queue.len(), 1);
        assert_eq!(guardian.available_passes(), 2);
    }

    #[test]
    fn new_sub_queues_oversized_author_filter_that_exceeds_single_req() {
        let id = OutboxSubId(0);
        let authors = (0..6).map(pubkey).collect::<Vec<_>>();
        let filter = Filter::new()
            .authors(authors.iter().map(Pubkey::bytes))
            .kinds([1])
            .build();
        let two_author_filter = Filter::new()
            .authors(authors[0..2].iter().map(Pubkey::bytes))
            .kinds([1])
            .build();
        let two_author_size =
            ReqFilterLimits::filter_json_size(&two_author_filter).expect("two author size");
        let limits = ReqFilterLimits::new(200, ReqFilterLimits::req_json_size(1, two_author_size));

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: vec![filter],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(3);
        let mut session = CompactionOperationPlan::default();
        session.sub(id);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            limits,
            &mut guardian,
            &subs,
            session,
        );

        assert!(
            data.relay_subs.is_empty(),
            "oversized author filters must stay queued until they fit one compaction REQ"
        );
        assert_eq!(data.queue.len(), 1);
        assert_eq!(guardian.available_passes(), 3);
    }

    #[test]
    fn new_sub_queues_single_request_that_exceeds_actual_req_json_limit() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let id = OutboxSubId(0);
        let filters = vec![
            Filter::new().kinds(vec![1]).build(),
            Filter::new().kinds(vec![2]).build(),
        ];
        let sid = RelayReqId::from("123e4567-e89b-12d3-a456-426614174000");
        let max_json_bytes = ClientMessage::req(sid.to_string(), filters.clone())
            .to_json()
            .expect("serialize combined compaction req")
            .len()
            - 1;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters,
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut session = CompactionOperationPlan::default();
        session.sub(id);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            ReqFilterLimits::new(usize::MAX, max_json_bytes),
            &mut guardian,
            &subs,
            session,
        );

        assert!(
            data.relay_subs.is_empty(),
            "compaction must not spread one logical sub at the relay JSON boundary"
        );
        assert_eq!(data.queue.len(), 1);
        assert_eq!(guardian.available_passes(), 2);
    }

    #[test]
    fn new_sub_queues_single_request_that_exceeds_available_passes() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![
                    Filter::new().kinds(vec![1]).build(),
                    Filter::new().kinds(vec![2]).build(),
                ],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            ReqFilterLimits::new(1, 100_000),
            &mut guardian,
            &subs,
            session,
        );

        assert!(data.relay_subs.is_empty());
        assert_eq!(data.queue.len(), 1);
        assert_eq!(guardian.available_passes(), 1);
    }

    #[test]
    fn subscribe_queues_modified_compaction_sub_when_filter_count_exceeds_limit() {
        use crate::relay::{FullRelayPkgsModificationTask, RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let id = OutboxSubId(0);
        let mut subs = OutboxSubscriptions::default();
        let relay_pkgs = RelayUrlPkgs::new(
            HashSet::new(),
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::PreferDedicated,
            ),
        );
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: relay_pkgs.clone(),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(2);
        let limits = ReqFilterLimits::new(1, 100_000);
        let mut initial = CompactionOperationPlan::default();
        initial.sub(id);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            limits,
            &mut guardian,
            &subs,
            initial,
        );

        assert_eq!(data.relay_subs.len(), 1);
        assert_eq!(guardian.available_passes(), 1);

        subs.ingest_task(
            &id,
            crate::relay::ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: vec![
                    Filter::new().kinds(vec![1]).build(),
                    Filter::new().kinds(vec![2]).build(),
                ],
                relays: relay_pkgs,
            }),
        );

        let mut modify = CompactionOperationPlan::default();
        modify.sub(id);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            limits,
            &mut guardian,
            &subs,
            modify,
        );

        assert!(
            data.relay_subs.is_empty(),
            "modified logical sub must leave compaction when it no longer fits one REQ"
        );
        assert_eq!(data.queue.len(), 1);
        assert_eq!(guardian.available_passes(), 2);
    }

    /// When no passes are available, requests are placed on existing relay subs.
    #[test]
    fn new_sub_places_on_existing_when_no_passes() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1); // Only 1 pass
        let json_limit = 100000;

        // Add 2 requests with only 1 pass - second must go on existing
        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(data.relay_subs.len(), 1);
        let sub = data.relay_subs.values().next().unwrap();
        assert_eq!(sub.requests.requests.len(), 2);
    }

    /// Subscriptions placed onto an existing compacted REQ must register
    /// request-to-relay mapping so a later unsubscribe updates the correct REQ.
    #[test]
    fn unsubscribe_after_place_on_existing_removes_request() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1); // Force second sub onto existing REQ
        let json_limit = 100000;

        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert_eq!(data.relay_subs.len(), 1);
        let relay_id = data.relay_subs.keys().next().cloned().unwrap();
        assert_eq!(data.request_to_sid.get(&OutboxSubId(0)), Some(&relay_id));
        assert_eq!(data.request_to_sid.get(&OutboxSubId(1)), Some(&relay_id));

        let mut session = CompactionOperationPlan::default();
        session.unsub(OutboxSubId(1));
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        assert!(data.queue.is_empty());
        assert_eq!(data.relay_subs.len(), 1);
        let sub = data.relay_subs.get(&relay_id).unwrap();
        assert_eq!(sub.requests.requests.len(), 1);
        assert!(sub.requests.requests.contains(&OutboxSubId(0)));
        assert!(!sub.requests.requests.contains(&OutboxSubId(1)));
        assert_eq!(data.request_to_sid.get(&OutboxSubId(0)), Some(&relay_id));
        assert!(!data.request_to_sid.contains_key(&OutboxSubId(1)));
    }

    /// Touching one compacted REQ must invalidate every sub carried by that REQ.
    #[test]
    fn apply_operation_plan_reports_invalidations_for_all_touched_compaction_subs() {
        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let json_limit = 100_000;

        let mut create = CompactionOperationPlan::default();
        create.sub(OutboxSubId(0));
        assert_eq!(
            apply_operation_plan_for_test(
                None,
                &mut data,
                json_limit,
                &mut guardian,
                &subs,
                create
            )
            .invalidated_sub_ids,
            HashSet::from([OutboxSubId(0)])
        );

        let mut touch = CompactionOperationPlan::default();
        touch.sub(OutboxSubId(1));
        let invalidated =
            apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, touch)
                .invalidated_sub_ids;

        assert_eq!(
            invalidated,
            HashSet::from([OutboxSubId(0), OutboxSubId(1)]),
            "adding one sub to an existing compacted REQ reissues that REQ for every carried sub"
        );
    }

    #[test]
    fn touched_active_compaction_req_reissues_same_sid_without_close() {
        let id_a = OutboxSubId(0);
        let id_b = OutboxSubId(1);
        let mut subs = OutboxSubscriptions::default();
        for (id, kind) in [(id_a, 1), (id_b, 2)] {
            subs.new_subscription(
                id,
                SubscribeTask {
                    filters: vec![Filter::new().kinds([kind]).build()],
                    relays: RelayUrlPkgs::new(
                        HashSet::new(),
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                },
                false,
            );
        }

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let json_limit = 100_000;
        let generation = Some(0);

        let mut initial = CompactionOperationPlan::default();
        initial.sub(id_a);
        let initial_transition = apply_operation_plan_for_test(
            generation,
            &mut data,
            json_limit,
            &mut guardian,
            &subs,
            initial,
        );

        let captured = frame_jsons(&initial_transition.frames);
        assert_eq!(captured.len(), 1);
        let old_sid = data
            .request_to_sid
            .get(&id_a)
            .expect("initial compaction sid")
            .clone();

        let mut touched = CompactionOperationPlan::default();
        touched.sub(id_b);
        let touched_transition = apply_operation_plan_for_test(
            generation,
            &mut data,
            json_limit,
            &mut guardian,
            &subs,
            touched,
        );

        let touched_frames = frame_jsons(&touched_transition.frames);
        assert_eq!(touched_frames.len(), 1);
        assert_eq!(captured_frame_kind(&captured[0]), "REQ");
        assert_eq!(captured_frame_kind(&touched_frames[0]), "REQ");
        assert_eq!(captured_frame_sid(&captured[0]), old_sid.to_string());
        assert_eq!(captured_frame_sid(&touched_frames[0]), old_sid.to_string());

        let current_sid = data
            .request_to_sid
            .get(&id_a)
            .expect("retained sid for first sub");
        assert_eq!(current_sid, &old_sid);
        assert_eq!(data.request_to_sid.get(&id_b), Some(current_sid));
    }

    #[tokio::test]
    async fn handle_relay_open_reports_all_reissued_compaction_sub_ids() {
        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            OutboxSubId(0),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let json_limit = 100_000;

        let mut session = CompactionOperationPlan::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        let _ = apply_operation_plan_for_test(
            Some(0),
            &mut data,
            json_limit,
            &mut guardian,
            &subs,
            session,
        );

        let sid0 = data
            .request_to_sid
            .get(&OutboxSubId(0))
            .expect("sid for first compaction sub")
            .clone();
        let sid1 = data
            .request_to_sid
            .get(&OutboxSubId(1))
            .expect("sid for second compaction sub")
            .clone();
        let _ = data.apply_eose(&sid0);
        let _ = data.apply_eose(&sid1);

        let replay =
            handle_relay_open_for_test(Some(1), &mut data, json_limit, &mut guardian, &subs);

        assert_eq!(
            replay.invalidated_sub_ids,
            HashSet::from([OutboxSubId(0), OutboxSubId(1)]),
            "relay-open replay should invalidate every sub carried by the reissued compaction REQ"
        );
        assert!(
            replay.released_ids.is_empty(),
            "single-REQ compaction replay should not leave compaction"
        );
        assert_eq!(
            data.req_status(&OutboxSubId(0)),
            Some(RelayReqStatus::InitialQuery),
            "relay-open replay must reset compaction req status to InitialQuery"
        );
        assert_eq!(
            data.req_status(&OutboxSubId(1)),
            Some(RelayReqStatus::InitialQuery),
            "relay-open replay must reset compaction req status to InitialQuery"
        );
    }

    #[tokio::test]
    async fn handle_relay_open_releases_active_request_that_exceeds_single_req() {
        let id = OutboxSubId(0);
        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: vec![
                    Filter::new().kinds(vec![1]).build(),
                    Filter::new().kinds(vec![2]).build(),
                ],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let initial_limits = ReqFilterLimits::new(2, 100_000);
        let mut session = CompactionOperationPlan::default();
        session.sub(id);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            initial_limits,
            &mut guardian,
            &subs,
            session,
        );
        assert_eq!(data.req_status(&id), Some(RelayReqStatus::InitialQuery));

        let reopened_limits = ReqFilterLimits::new(1, 100_000);
        let replay = data.handle_relay_open(
            Some(1),
            reopened_limits,
            take_all_passes(&mut guardian),
            &subs,
        );
        let replay = finish_test_replay(&mut guardian, replay);

        assert_eq!(replay.invalidated_sub_ids, HashSet::from([id]));
        assert_eq!(
            replay.released_ids,
            HashSet::from([id]),
            "unrepresentable compaction must leave active compaction ownership"
        );
        assert_eq!(
            data.req_status(&id),
            None,
            "reopen must drop an active compaction REQ that cannot be sent under current limits"
        );
        assert!(
            data.queue.is_empty(),
            "unrepresentable compaction should not create impossible queued work"
        );
        assert_eq!(
            data.relay_subs.len(),
            0,
            "unrepresentable compaction should not keep an unsendable active relay REQ"
        );
        assert_eq!(guardian.available_passes(), 1);
    }

    #[tokio::test]
    async fn handle_relay_open_releases_group_that_no_longer_fits_one_req() {
        let id_a = OutboxSubId(0);
        let id_b = OutboxSubId(1);
        let filter_a = Filter::new().kinds(vec![1]).build();
        let filter_b = Filter::new().kinds(vec![2]).build();
        let combined_json_limit = ClientMessage::req(
            RelayReqId::default().to_string(),
            vec![filter_a.clone(), filter_b.clone()],
        )
        .to_json()
        .expect("combined req json")
        .len();
        let mut subs = OutboxSubscriptions::default();
        for (id, filter) in [(id_a, filter_a), (id_b, filter_b)] {
            subs.new_subscription(
                id,
                SubscribeTask {
                    filters: vec![filter],
                    relays: RelayUrlPkgs::new(
                        HashSet::new(),
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                },
                false,
            );
        }

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let initial_limits = ReqFilterLimits::new(200, combined_json_limit);
        let mut session = CompactionOperationPlan::default();
        session.sub(id_a);
        session.sub(id_b);
        apply_operation_plan_with_limits_for_test(
            None,
            &mut data,
            initial_limits,
            &mut guardian,
            &subs,
            session,
        );
        assert_eq!(data.relay_subs.len(), 1);
        assert_eq!(guardian.available_passes(), 0);

        let reopened_limits = ReqFilterLimits::new(200, combined_json_limit - 1);
        let replay = data.handle_relay_open(
            Some(1),
            reopened_limits,
            take_all_passes(&mut guardian),
            &subs,
        );
        let replay = finish_test_replay(&mut guardian, replay);

        assert_eq!(replay.released_ids, HashSet::from([id_a, id_b]));
        assert_eq!(replay.invalidated_sub_ids, HashSet::from([id_a, id_b]));
        assert!(data.relay_subs.is_empty());
        assert_eq!(data.req_status(&id_a), None);
        assert_eq!(data.req_status(&id_b), None);
        assert_eq!(guardian.available_passes(), 1);
    }

    /// When requesting multiple free passes, multiple subs are compacted
    /// and all requests are consolidated into fewer relay subs.
    #[test]
    fn compact_multiple_subs() {
        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(3);
        let json_limit = 100000;
        let mut subs = OutboxSubscriptions::default();
        for i in 0..3 {
            subs.new_subscription(
                OutboxSubId(i),
                SubscribeTask {
                    filters: vec![Filter::new().kinds(vec![i + 1]).build()],
                    relays: RelayUrlPkgs::new(
                        HashSet::new(),
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                },
                false,
            );
        }

        // Create 3 subs and request 2 free in same session
        let mut session = CompactionOperationPlan::default();
        for i in 0..3 {
            session.sub(OutboxSubId(i));
        }
        session.request_free_subs(2);
        apply_operation_plan_for_test(None, &mut data, json_limit, &mut guardian, &subs, session);

        // Should compact down to 1 sub with all 3 requests
        assert_eq!(data.relay_subs.len(), 1);
        assert_eq!(guardian.available_passes(), 2);

        let sub = data.relay_subs.values().next().unwrap();
        assert_eq!(sub.requests.requests.len(), 3);
    }

    /// One unplaceable queued item should not block other queued items that can be placed.
    #[test]
    fn capacity_application_does_not_starve_placeable_items() {
        use crate::relay::{RelayUrlPkgs, SubscribeTask};
        use hashbrown::HashSet;

        let id_seed = OutboxSubId(0);
        let id_blocked = OutboxSubId(1);
        let id_placeable = OutboxSubId(2);

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id_seed,
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![1]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            id_blocked,
            SubscribeTask {
                filters: vec![Filter::new().kinds((2u64..40).collect::<Vec<_>>()).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        subs.new_subscription(
            id_placeable,
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![3]).build()],
                relays: RelayUrlPkgs::new(
                    HashSet::new(),
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        let relay_id = RelayReqId::from("123e4567-e89b-12d3-a456-426614174000");
        let req_json_len = |ids: &[OutboxSubId]| {
            let filters = ids
                .iter()
                .filter_map(|id| subs.filters_for_compaction(id))
                .flatten()
                .collect::<Vec<_>>();
            ClientMessage::req(relay_id.to_string(), filters)
                .to_json()
                .expect("serialize test req")
                .len()
        };
        let placeable_json_size = req_json_len(&[id_seed, id_placeable]);
        let blocked_json_size = req_json_len(&[id_seed, id_blocked]);
        assert!(blocked_json_size > placeable_json_size);
        let json_limit = placeable_json_size;

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let seed_pass = guardian.take_pass().unwrap();

        let mut requests = SubRequests::default();
        add_sub_request_filters(&mut requests, &subs, id_seed);
        data.relay_subs.insert(
            relay_id.clone(),
            RelaySubData {
                requests,
                status: RelayReqStatus::InitialQuery,
                sub_pass: seed_pass,
            },
        );
        data.request_to_sid.insert(id_seed, relay_id.clone());

        data.queue.enqueue(id_blocked);
        data.queue.enqueue(id_placeable);

        apply_operation_plan_for_test(
            None,
            &mut data,
            json_limit,
            &mut guardian,
            &subs,
            CompactionOperationPlan::default(),
        );

        assert_eq!(data.request_to_sid.get(&id_seed), Some(&relay_id));
        assert_eq!(data.request_to_sid.get(&id_placeable), Some(&relay_id));
        assert!(!data.request_to_sid.contains_key(&id_blocked));
        assert_eq!(data.queue.len(), 1);
    }
}
