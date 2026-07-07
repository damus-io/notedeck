use hashbrown::{HashMap, HashSet};
use uuid::Uuid;

use crate::{
    relay::{
        frame::QueuedRelayFrame, subscription::StoredSubscriptionRef, OutboxSubId, QueuedTasks,
        RelayReqId, RelayReqStatus, ReqFilterLimits, SubPass, SubPassRevocation,
    },
    same_canonical_filter_set, ClientMessage,
};

/// TransparentData tracks the outstanding transparent REQs and their metadata.
///
/// One `OutboxSubId` may be queued for retry, active on the relay, both when
/// the coordinator is retrying active-route growth, or absent.
#[derive(Default)]
pub(in crate::relay) struct TransparentData {
    active_leg_by_sid: HashMap<RelayReqId, ActiveTransparentLeg>,
    sid_by_id: HashMap<OutboxSubId, RelayReqId>,
    queue: QueuedTasks,
}

impl TransparentData {
    #[cfg(debug_assertions)]
    fn assert_consistent(&self) {
        debug_assert_eq!(
            self.sid_by_id.len(),
            self.active_leg_by_sid
                .values()
                .map(|leg| leg.owner_filter_revisions.len())
                .sum::<usize>(),
            "transparent owner index must match active relay-local owners"
        );
        for (sid, active_leg) in &self.active_leg_by_sid {
            debug_assert_eq!(sid, &active_leg.sid);
            debug_assert!(
                !active_leg.owner_filter_revisions.is_empty(),
                "transparent relay-local leg must have at least one owner"
            );
            for req_id in active_leg.owner_filter_revisions.keys() {
                debug_assert_eq!(
                    self.sid_by_id.get(req_id),
                    Some(sid),
                    "transparent owner index must point back to the owning relay sid"
                );
            }
        }
    }

    #[cfg(test)]
    pub(in crate::relay) fn num_subs(&self) -> usize {
        self.sid_by_id.len()
    }

    #[cfg(test)]
    pub(in crate::relay) fn contains(&self, id: &OutboxSubId) -> bool {
        self.sid_by_id.contains_key(id)
    }

    pub(in crate::relay) fn request_ids(&self) -> Vec<OutboxSubId> {
        self.sid_by_id.keys().copied().collect()
    }

    pub(in crate::relay) fn set_req_status(&mut self, sid: &str, status: RelayReqStatus) {
        let Some(active_leg) = self.active_leg_by_sid.get_mut(sid) else {
            return;
        };
        active_leg.status = status;
    }

    pub(in crate::relay) fn req_status(&self, req_id: &OutboxSubId) -> Option<RelayReqStatus> {
        let sid = self.sid_by_id.get(req_id)?;
        self.active_leg_by_sid.get(sid).map(|leg| leg.status)
    }

    /// Returns the OutboxSubIds associated with the given relay subscription ID.
    fn ids(&self, sid: &RelayReqId) -> Option<HashSet<OutboxSubId>> {
        Some(
            self.active_leg_by_sid
                .get(sid)?
                .owner_filter_revisions
                .keys()
                .copied()
                .collect(),
        )
    }

    /// Returns all outbox IDs currently carried by one transparent relay sid.
    pub(in crate::relay) fn ids_for_sid(&self, sid: &RelayReqId) -> Option<HashSet<OutboxSubId>> {
        self.ids(sid)
    }

    /// Returns the live relay subscription ID for one active transparent leg.
    pub(in crate::relay) fn active_sid(&self, req_id: &OutboxSubId) -> Option<RelayReqId> {
        self.sid_by_id.get(req_id).cloned()
    }

    /// Returns all owners sharing `req_id`'s relay subscription ID.
    pub(in crate::relay) fn owner_ids_for(
        &self,
        req_id: &OutboxSubId,
    ) -> Option<HashSet<OutboxSubId>> {
        let sid = self.sid_by_id.get(req_id)?;
        self.ids(sid)
    }

    pub(in crate::relay) fn active_leg_count(&self, req_id: &OutboxSubId) -> usize {
        usize::from(self.sid_by_id.contains_key(req_id))
    }

    /// Returns how many additional passes this transparent request needs.
    pub(in crate::relay) fn pass_deficit(
        &self,
        view: &StoredSubscriptionRef<'_>,
        limits: ReqFilterLimits,
    ) -> Option<usize> {
        let filters = view.filters.filters_for_single_req(limits)?;
        if let Some(current_sid) = self.sid_by_id.get(&view.id) {
            if self.leg_filters_match(current_sid, &filters) {
                return Some(0);
            }
            if self
                .matching_sid_for_filters(&filters, Some(current_sid))
                .is_some()
            {
                return Some(0);
            }
            if self.owner_count(current_sid) == 1 {
                return Some(0);
            }
        }
        if self.matching_sid_for_filters(&filters, None).is_some() {
            return Some(0);
        }
        if !limits.filters_fit_single_req(&filters)? {
            return None;
        }

        Some(1)
    }

    /// Classifies whether `view` can use the transparent route with the current
    /// pass budget.
    pub(in crate::relay) fn placement_feasibility(
        &self,
        view: &StoredSubscriptionRef<'_>,
        limits: ReqFilterLimits,
        available_passes: usize,
    ) -> TransparentPlacementFeasibility {
        let Some(pass_deficit) = self.pass_deficit(view, limits) else {
            return TransparentPlacementFeasibility::Unrepresentable;
        };

        let pass_deficit = pass_deficit.saturating_sub(available_passes);
        if pass_deficit > 0 {
            return TransparentPlacementFeasibility::NeedsCapacity { pass_deficit };
        }

        TransparentPlacementFeasibility::Ready
    }

