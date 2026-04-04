use hashbrown::{HashMap, HashSet};
use uuid::Uuid;

use crate::{
    relay::{
        subscription::SubscriptionView, MetadataFilters, OutboxSubId, OutboxSubscriptions,
        QueuedTasks, RelayReqId, RelayReqStatus, SubPass, SubPassGuardian, SubPassRevocation,
        WebsocketRelay,
    },
    ClientMessage,
};

/// TransparentData tracks the outstanding transparent REQs and their metadata.
///
/// One `OutboxSubId` may be queued for retry, active on the relay, or absent.
/// It must never remain both queued and active at the same time.
#[derive(Default)]
pub struct TransparentData {
    active_legs_by_request: HashMap<OutboxSubId, ActiveTransparentLeg>,
    request_by_sid: HashMap<RelayReqId, OutboxSubId>,
    queue: QueuedTasks,
}

impl TransparentData {
    #[cfg(debug_assertions)]
    fn assert_consistent(&self) {
        debug_assert_eq!(
            self.active_legs_by_request.len(),
            self.request_by_sid.len(),
            "transparent active-leg store and reverse sid index must have matching sizes"
        );
        for (req_id, active_leg) in &self.active_legs_by_request {
            debug_assert_eq!(
                self.request_by_sid.get(&active_leg.sid),
                Some(req_id),
                "transparent reverse sid index must point back to the owning request"
            );
        }
    }

    #[cfg(test)]
    pub fn num_subs(&self) -> usize {
        self.active_legs_by_request.len()
    }

    #[cfg(test)]
    pub fn contains(&self, id: &OutboxSubId) -> bool {
        self.active_legs_by_request.contains_key(id)
    }

    pub fn request_ids(&self) -> Vec<OutboxSubId> {
        self.active_legs_by_request.keys().copied().collect()
    }

    pub fn set_req_status(&mut self, sid: &str, status: RelayReqStatus) {
        let Some(req_id) = self.request_by_sid.get(sid).copied() else {
            return;
        };
        let entry = self
            .active_legs_by_request
            .get_mut(&req_id)
            .unwrap_or_else(|| {
                panic!("transparent sid {sid} mapped to missing request {req_id:?}")
            });
        entry.status = status;
    }

    pub fn req_status(&self, req_id: &OutboxSubId) -> Option<RelayReqStatus> {
        Some(self.active_legs_by_request.get(req_id)?.status)
    }

    /// Returns the OutboxSubId associated with the given relay subscription ID.
    pub fn id(&self, sid: &RelayReqId) -> Option<OutboxSubId> {
        self.request_by_sid.get(sid).copied()
    }

    #[cfg(test)]
    /// Returns the live relay subscription ID for one active transparent leg.
    pub fn active_sid(&self, req_id: &OutboxSubId) -> Option<RelayReqId> {
        Some(self.active_legs_by_request.get(req_id)?.sid.clone())
    }

    fn active_leg_mut(
        &mut self,
        req_id: &OutboxSubId,
    ) -> Option<(RelayReqId, &mut ActiveTransparentLeg)> {
        let active = self.active_legs_by_request.get_mut(req_id)?;
        Some((active.sid.clone(), active))
    }

    fn insert_active_leg(&mut self, req_id: OutboxSubId, active_leg: ActiveTransparentLeg) {
        let old_sid = self.request_by_sid.insert(active_leg.sid.clone(), req_id);
        debug_assert!(
            old_sid.is_none(),
            "transparent request_by_sid must not overwrite an existing sid"
        );
        let old_active = self.active_legs_by_request.insert(req_id, active_leg);
        debug_assert!(
            old_active.is_none(),
            "transparent active_legs_by_request must not overwrite an existing request"
        );
        #[cfg(debug_assertions)]
        self.assert_consistent();
    }