    /// Returns the number of queued transparent retry requests.
    pub(in crate::relay) fn queued_len(&self) -> usize {
        self.queue.len()
    }

    /// Pops the next queued transparent retry without making placement policy decisions.
    pub(in crate::relay) fn pop_queued_retry(&mut self) -> Option<OutboxSubId> {
        self.queue.pop()
    }

    fn insert_active_leg(&mut self, active_leg: ActiveTransparentLeg) {
        let sid = active_leg.sid.clone();
        for req_id in active_leg.owner_filter_revisions.keys() {
            let old_sid = self.sid_by_id.insert(*req_id, sid.clone());
            debug_assert!(
                old_sid.is_none(),
                "transparent owner index must not overwrite an existing request"
            );
        }
        let old_active = self.active_leg_by_sid.insert(sid, active_leg);
        debug_assert!(
            old_active.is_none(),
            "transparent active_leg_by_sid must not overwrite an existing relay sid"
        );
        #[cfg(debug_assertions)]
        self.assert_consistent();
    }

    fn remove_active_leg_by_sid(&mut self, sid: &RelayReqId) -> Option<ActiveTransparentLeg> {
        let removed = self.active_leg_by_sid.remove(sid)?;
        for req_id in removed.owner_filter_revisions.keys() {
            let removed_sid = self.sid_by_id.remove(req_id);
            debug_assert_eq!(removed_sid.as_ref(), Some(sid));
        }
        #[cfg(debug_assertions)]
        self.assert_consistent();
        Some(removed)
    }

    fn remove_owner(&mut self, req_id: &OutboxSubId) -> Option<ActiveTransparentLeg> {
        let sid = self.sid_by_id.remove(req_id)?;
        let active_leg = self
            .active_leg_by_sid
            .get_mut(&sid)
            .expect("transparent owner sid should point to an active leg");
        active_leg.owner_filter_revisions.remove(req_id);
        if !active_leg.owner_filter_revisions.is_empty() {
            #[cfg(debug_assertions)]
            self.assert_consistent();
            return None;
        }

        let removed = self
            .active_leg_by_sid
            .remove(&sid)
            .expect("empty transparent leg should still exist");
        #[cfg(debug_assertions)]
        self.assert_consistent();
        Some(removed)
    }

    fn attach_owner(&mut self, sid: &RelayReqId, req_id: OutboxSubId, wire_filter_revision: u64) {
        let active_leg = self
            .active_leg_by_sid
            .get_mut(sid)
            .expect("transparent shared sid should exist");
        active_leg
            .owner_filter_revisions
            .insert(req_id, wire_filter_revision);
        let old_sid = self.sid_by_id.insert(req_id, sid.clone());
        debug_assert!(
            old_sid.is_none(),
            "transparent owner attach must not overwrite an existing request"
        );
        #[cfg(debug_assertions)]
        self.assert_consistent();
    }

    fn update_owner_revision(&mut self, req_id: OutboxSubId, wire_filter_revision: u64) {
        let sid = self
            .sid_by_id
            .get(&req_id)
            .expect("transparent owner should exist")
            .clone();
        let active_leg = self
            .active_leg_by_sid
            .get_mut(&sid)
            .expect("transparent owner sid should point to active leg");
        active_leg
            .owner_filter_revisions
            .insert(req_id, wire_filter_revision);
    }

    fn owner_count(&self, sid: &RelayReqId) -> usize {
        self.active_leg_by_sid
            .get(sid)
            .map(|leg| leg.owner_filter_revisions.len())
            .unwrap_or_default()
    }

    fn leg_filters_match(&self, sid: &RelayReqId, filters: &[nostrdb::Filter]) -> bool {
        self.active_leg_by_sid
            .get(sid)
            .is_some_and(|leg| same_canonical_filter_set(&leg.filters, filters))
    }

    fn matching_sid_for_filters(
        &self,
        filters: &[nostrdb::Filter],
        exclude_sid: Option<&RelayReqId>,
    ) -> Option<RelayReqId> {
        self.active_leg_by_sid
            .iter()
            .find(|(sid, leg)| {
                exclude_sid != Some(*sid) && same_canonical_filter_set(&leg.filters, filters)
            })
            .map(|(sid, _)| sid.clone())
    }

    #[cfg(test)]
    pub(in crate::relay) fn queued_len_for_test(&self) -> usize {
        self.queued_len()
    }

    /// Clears all transparent REQ state without sending `CLOSE` frames.
    pub(in crate::relay) fn clear_without_closing(&mut self) -> TransparentClearOutput {
        let mut affected = HashSet::new();
        let mut returned_passes = Vec::new();
        for (_, active_leg) in self.active_leg_by_sid.drain() {
            affected.extend(active_leg.owner_filter_revisions.keys().copied());
            returned_passes.push(active_leg.sub_pass);
        }
        self.sid_by_id.clear();

        while let Some(id) = self.queue.pop() {
            affected.insert(id);
        }

        TransparentClearOutput {
            affected,
            returned_passes,
        }
    }
}

/// Result of trying to place a subscription onto the transparent relay path.
pub(in crate::relay) enum TransparentPlaceResult {
    Placed,
    NoRoom,
}

/// Feasibility for placing one request on the transparent relay path.
pub(in crate::relay) enum TransparentPlacementFeasibility {
    Ready,
    NeedsCapacity { pass_deficit: usize },
    Unrepresentable,
}

/// Result of replaying one active transparent REQ on websocket reopen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::relay) enum TransparentReplayOutcome {
    Reissued(OutboxSubId),
    Blocked(OutboxSubId),
}

/// Output from a transparent placement attempt.
pub(in crate::relay) struct TransparentSubscribeOutput {
    pub(in crate::relay) result: TransparentPlaceResult,
    pub(in crate::relay) frames: Vec<QueuedRelayFrame>,
    pub(in crate::relay) returned_passes: Vec<SubPass>,
}

/// Output from transparent websocket-open replay.
pub(in crate::relay) struct TransparentReplayOutput {
    pub(in crate::relay) outcomes: Vec<TransparentReplayOutcome>,
    pub(in crate::relay) frames: Vec<QueuedRelayFrame>,
    pub(in crate::relay) returned_passes: Vec<SubPass>,
}

/// Output from transparent unsubscribe.
pub(in crate::relay) struct TransparentUnsubscribeOutput {
    pub(in crate::relay) frames: Vec<QueuedRelayFrame>,
    pub(in crate::relay) returned_passes: Vec<SubPass>,
}

/// Output from dropping all transparent state without sending relay frames.
pub(in crate::relay) struct TransparentClearOutput {
    pub(in crate::relay) affected: HashSet<OutboxSubId>,
    pub(in crate::relay) returned_passes: Vec<SubPass>,
}

impl TransparentData {
    /// Try to place this subscription on transparent without mutating the retry queue.
    pub(in crate::relay) fn try_subscribe(
        &mut self,
        current_generation: Option<u64>,
        pass: Option<SubPass>,
        limits: ReqFilterLimits,
        view: StoredSubscriptionRef,
    ) -> TransparentSubscribeOutput {
        self.try_subscribe_inner(current_generation, pass, limits, view)
    }

    fn return_unused_pass(returned_passes: &mut Vec<SubPass>, pass: Option<SubPass>) {
        if let Some(pass) = pass {
            returned_passes.push(pass);
        }
    }

    fn place_existing(
        frames: Vec<QueuedRelayFrame>,
        mut returned_passes: Vec<SubPass>,
        pass: Option<SubPass>,
    ) -> TransparentSubscribeOutput {
        Self::return_unused_pass(&mut returned_passes, pass);
        TransparentSubscribeOutput {
            result: TransparentPlaceResult::Placed,
            frames,
            returned_passes,
        }
    }

    fn no_room(
        frames: Vec<QueuedRelayFrame>,
        mut returned_passes: Vec<SubPass>,
        pass: Option<SubPass>,
    ) -> TransparentSubscribeOutput {
        Self::return_unused_pass(&mut returned_passes, pass);
        TransparentSubscribeOutput {
            result: TransparentPlaceResult::NoRoom,
            frames,
            returned_passes,
        }
    }

    fn try_subscribe_inner(
        &mut self,
        current_generation: Option<u64>,
        pass: Option<SubPass>,
        limits: ReqFilterLimits,
        view: StoredSubscriptionRef,
    ) -> TransparentSubscribeOutput {
        let req_id = view.id;
        self.queue.cancel(req_id);
        let Some(filters) = view.filters.filters_for_single_req(limits) else {
            let removed = self.unsubscribe_inner(current_generation, req_id);
            return Self::no_room(removed.frames, removed.returned_passes, pass);
        };

        if let Some(current_sid) = self.active_sid(&req_id) {
            if self.leg_filters_match(&current_sid, &filters) {
                self.update_owner_revision(req_id, view.wire_filter_revision);
                return Self::place_existing(Vec::new(), Vec::new(), pass);
            }

            if let Some(matching_sid) = self.matching_sid_for_filters(&filters, Some(&current_sid))
            {
                let mut frames = Vec::new();
                let mut returned_passes = Vec::new();
                if let Some(removed) = self.remove_owner(&req_id) {
                    let (pass, close_frames) = close_active_leg(current_generation, removed);
                    returned_passes.push(pass);
                    frames.extend(close_frames);
                }
                self.attach_owner(&matching_sid, req_id, view.wire_filter_revision);
                return Self::place_existing(frames, returned_passes, pass);
            }

            if self.owner_count(&current_sid) == 1 {
                let mut active_leg = self
                    .remove_active_leg_by_sid(&current_sid)
                    .expect("current transparent sid should have an active leg");
                active_leg
                    .owner_filter_revisions
                    .insert(req_id, view.wire_filter_revision);
                let frames = send_request(current_generation, &mut active_leg, filters);
                self.insert_active_leg(active_leg);
                return Self::place_existing(frames, Vec::new(), pass);
            }

            let removed = self.remove_owner(&req_id);
            debug_assert!(removed.is_none());
        }

        if let Some(matching_sid) = self.matching_sid_for_filters(&filters, None) {
            self.attach_owner(&matching_sid, req_id, view.wire_filter_revision);
            return Self::place_existing(Vec::new(), Vec::new(), pass);
        }

        let Some(new_pass) = pass else {
            return Self::no_room(Vec::new(), Vec::new(), pass);
        };

        tracing::debug!("Transparent took pass for {req_id:?}");
        let mut active_leg = new_transparent_leg(new_pass, req_id, view.wire_filter_revision);

        let frames = send_request(current_generation, &mut active_leg, filters);
        self.insert_active_leg(active_leg);
        TransparentSubscribeOutput {
            result: TransparentPlaceResult::Placed,
            frames,
            returned_passes: Vec::new(),
        }
    }

    /// Queue a subscription for a later transparent placement retry.
    pub(in crate::relay) fn queue_subscribe(&mut self, req_id: OutboxSubId) {
        self.queue.enqueue(req_id);
    }