    fn remove_active_leg(&mut self, req_id: &OutboxSubId) -> Option<ActiveTransparentLeg> {
        let removed = self.active_legs_by_request.remove(req_id)?;
        let removed_req = self.request_by_sid.remove(&removed.sid);
        debug_assert_eq!(
            removed_req,
            Some(*req_id),
            "transparent reverse sid index must match removed request"
        );
        #[cfg(debug_assertions)]
        self.assert_consistent();
        Some(removed)
    }

    fn iter_active_legs_mut(
        &mut self,
    ) -> impl Iterator<Item = (OutboxSubId, &mut ActiveTransparentLeg)> {
        self.active_legs_by_request
            .iter_mut()
            .map(|(req_id, active_leg)| (*req_id, active_leg))
    }

    #[cfg(test)]
    pub(crate) fn queued_len_for_test(&self) -> usize {
        self.queue.len()
    }
}

pub struct TransparentRelay<'a> {
    relay: Option<&'a mut WebsocketRelay>,
    data: &'a mut TransparentData,
    sub_guardian: &'a mut SubPassGuardian,
}

/// Result of trying to place a subscription onto the transparent relay path.
pub enum TransparentPlaceResult {
    Placed,
    NoRoom,
}

/// TransparentRelay manages per-subscription REQs for outbox subscriptions which
/// need to get EOSE ASAP (or some other need)
impl<'a> TransparentRelay<'a> {
    pub fn new(
        relay: Option<&'a mut WebsocketRelay>,
        data: &'a mut TransparentData,
        sub_guardian: &'a mut SubPassGuardian,
    ) -> Self {
        Self {
            relay,
            data,
            sub_guardian,
        }
    }

    /// Tries queued transparent subscribes and returns IDs that were placed.
    pub fn try_flush_queue(&mut self, subs: &OutboxSubscriptions) -> Vec<OutboxSubId> {
        let mut placed = Vec::new();
        while self.sub_guardian.available_passes() > 0 && !self.data.queue.is_empty() {
            let Some(next) = self.data.queue.pop() else {
                return placed;
            };

            let Some(view) = subs.view(&next) else {
                continue;
            };

            if let TransparentPlaceResult::NoRoom = self.try_subscribe(view) {
                self.queue_subscribe(next);
                break;
            }
            placed.push(next);
        }
        placed
    }

    /// Try to place this subscription on transparent without mutating the retry queue.
    pub fn try_subscribe(&mut self, view: SubscriptionView) -> TransparentPlaceResult {
        let req_id = view.id;
        self.data.queue.cancel(req_id);

        if let Some((existing_sid, active_leg)) = self.data.active_leg_mut(&req_id) {
            active_leg.status = RelayReqStatus::InitialQuery;
            send_req(&mut self.relay, &existing_sid, view.filters);
            return TransparentPlaceResult::Placed;
        }

        let Some(new_pass) = self.sub_guardian.take_pass() else {
            return TransparentPlaceResult::NoRoom;
        };
        tracing::debug!("Transparent took pass for {req_id:?}");
        let sid: RelayReqId = Uuid::new_v4().into();
        send_req(&mut self.relay, &sid, view.filters);
        self.data.insert_active_leg(
            req_id,
            ActiveTransparentLeg {
                sid,
                status: RelayReqStatus::InitialQuery,
                sub_pass: new_pass,
            },
        );
        TransparentPlaceResult::Placed
    }

    /// Queue a subscription for a later transparent placement retry.
    pub fn queue_subscribe(&mut self, req_id: OutboxSubId) {
        self.data.queue.enqueue(req_id);
    }

    pub fn unsubscribe(&mut self, req_id: OutboxSubId) {
        self.data.queue.cancel(req_id);

        let Some(removed) = self.data.remove_active_leg(&req_id) else {
            return;
        };

        self.sub_guardian.return_pass(removed.sub_pass);

        let Some(relay) = &mut self.relay else {
            return;
        };

        if relay.is_connected() {
            relay
                .conn
                .send(&ClientMessage::close(removed.sid.to_string()));
        }
    }