    pub(in crate::relay) fn unsubscribe(
        &mut self,
        current_generation: Option<u64>,
        req_id: OutboxSubId,
    ) -> TransparentUnsubscribeOutput {
        self.unsubscribe_inner(current_generation, req_id)
    }

    fn unsubscribe_inner(
        &mut self,
        current_generation: Option<u64>,
        req_id: OutboxSubId,
    ) -> TransparentUnsubscribeOutput {
        self.queue.cancel(req_id);

        let Some(removed) = self.remove_owner(&req_id) else {
            return TransparentUnsubscribeOutput {
                frames: Vec::new(),
                returned_passes: Vec::new(),
            };
        };

        let (pass, frames) = close_active_leg(current_generation, removed);
        TransparentUnsubscribeOutput {
            frames,
            returned_passes: vec![pass],
        }
    }

    #[profiling::function]
    pub(in crate::relay) fn handle_relay_open(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
    ) -> TransparentReplayOutput {
        self.handle_relay_open_inner(current_generation, limits)
    }

    fn handle_relay_open_inner(
        &mut self,
        current_generation: Option<u64>,
        limits: ReqFilterLimits,
    ) -> TransparentReplayOutput {
        let Some(current_generation) = current_generation else {
            return TransparentReplayOutput {
                outcomes: Vec::new(),
                frames: Vec::new(),
                returned_passes: Vec::new(),
            };
        };
        let mut outcomes = Vec::new();
        let mut frames = Vec::new();
        let mut returned_passes = Vec::new();
        let request_sids = self.active_leg_by_sid.keys().cloned().collect::<Vec<_>>();
        for sid in request_sids {
            let Some(mut active_leg) = self.remove_active_leg_by_sid(&sid) else {
                continue;
            };

            if active_leg.last_enqueued_generation == Some(current_generation) {
                self.insert_active_leg(active_leg);
                continue;
            }

            let Some(filters) = limits.filters_for_single_req(&active_leg.filters) else {
                let blocked_ids = active_leg
                    .owner_filter_revisions
                    .keys()
                    .copied()
                    .collect::<Vec<_>>();
                returned_passes.push(active_leg.sub_pass);
                outcomes.extend(
                    blocked_ids
                        .into_iter()
                        .map(TransparentReplayOutcome::Blocked),
                );
                continue;
            };

            frames.extend(send_request(
                Some(current_generation),
                &mut active_leg,
                filters,
            ));
            outcomes.extend(
                active_leg
                    .owner_filter_revisions
                    .keys()
                    .copied()
                    .map(TransparentReplayOutcome::Reissued),
            );
            self.insert_active_leg(active_leg);
        }

        TransparentReplayOutput {
            outcomes,
            frames,
            returned_passes,
        }
    }
}

fn send_req(
    current_generation: Option<u64>,
    sid: &RelayReqId,
    filters: Vec<nostrdb::Filter>,
) -> (Option<u64>, Vec<QueuedRelayFrame>) {
    let Some(generation) = current_generation else {
        return (None, Vec::new());
    };
    (
        Some(generation),
        vec![(generation, ClientMessage::req(sid.to_string(), filters))],
    )
}

fn send_request(
    current_generation: Option<u64>,
    active_leg: &mut ActiveTransparentLeg,
    filters: Vec<nostrdb::Filter>,
) -> Vec<QueuedRelayFrame> {
    active_leg.status = RelayReqStatus::InitialQuery;
    active_leg.filters = filters.clone();
    let (generation, frames) = send_req(current_generation, &active_leg.sid, filters);
    active_leg.last_enqueued_generation = generation;
    frames
}

fn new_transparent_leg(
    sub_pass: SubPass,
    owner: OutboxSubId,
    wire_filter_revision: u64,
) -> ActiveTransparentLeg {
    ActiveTransparentLeg {
        sid: Uuid::new_v4().into(),
        status: RelayReqStatus::InitialQuery,
        sub_pass,
        last_enqueued_generation: None,
        filters: Vec::new(),
        owner_filter_revisions: HashMap::from([(owner, wire_filter_revision)]),
    }
}

fn close_transparent_leg(
    current_generation: Option<u64>,
    active_leg: &ActiveTransparentLeg,
) -> Vec<QueuedRelayFrame> {
    if active_leg.last_enqueued_generation != current_generation {
        return Vec::new();
    }

    let Some(generation) = current_generation else {
        return Vec::new();
    };
    vec![(generation, ClientMessage::close(active_leg.sid.to_string()))]
}

fn close_active_leg(
    current_generation: Option<u64>,
    active_leg: ActiveTransparentLeg,
) -> (SubPass, Vec<QueuedRelayFrame>) {
    let frames = close_transparent_leg(current_generation, &active_leg);
    (active_leg.sub_pass, frames)
}

/// Evicts transparent subscriptions whose passes were revoked and returns the
/// affected Outbox subscription IDs for higher-level rerouting.
pub(in crate::relay) struct TransparentRevocationOutput {
    pub(in crate::relay) revoked_ids: Vec<OutboxSubId>,
    pub(in crate::relay) frames: Vec<QueuedRelayFrame>,
}

pub(in crate::relay) fn take_revoked_transparent_subs(
    current_generation: Option<u64>,
    data: &mut TransparentData,
    targets: Vec<(OutboxSubId, SubPassRevocation)>,
) -> TransparentRevocationOutput {
    let mut revoked_ids = Vec::new();
    let mut revoked_sids = HashSet::new();
    let mut frames = Vec::new();
    for (id, mut revocation) in targets {
        data.queue.cancel(id);
        let sid = data.active_sid(&id).unwrap_or_else(|| {
            panic!("transparent revocation selected {id:?} without a live active request")
        });
        if !revoked_sids.insert(sid.clone()) {
            continue;
        }

        let removed = data.remove_active_leg_by_sid(&sid).unwrap_or_else(|| {
            panic!("transparent revocation selected {id:?} without a live active request")
        });
        revoked_ids.extend(removed.owner_filter_revisions.keys().copied());
        frames.extend(close_transparent_leg(current_generation, &removed));
        revocation.revocate(removed.sub_pass);
    }

    TransparentRevocationOutput {
        revoked_ids,
        frames,
    }
}

struct ActiveTransparentLeg {
    pub sid: RelayReqId,
    pub status: RelayReqStatus,
    pub sub_pass: SubPass,
    /// Websocket leg generation this request has already been enqueued onto.
    pub last_enqueued_generation: Option<u64>,
    /// Filter set represented by this relay subscription id.
    pub filters: Vec<nostrdb::Filter>,
    /// Outbox subscription owners sharing this relay subscription id.
    pub owner_filter_revisions: HashMap<OutboxSubId, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{
        frame::QueuedRelayFrame, FullRelayPkgsModificationTask, ModifyTask, OutboxSubscriptions,
        RelayUrlPkgs, SubPassGuardian, SubscribeTask,
    };
    use hashbrown::HashSet;
    use nostrdb::Filter;

    // ==================== TransparentData tests ====================

    fn trivial_filter() -> Vec<Filter> {
        vec![Filter::new().kinds([0]).build()]
    }

    fn kind_filter(kind: u64) -> Vec<Filter> {
        vec![Filter::new().kinds([kind]).build()]
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

    fn create_subs_with_filter(id: OutboxSubId, filters: Vec<Filter>) -> OutboxSubscriptions {
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, id, filters, false);
        subs
    }