    #[profiling::function]
    pub fn handle_relay_open(&mut self, subs: &OutboxSubscriptions) -> HashSet<OutboxSubId> {
        let Some(relay) = &mut self.relay else {
            return HashSet::new();
        };

        if !relay.is_connected() {
            return HashSet::new();
        }

        let mut invalidated = HashSet::new();
        for (req_id, active_leg) in self.data.iter_active_legs_mut() {
            let Some(view) = subs.view(&req_id) else {
                continue;
            };

            active_leg.status = RelayReqStatus::InitialQuery;
            relay.conn.send(&ClientMessage::req(
                active_leg.sid.to_string(),
                view.filters.get_filters().clone(),
            ));
            invalidated.insert(req_id);
        }

        invalidated
    }
}

fn send_req(relay: &mut Option<&mut WebsocketRelay>, sid: &RelayReqId, filters: &MetadataFilters) {
    let Some(relay) = relay.as_mut() else {
        return;
    };

    if !relay.is_connected() {
        return;
    }

    relay.conn.send(&ClientMessage::req(
        sid.to_string(),
        filters.get_filters().clone(),
    ));
}

/// Evicts transparent subscriptions whose passes were revoked and returns the
/// affected Outbox subscription IDs for higher-level rerouting.
pub fn take_revoked_transparent_subs(
    mut relay: Option<&mut WebsocketRelay>,
    data: &mut TransparentData,
    ids: Vec<OutboxSubId>,
    revocations: Vec<SubPassRevocation>,
) -> Vec<OutboxSubId> {
    let mut revoked_ids = Vec::with_capacity(ids.len());
    for (id, mut revocation) in ids.into_iter().zip(revocations) {
        data.queue.cancel(id);
        let removed = data.remove_active_leg(&id).unwrap_or_else(|| {
            panic!("transparent revocation selected {id:?} without a live active request")
        });

        revoked_ids.push(id);
        revocation.revocate(removed.sub_pass);

        let Some(relay) = &mut relay else {
            continue;
        };
        if relay.is_connected() {
            relay
                .conn
                .send(&ClientMessage::close(removed.sid.to_string()));
        }
    }

    revoked_ids
}

struct ActiveTransparentLeg {
    pub sid: RelayReqId,
    pub status: RelayReqStatus,
    pub sub_pass: SubPass,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        relay::{test_utils::MockWakeup, RelayStatus, RelayUrlPkgs, SubscribeTask},
        WebsocketConn,
    };
    use hashbrown::HashSet;
    use nostrdb::Filter;

    // ==================== TransparentData tests ====================

    fn trivial_filter() -> Vec<Filter> {
        vec![Filter::new().kinds([0]).build()]
    }

    fn create_subs_with_filter(id: OutboxSubId, filters: Vec<Filter>) -> OutboxSubscriptions {
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, id, filters, false);
        subs
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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            is_oneshot,
        );
    }

    #[test]
    fn transparent_data_manual_insert_and_query() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();

        let req_id = OutboxSubId(42);
        let sid = RelayReqId::default();

        data.insert_active_leg(
            req_id,
            ActiveTransparentLeg {
                sid: sid.clone(),
                status: RelayReqStatus::InitialQuery,
                sub_pass: pass,
            },
        );

        assert!(data.contains(&req_id));
        assert_eq!(data.num_subs(), 1);
        assert_eq!(data.req_status(&req_id), Some(RelayReqStatus::InitialQuery));

        // Update status
        data.set_req_status(&sid.to_string(), RelayReqStatus::Eose);
        assert_eq!(data.req_status(&req_id), Some(RelayReqStatus::Eose));
    }

    // ==================== TransparentRelay tests ====================

    #[test]
    fn transparent_relay_subscribe_creates_mapping() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
        }

        assert!(data.contains(&OutboxSubId(0)));
        assert_eq!(data.num_subs(), 1);
        assert_eq!(guardian.available_passes(), 4); // One pass consumed
    }

    #[test]
    fn transparent_relay_try_subscribe_reports_no_room_when_no_passes() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(0); // No passes available
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        let result = {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap())
        };

        assert!(matches!(result, TransparentPlaceResult::NoRoom));
        // Caller decides fallback vs retry queue.
        assert!(!data.contains(&OutboxSubId(0)));
        assert_eq!(data.num_subs(), 0);
        assert_eq!(data.queue.len(), 0);
    }

    #[test]
    fn transparent_relay_queue_subscribe_queues_when_requested() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(0);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.queue_subscribe(OutboxSubId(0));
        }

        assert_eq!(data.queue.len(), 1);
    }

    #[test]
    fn transparent_relay_unsubscribe_returns_pass() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
        }

        assert_eq!(guardian.available_passes(), 0);
        assert!(data.queue.is_empty());

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.unsubscribe(OutboxSubId(0));
        }

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

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.unsubscribe(OutboxSubId(0));
        }

        assert!(data.queue.is_empty());
    }

    #[test]
    fn transparent_relay_unsubscribe_unknown_no_op() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.unsubscribe(OutboxSubId(999)); // Unknown ID
        }

        // Should not panic, passes unchanged
        assert_eq!(guardian.available_passes(), 5);
    }

    #[test]
    fn transparent_relay_subscribe_replaces_existing() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(5);

        let filters1 = vec![Filter::new().kinds(vec![1]).build()];
        let filters2 = vec![Filter::new().kinds(vec![4]).build()];

        let subs1 = create_subs_with_filter(OutboxSubId(0), filters1);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs1.view(&OutboxSubId(0)).unwrap());
        }

        assert_eq!(guardian.available_passes(), 4);

        let subs2 = create_subs_with_filter(OutboxSubId(0), filters2);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs2.view(&OutboxSubId(0)).unwrap());
        }

        // Should still have same number of passes (replaced, not added)
        assert_eq!(guardian.available_passes(), 4);
        assert_eq!(data.num_subs(), 1);

        // Verify replacement happened - status should be reset to InitialQuery
        assert_eq!(
            data.req_status(&OutboxSubId(0)),
            Some(RelayReqStatus::InitialQuery)
        );
    }

    #[test]
    fn transparent_relay_try_flush_queue_processes_when_passes_available() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(0); // Start with no passes
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        // Queue a subscription
        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.queue_subscribe(OutboxSubId(0));
        }

        assert_eq!(data.queue.len(), 1);
        assert!(!data.contains(&OutboxSubId(0)));

        // Return a pass
        guardian.spawn_passes(1);

        // Flush queue
        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            let placed = relay.try_flush_queue(&subs);
            assert_eq!(placed, vec![OutboxSubId(0)]);
        }

        // Should now be active
        assert!(data.queue.is_empty());
        assert!(data.contains(&OutboxSubId(0)));
    }

    #[test]
    fn transparent_relay_try_subscribe_clears_stale_queued_retry() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(1);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.queue_subscribe(OutboxSubId(0));
        }

        assert_eq!(data.queue.len(), 1);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            let placed = relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            assert!(matches!(placed, TransparentPlaceResult::Placed));
        }

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

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            let placed = relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            assert!(matches!(placed, TransparentPlaceResult::Placed));
            relay.queue_subscribe(OutboxSubId(0));
        }

        assert!(data.contains(&OutboxSubId(0)));
        assert_eq!(data.queue.len(), 1);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.unsubscribe(OutboxSubId(0));
        }

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
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(2), trivial_filter(), false);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(1)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(2)).unwrap());
        }

        assert_eq!(data.num_subs(), 3);
        assert_eq!(guardian.available_passes(), 0);

        // All should be tracked
        assert!(data.contains(&OutboxSubId(0)));
        assert!(data.contains(&OutboxSubId(1)));
        assert!(data.contains(&OutboxSubId(2)));
    }

    #[test]
    fn transparent_data_id_returns_outbox_sub_id() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), true);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(1)).unwrap());
        }

        let sid = data.active_sid(&OutboxSubId(0)).unwrap();

        // id() should return the OutboxSubId for the relay subscription
        let outbox_id = data.id(&sid);
        assert_eq!(outbox_id, Some(OutboxSubId(0)));

        // Unknown sid should return None
        let unknown_sid = RelayReqId::from("unknown");
        assert!(data.id(&unknown_sid).is_none());
    }

    #[test]
    fn handle_relay_open_reports_reissued_transparent_sub_ids() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let mut websocket = WebsocketRelay::new(
            WebsocketConn::from_wakeup(
                nostr::RelayUrl::parse("wss://transparent-replay.example.com").unwrap(),
                MockWakeup::default(),
            )
            .unwrap(),
        );
        websocket.conn.set_status(RelayStatus::Connected);

        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);

        {
            let mut relay = TransparentRelay::new(Some(&mut websocket), &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(1)).unwrap());
        }

        let sid0 = data
            .active_sid(&OutboxSubId(0))
            .expect("sid for first transparent sub");
        let sid1 = data
            .active_sid(&OutboxSubId(1))
            .expect("sid for second transparent sub");
        data.set_req_status(&sid0.to_string(), RelayReqStatus::Eose);
        data.set_req_status(&sid1.to_string(), RelayReqStatus::Eose);

        let invalidated = {
            let mut relay = TransparentRelay::new(Some(&mut websocket), &mut data, &mut guardian);
            relay.handle_relay_open(&subs)
        };

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

    // ==================== take_revoked_transparent_subs tests ====================

    #[test]
    fn take_revoked_transparent_subs_removes_subscriptions() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(3);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(2), trivial_filter(), false);

        // Set up some subscriptions
        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(1)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(2)).unwrap());
        }

        assert_eq!(data.num_subs(), 3);

        // Create revocations for 2 subs
        let revocations = vec![SubPassRevocation::new(), SubPassRevocation::new()];

        let revoked = take_revoked_transparent_subs(
            None,
            &mut data,
            vec![OutboxSubId(0), OutboxSubId(1)],
            revocations,
        );

        // Should have removed 2 subscriptions
        assert_eq!(data.num_subs(), 1);
        assert_eq!(revoked.len(), 2);
        assert_eq!(data.queue.len(), 0);
    }

    #[test]
    fn take_revoked_transparent_subs_empty_revocations() {
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(2);
        let subs = create_subs_with_filter(OutboxSubId(0), trivial_filter());

        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
        }

        // No revocations
        let revocations: Vec<SubPassRevocation> = vec![];
        let revoked = take_revoked_transparent_subs(None, &mut data, Vec::new(), revocations);

        // Nothing should change
        assert!(revoked.is_empty());
        assert_eq!(data.num_subs(), 1);
    }

    #[test]
    fn take_revoked_transparent_subs_exactly_matching() {
        // Test with exactly matching number of revocations and subscriptions
        let mut data = TransparentData::default();
        let mut guardian = SubPassGuardian::new(3);
        let mut subs = OutboxSubscriptions::default();
        insert_sub(&mut subs, OutboxSubId(0), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(1), trivial_filter(), false);
        insert_sub(&mut subs, OutboxSubId(2), trivial_filter(), false);

        // Create 3 subscriptions
        {
            let mut relay = TransparentRelay::new(None, &mut data, &mut guardian);
            relay.try_subscribe(subs.view(&OutboxSubId(0)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(1)).unwrap());
            relay.try_subscribe(subs.view(&OutboxSubId(2)).unwrap());
        }

        assert_eq!(data.num_subs(), 3);
        assert_eq!(guardian.available_passes(), 0);

        // Create exactly 3 revocations
        let revocations = vec![
            SubPassRevocation::new(),
            SubPassRevocation::new(),
            SubPassRevocation::new(),
        ];

        // This should revoke all subscriptions
        let revoked = take_revoked_transparent_subs(
            None,
            &mut data,
            vec![OutboxSubId(0), OutboxSubId(1), OutboxSubId(2)],
            revocations,
        );

        assert_eq!(data.num_subs(), 0);
        assert_eq!(revoked.len(), 3);
        assert_eq!(data.queue.len(), 0);
    }
}