    fn test_relay_pkgs() -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            HashSet::new(),
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::PreferDedicated,
            ),
        )
    }

    fn insert_sub(
        subs: &mut OutboxSubscriptions,
        id: OutboxSubId,
        filters: Vec<Filter>,
        is_oneshot: bool,
    ) {
        subs.new_subscription(
            id,
            SubscribeTask {
                filters,
                relays: test_relay_pkgs(),
            },
            is_oneshot,
        );
    }

    fn reissued_ids(outcomes: Vec<TransparentReplayOutcome>) -> HashSet<OutboxSubId> {
        outcomes
            .into_iter()
            .filter_map(|outcome| match outcome {
                TransparentReplayOutcome::Reissued(id) => Some(id),
                TransparentReplayOutcome::Blocked(_) => None,
            })
            .collect()
    }

    fn try_subscribe(
        data: &mut TransparentData,
        guardian: &mut SubPassGuardian,
        view: StoredSubscriptionRef,
    ) -> TransparentPlaceResult {
        try_subscribe_connected(None, &mut Vec::new(), data, guardian, view)
    }

    fn try_subscribe_connected(
        generation: Option<u64>,
        frames: &mut Vec<QueuedRelayFrame>,
        data: &mut TransparentData,
        guardian: &mut SubPassGuardian,
        view: StoredSubscriptionRef,
    ) -> TransparentPlaceResult {
        let limits = ReqFilterLimits::new(usize::MAX, usize::MAX);
        let pass = if data.pass_deficit(&view, limits).unwrap_or_default() > 0 {
            guardian.take_pass()
        } else {
            None
        };
        let output = data.try_subscribe(generation, pass, limits, view);
        return_passes(guardian, output.returned_passes);
        frames.extend(output.frames);
        output.result
    }

    fn unsubscribe(data: &mut TransparentData, guardian: &mut SubPassGuardian, id: OutboxSubId) {
        let output = data.unsubscribe(None, id);
        return_passes(guardian, output.returned_passes);
        assert!(output.frames.is_empty());
    }

    fn unsubscribe_connected(
        generation: Option<u64>,
        frames: &mut Vec<QueuedRelayFrame>,
        data: &mut TransparentData,
        guardian: &mut SubPassGuardian,
        id: OutboxSubId,
    ) {
        let output = data.unsubscribe(generation, id);
        return_passes(guardian, output.returned_passes);
        frames.extend(output.frames);
    }

    fn handle_relay_open_connected(
        generation: Option<u64>,
        frames: &mut Vec<QueuedRelayFrame>,
        data: &mut TransparentData,
        guardian: &mut SubPassGuardian,
    ) -> Vec<TransparentReplayOutcome> {
        let output =
            data.handle_relay_open(generation, ReqFilterLimits::new(usize::MAX, usize::MAX));
        return_passes(guardian, output.returned_passes);
        frames.extend(output.frames);
        output.outcomes
    }

    fn return_passes(guardian: &mut SubPassGuardian, passes: Vec<SubPass>) {
        for pass in passes {
            guardian.return_pass(pass);
        }
    }

    fn test_active_leg(
        sid: RelayReqId,
        status: RelayReqStatus,
        sub_pass: SubPass,
        owner: OutboxSubId,
    ) -> ActiveTransparentLeg {
        ActiveTransparentLeg {
            sid,
            status,
            sub_pass,
            last_enqueued_generation: None,
            filters: trivial_filter(),
            owner_filter_revisions: HashMap::from([(owner, 0)]),
        }
    }

    #[test]
    fn transparent_data_manual_insert_and_query() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();

        let req_id = OutboxSubId(42);
        let sid = RelayReqId::default();

        data.insert_active_leg(test_active_leg(
            sid.clone(),
            RelayReqStatus::InitialQuery,
            pass,
            req_id,
        ));

        assert!(data.contains(&req_id));
        assert_eq!(data.num_subs(), 1);
        assert_eq!(data.req_status(&req_id), Some(RelayReqStatus::InitialQuery));

        // Update status
        data.set_req_status(&sid.to_string(), RelayReqStatus::Eose);
        assert_eq!(data.req_status(&req_id), Some(RelayReqStatus::Eose));
    }

    #[test]
    fn transparent_data_req_status_reports_closed_leg() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let req_id = OutboxSubId(42);

        data.insert_active_leg(test_active_leg(
            RelayReqId::from("transparent-sid"),
            RelayReqStatus::Closed,
            guardian.take_pass().expect("relay pass"),
            req_id,
        ));

        assert_eq!(data.req_status(&req_id), Some(RelayReqStatus::Closed));
    }

    // ==================== TransparentData transition tests ====================

    #[test]
    fn transparent_relay_subscribe_creates_mapping() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        assert!(data.contains(&OutboxSubId(0)));
        assert_eq!(data.num_subs(), 1);
        assert_eq!(guardian.available_passes(), 4); // One pass consumed
    }

    #[test]
    fn transparent_relay_try_subscribe_reports_no_room_when_no_passes() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(0); // No passes available
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        let result = try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        assert!(matches!(result, TransparentPlaceResult::NoRoom));
        // Caller decides fallback vs retry queue.
        assert!(!data.contains(&OutboxSubId(0)));
        assert_eq!(data.num_subs(), 0);
        assert_eq!(data.queue.len(), 0);
    }

    #[test]
    fn transparent_relay_queue_subscribe_queues_when_requested() {
        let mut data = TransparentData::default();

        data.queue_subscribe(OutboxSubId(0));

        assert_eq!(data.queue.len(), 1);
    }

    #[test]
    fn transparent_relay_unsubscribe_returns_pass() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        assert_eq!(guardian.available_passes(), 0);
        assert!(data.queue.is_empty());

        unsubscribe(&mut data, &mut guardian, OutboxSubId(0));

        assert_eq!(guardian.available_passes(), 1);
        assert!(!data.contains(&OutboxSubId(0)));
        assert_eq!(data.num_subs(), 0);
        assert!(data.queue.is_empty());
    }

    #[test]
    fn transparent_relay_sub_unsub_no_passes() {
        let mut data = TransparentData::default();

        // no passes available
        let mut guardian = SubPassGuardian::new(0);

        unsubscribe(&mut data, &mut guardian, OutboxSubId(0));

        assert!(data.queue.is_empty());
    }

    #[test]
    fn transparent_relay_unsubscribe_unknown_no_op() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);

        unsubscribe(&mut data, &mut guardian, OutboxSubId(999)); // Unknown ID

        // Should not panic, passes unchanged
        assert_eq!(guardian.available_passes(), 5);
    }

    #[test]
    fn transparent_relay_subscribe_replaces_existing() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);

        let filters1 = vec![Filter::new().kinds(vec![1]).build()];
        let filters2 = vec![Filter::new().kinds(vec![4]).build()];

        let mut subs = create_subs_with_filter(OutboxSubId(0), filters1);

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        let first_sid = data
            .active_sid(&OutboxSubId(0))
            .expect("first transparent sid");
        assert_eq!(guardian.available_passes(), 4);

        subs.ingest_task(
            &OutboxSubId(0),
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: filters2,
                relays: test_relay_pkgs(),
            }),
        );

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        // Should still have same number of passes (replaced, not added)
        assert_eq!(guardian.available_passes(), 4);
        assert_eq!(data.num_subs(), 1);

        // Verify replacement happened - status should be reset to InitialQuery
        assert_eq!(
            data.req_status(&OutboxSubId(0)),
            Some(RelayReqStatus::InitialQuery)
        );
        assert_eq!(
            data.active_sid(&OutboxSubId(0))
                .expect("replacement transparent sid"),
            first_sid,
            "NIP-01 replacement should re-REQ the existing relay subscription id"
        );
    }

    #[test]
    fn transparent_relay_try_subscribe_same_wire_revision_is_no_op() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        let sid = data
            .active_sid(&OutboxSubId(0))
            .expect("active transparent sid");
        data.set_req_status(&sid.0, RelayReqStatus::Eose);

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        assert_eq!(guardian.available_passes(), 4);
        assert_eq!(
            data.active_sid(&OutboxSubId(0))
                .expect("same transparent sid"),
            sid
        );
        assert_eq!(data.req_status(&OutboxSubId(0)), Some(RelayReqStatus::Eose));
    }

    #[test]
    fn transparent_relay_try_subscribe_clears_stale_queued_retry() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        data.queue_subscribe(OutboxSubId(0));

        assert_eq!(data.queue.len(), 1);

        let placed = try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        assert!(matches!(placed, TransparentPlaceResult::Placed));

        assert!(
            data.queue.is_empty(),
            "successful placement must consume any stale queued retry"
        );
        assert!(data.contains(&OutboxSubId(0)));
    }

    #[test]
    fn transparent_relay_unsubscribe_clears_stale_queued_retry_for_active_sub() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        let placed = try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        assert!(matches!(placed, TransparentPlaceResult::Placed));
        data.queue_subscribe(OutboxSubId(0));

        assert!(data.contains(&OutboxSubId(0)));
        assert_eq!(data.queue.len(), 1);

        unsubscribe(&mut data, &mut guardian, OutboxSubId(0));

        assert!(!data.contains(&OutboxSubId(0)));
        assert!(
            data.queue.is_empty(),
            "removing a transparent sub must clear any stale queued retry"
        );
        assert_eq!(guardian.available_passes(), 1);
    }

    #[test]
    fn transparent_relay_multiple_subscriptions() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(3);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), kind_filter(0), false);
        insert_sub(&mut subs, OutboxSubId(1), kind_filter(1), false);
        insert_sub(&mut subs, OutboxSubId(2), kind_filter(2), false);

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(2)).unwrap(),
        );

        assert_eq!(data.num_subs(), 3);
        assert_eq!(guardian.available_passes(), 0);

        // All should be tracked
        assert!(data.contains(&OutboxSubId(0)));
        assert!(data.contains(&OutboxSubId(1)));
        assert!(data.contains(&OutboxSubId(2)));
    }

    #[test]
    fn transparent_relay_identical_filters_share_relay_req() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );

        let sid = data
            .active_sid(&OutboxSubId(0))
            .expect("shared transparent sid");
        assert_eq!(data.active_sid(&OutboxSubId(1)), Some(sid.clone()));
        assert_eq!(data.num_subs(), 2);
        assert_eq!(guardian.available_passes(), 1);

        data.set_req_status(&sid.to_string(), RelayReqStatus::Eose);
        assert_eq!(data.req_status(&OutboxSubId(0)), Some(RelayReqStatus::Eose));
        assert_eq!(data.req_status(&OutboxSubId(1)), Some(RelayReqStatus::Eose));
        assert_eq!(
            data.ids_for_sid(&sid).expect("shared sid ids"),
            HashSet::from([OutboxSubId(0), OutboxSubId(1)])
        );
    }

    #[test]
    fn transparent_shared_owner_filter_change_needs_new_pass() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );

        assert_eq!(guardian.available_passes(), 0);

        subs.ingest_task(
            &OutboxSubId(1),
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: kind_filter(2),
                relays: test_relay_pkgs(),
            }),
        );

        let changed = subs
            .stored_ref(&OutboxSubId(1))
            .expect("changed subscription");
        assert_eq!(
            data.pass_deficit(&changed, ReqFilterLimits::new(usize::MAX, usize::MAX)),
            Some(1)
        );
        assert!(matches!(
            data.placement_feasibility(
                &changed,
                ReqFilterLimits::new(usize::MAX, usize::MAX),
                guardian.available_passes(),
            ),
            TransparentPlacementFeasibility::NeedsCapacity { pass_deficit: 1 }
        ));
    }

    #[test]
    fn transparent_data_ids_return_shared_outbox_sub_ids() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), true);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );

        let sid = data.active_sid(&OutboxSubId(0)).unwrap();

        let outbox_ids = data.ids(&sid).expect("shared transparent ids");
        assert_eq!(outbox_ids, HashSet::from([OutboxSubId(0), OutboxSubId(1)]));

        // Unknown sid should return None
        let unknown_sid = RelayReqId::from("unknown");
        assert!(data.ids(&unknown_sid).is_none());
    }

    #[test]
    fn handle_relay_open_reports_reissued_transparent_sub_ids() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut generation = Some(0);
        let mut frames = Vec::new();

        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);

        try_subscribe_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );

        let sid0 = data
            .active_sid(&OutboxSubId(0))
            .expect("sid for first transparent sub");
        let sid1 = data
            .active_sid(&OutboxSubId(1))
            .expect("sid for second transparent sub");
        data.set_req_status(&sid0.to_string(), RelayReqStatus::Eose);
        data.set_req_status(&sid1.to_string(), RelayReqStatus::Eose);

        generation = Some(1);

        let invalidated = reissued_ids(handle_relay_open_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
        ));

        assert_eq!(
            invalidated,
            HashSet::from([OutboxSubId(0), OutboxSubId(1)]),
            "relay-open replay should invalidate every transparent REQ it reissues"
        );
        assert_eq!(
            data.req_status(&OutboxSubId(0)),
            Some(RelayReqStatus::InitialQuery),
            "relay-open replay must reset transparent req status to InitialQuery"
        );
        assert_eq!(
            data.req_status(&OutboxSubId(1)),
            Some(RelayReqStatus::InitialQuery),
            "relay-open replay must reset transparent req status to InitialQuery"
        );
    }

    #[test]
    fn transparent_subscribe_enqueues_before_open_and_initial_open_does_not_replay() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());
        let generation = Some(0);
        let mut frames = Vec::new();

        assert!(matches!(
            try_subscribe_connected(
                generation,
                &mut frames,
                &mut data,
                &mut guardian,
                subs.stored_ref(&OutboxSubId(0)).unwrap(),
            ),
            TransparentPlaceResult::Placed
        ));
        assert_eq!(frame_jsons(&frames).len(), 1);

        let outcomes =
            handle_relay_open_connected(generation, &mut frames, &mut data, &mut guardian);

        assert!(
            outcomes.is_empty(),
            "initial open must not replay a transparent req already enqueued on this websocket leg"
        );
        assert_eq!(frame_jsons(&frames).len(), 1);
    }

    #[test]
    fn transparent_shared_req_closes_after_last_owner_unsubscribes() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);
        let generation = Some(0);
        let mut frames = Vec::new();

        assert!(matches!(
            try_subscribe_connected(
                generation,
                &mut frames,
                &mut data,
                &mut guardian,
                subs.stored_ref(&OutboxSubId(0)).unwrap(),
            ),
            TransparentPlaceResult::Placed
        ));
        assert!(matches!(
            try_subscribe_connected(
                generation,
                &mut frames,
                &mut data,
                &mut guardian,
                subs.stored_ref(&OutboxSubId(1)).unwrap(),
            ),
            TransparentPlaceResult::Placed
        ));
        let captured = frame_jsons(&frames);
        assert_eq!(captured.len(), 1);
        assert!(captured[0].starts_with("[\"REQ\","));

        unsubscribe_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
            OutboxSubId(0),
        );

        assert!(data.contains(&OutboxSubId(1)));
        assert_eq!(guardian.available_passes(), 0);
        assert_eq!(frame_jsons(&frames).len(), 1);

        unsubscribe_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
            OutboxSubId(1),
        );

        let captured = frame_jsons(&frames);
        assert_eq!(captured.len(), 2);
        assert!(captured[1].starts_with("[\"CLOSE\","));
        assert_eq!(guardian.available_passes(), 1);
    }

    #[test]
    fn transparent_unsubscribe_closes_req_enqueued_before_open() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());
        let generation = Some(0);
        let mut frames = Vec::new();

        assert!(matches!(
            try_subscribe_connected(
                generation,
                &mut frames,
                &mut data,
                &mut guardian,
                subs.stored_ref(&OutboxSubId(0)).unwrap(),
            ),
            TransparentPlaceResult::Placed
        ));
        let sid = data
            .active_sid(&OutboxSubId(0))
            .expect("active transparent sid");
        let captured = frame_jsons(&frames);
        assert_eq!(captured.len(), 1);
        assert!(captured[0].starts_with("[\"REQ\","));

        unsubscribe_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
            OutboxSubId(0),
        );

        let captured = frame_jsons(&frames);
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1], format!(r#"["CLOSE","{}"]"#, sid.0));
        assert_eq!(guardian.available_passes(), 1);
        assert!(!data.contains(&OutboxSubId(0)));
    }

    #[test]
    fn transparent_reconnect_replays_once_on_new_websocket_leg() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());
        let mut generation = Some(0);
        let mut frames = Vec::new();

        assert!(matches!(
            try_subscribe_connected(
                generation,
                &mut frames,
                &mut data,
                &mut guardian,
                subs.stored_ref(&OutboxSubId(0)).unwrap(),
            ),
            TransparentPlaceResult::Placed
        ));
        assert_eq!(frame_jsons(&frames).len(), 1);

        let outcomes =
            handle_relay_open_connected(generation, &mut frames, &mut data, &mut guardian);
        assert!(
            outcomes.is_empty(),
            "initial open should not invalidate already-enqueued transparent reqs"
        );
        assert_eq!(frame_jsons(&frames).len(), 1);

        generation = Some(1);
        let invalidated = reissued_ids(handle_relay_open_connected(
            generation,
            &mut frames,
            &mut data,
            &mut guardian,
        ));

        assert_eq!(
            invalidated,
            HashSet::from([OutboxSubId(0)]),
            "reconnect must replay the active transparent req on the new websocket leg"
        );
        assert_eq!(frame_jsons(&frames).len(), 2);
    }

    // ==================== take_revoked_transparent_subs tests ====================

    #[test]
    fn take_revoked_transparent_subs_removes_subscriptions() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(3);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), kind_filter(0), false);
        insert_sub(&mut subs, OutboxSubId(1), kind_filter(1), false);
        insert_sub(&mut subs, OutboxSubId(2), kind_filter(2), false);

        // Set up some subscriptions
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(2)).unwrap(),
        );

        assert_eq!(data.num_subs(), 3);

        let revoked = take_revoked_transparent_subs(
            None,
            &mut data,
            vec![
                (OutboxSubId(0), SubPassRevocation::new()),
                (OutboxSubId(1), SubPassRevocation::new()),
            ],
        );

        // Should have removed 2 subscriptions
        assert_eq!(data.num_subs(), 1);
        assert_eq!(revoked.revoked_ids.len(), 2);
        assert_eq!(data.queue.len(), 0);
    }

    #[test]
    fn take_revoked_transparent_subs_empty_revocations() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );

        let revoked = take_revoked_transparent_subs(None, &mut data, Vec::new());

        // Nothing should change
        assert!(revoked.revoked_ids.is_empty());
        assert_eq!(data.num_subs(), 1);
    }

    #[test]
    fn take_revoked_transparent_subs_exactly_matching() {
        // Test with exactly matching number of revocations and subscriptions
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(3);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), kind_filter(0), false);
        insert_sub(&mut subs, OutboxSubId(1), kind_filter(1), false);
        insert_sub(&mut subs, OutboxSubId(2), kind_filter(2), false);

        // Create 3 subscriptions
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(0)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(1)).unwrap(),
        );
        try_subscribe(
            &mut data,
            &mut guardian,
            subs.stored_ref(&OutboxSubId(2)).unwrap(),
        );

        assert_eq!(data.num_subs(), 3);
        assert_eq!(guardian.available_passes(), 0);

        // This should revoke all subscriptions
        let revoked = take_revoked_transparent_subs(
            None,
            &mut data,
            vec![
                (OutboxSubId(0), SubPassRevocation::new()),
                (OutboxSubId(1), SubPassRevocation::new()),
                (OutboxSubId(2), SubPassRevocation::new()),
            ],
        );

        assert_eq!(data.num_subs(), 0);
        assert_eq!(revoked.revoked_ids.len(), 3);
        assert_eq!(data.queue.len(), 0);
    }
}
