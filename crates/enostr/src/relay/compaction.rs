use std::collections::HashMap;

use hashbrown::HashSet;
use nostrdb::Filter;

use crate::{
    relay::{
        websocket::WebsocketRelay, OutboxSubId, OutboxSubscriptions, QueuedTasks, RelayReqId,
        RelayReqStatus, RelayTask, SubPass, SubPassGuardian, SubPassRevocation,
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
    pub fn num_subs(&self) -> usize {
        self.relay_subs.len()
    }

    pub fn set_req_status(&mut self, sid: &str, status: RelayReqStatus) {
        let Some(data) = self.relay_subs.get_mut(sid) else {
            return;
        };

        data.status = status;
    }

    pub fn req_status(&self, id: &OutboxSubId) -> Option<RelayReqStatus> {
        let sid = self.request_to_sid.get(id)?;
        let data = self.relay_subs.get(sid)?;
        Some(data.status)
    }

    pub fn has_eose(&self, id: &OutboxSubId) -> bool {
        self.req_status(id) == Some(RelayReqStatus::Eose)
    }

    /// Returns the OutboxSubIds associated with the given relay subscription ID.
    pub fn ids(&self, sid: &RelayReqId) -> Option<&HashSet<OutboxSubId>> {
        self.relay_subs.get(sid).map(|d| &d.requests.requests)
    }
}

/// Ensures `max_subs` REQ to the websocket relay by "compacting" subscriptions (combining multiple requests into one)
pub struct CompactionRelay<'a> {
    ctx: CompactionCtx<'a>,
    sub_guardian: &'a mut SubPassGuardian,
    json_limit: usize,
}

/// CompactionRelay ensures multiple Outbox subscriptions are packed into as few
/// REQs as possible, respecting per-relay limits.
impl<'a> CompactionRelay<'a> {
    pub fn new(
        relay: Option<&'a mut WebsocketRelay>,
        data: &'a mut CompactionData,
        json_limit: usize,
        sub_guardian: &'a mut SubPassGuardian,
        subs: &'a OutboxSubscriptions,
    ) -> Self {
        let ctx = match relay {
            Some(relay) => CompactionCtx::Active(CompactionHandler::new(relay, data, subs)),
            None => CompactionCtx::Inactive {
                data,
                session: CompactionSubSession::default(),
                subs,
            },
        };
        Self {
            ctx,
            sub_guardian,
            json_limit,
        }
    }

    #[profiling::function]
    pub fn ingest_session(mut self, session: CompactionSession) {
        let request_free = session.request_free;
        let mut reserved: Vec<SubPass> = Vec::new();

        // Reserve passes - take from guardian or compact to free them
        while reserved.len() < request_free {
            if let Some(pass) = self.sub_guardian.take_pass() {
                reserved.push(pass);
            } else if let Some(ejected_pass) = self.compact() {
                reserved.push(ejected_pass);
            } else {
                break;
            }
        }

        // Process session (can't touch reserved passes)
        self.ingest_session_internal(session);

        // Drain queue
        {
            profiling::scope!("drain queue");
            loop {
                let Some(id) = self.ctx.data().queue.pop() else {
                    break;
                };
                if self.subscribe(id) == PlaceResult::Queued {
                    break;
                }
            }
        }

        // Return reserved passes
        for pass in reserved {
            self.sub_guardian.return_pass(pass);
        }
    }

    #[profiling::function]
    fn ingest_session_internal(&mut self, session: CompactionSession) {
        for (id, task) in session.tasks {
            match task {
                RelayTask::Unsubscribe => {
                    self.unsubscribe(id);
                }
                RelayTask::Subscribe => {
                    self.subscribe(id);
                }
            }
        }
    }

    #[profiling::function]
    pub fn handle_relay_open(&mut self) {
        let CompactionCtx::Active(handler) = &mut self.ctx else {
            return;
        };

        if !handler.relay.is_connected() {
            return;
        }

        for (sid, sub_data) in &handler.data.relay_subs {
            let filters = handler.subs.filters_all(&sub_data.requests.requests);
            if are_filters_empty(&filters) {
                continue;
            }

            handler
                .relay
                .conn
                .send(&ClientMessage::req(sid.to_string(), filters));
        }
    }

    pub fn revocate(&mut self, mut revocation: SubPassRevocation) {
        let Some(pass) = self.compact() else {
            // this shouldn't be possible
            return;
        };

        revocation.revocate(pass);
    }

    pub fn revocate_all(&mut self, revocations: Vec<SubPassRevocation>) {
        for revocation in revocations {
            self.revocate(revocation);
        }
    }

    #[profiling::function]
    fn compact(&mut self) -> Option<SubPass> {
        let SharedCtx {
            data,
            session,
            subs,
        } = self.ctx.shared();

        let (id, smallest) = take_smallest_sub_reqs(subs, &mut data.relay_subs)?;

        session.tasks.insert(id, SubSessionTask::Removed);
        for id in smallest.requests.requests {
            self.ctx.data().request_to_sid.remove(&id);
            self.place(id);
        }

        Some(smallest.sub_pass)
    }

    #[profiling::function]
    fn new_sub(&mut self, id: OutboxSubId) -> PlaceResult {
        let Some(new_pass) = self.sub_guardian.take_pass() else {
            // pass not available, try to place on an existing sub
            return self.place(id);
        };

        let relay_id = RelayReqId::default();
        let mut requests = SubRequests::default();
        requests.add(id);

        let SharedCtx {
            data,
            session,
            subs: _,
        } = self.ctx.shared();
        data.relay_subs.insert(
            relay_id.clone(),
            RelaySubData {
                requests,
                status: RelayReqStatus::InitialQuery,
                sub_pass: new_pass,
            },
        );
        data.request_to_sid.insert(id, relay_id.clone());
        session.tasks.insert(relay_id, SubSessionTask::New);
        PlaceResult::Placed
    }

    #[profiling::function]
    pub fn subscribe(&mut self, id: OutboxSubId) -> PlaceResult {
        let SharedCtx {
            data,
            session,
            subs: _,
        } = self.ctx.shared();
        let Some(relay_id) = data.request_to_sid.get(&id) else {
            return self.new_sub(id);
        };

        let Some(sub_data) = data.relay_subs.get_mut(relay_id) else {
            return self.new_sub(id);
        };

        // modifying a filter
        sub_data.requests.add(id);

        sub_data.status = RelayReqStatus::InitialQuery;

        session
            .tasks
            .insert(relay_id.clone(), SubSessionTask::Touched);
        tracing::debug!("Placed {id:?} on an existing subscription: {relay_id:?}");
        PlaceResult::Placed
    }

    #[profiling::function]
    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        let SharedCtx {
            data: compaction_data,
            session,
            subs: _,
        } = self.ctx.shared();
        let Some(relay_id) = compaction_data.request_to_sid.remove(&id) else {
            compaction_data.queue.add(id, RelayTask::Unsubscribe);
            return;
        };

        let Some(data) = compaction_data.relay_subs.get_mut(&relay_id) else {
            compaction_data.queue.add(id, RelayTask::Unsubscribe);
            return;
        };

        data.status = RelayReqStatus::InitialQuery;

        if !data.requests.remove(&id) {
            return;
        }

        if !data.requests.is_empty() {
            session
                .tasks
                .insert(relay_id.clone(), SubSessionTask::Touched);
            return;
        }

        let Some(data) = compaction_data.relay_subs.remove(&relay_id) else {
            return;
        };

        self.sub_guardian.return_pass(data.sub_pass);
        tracing::debug!("Unsubed from last internal id in REQ, returning pass");
        session
            .tasks
            .insert(relay_id.clone(), SubSessionTask::Removed);
    }

    #[profiling::function]
    fn place(&mut self, id: OutboxSubId) -> PlaceResult {
        let SharedCtx {
            data,
            session,
            subs,
        } = self.ctx.shared();
        let placed_on = 'place: {
            for (relay_id, relay_data) in &mut data.relay_subs {
                if !relay_data.requests.can_fit(subs, &id, self.json_limit) {
                    continue;
                }

                session
                    .tasks
                    .insert(relay_id.clone(), SubSessionTask::Touched);
                relay_data.requests.add(id);
                break 'place Some(relay_id.clone());
            }

            None
        };

        if let Some(relay_id) = placed_on {
            data.request_to_sid.insert(id, relay_id);
            return PlaceResult::Placed;
        }

        data.queue.add(id, RelayTask::Subscribe);
        PlaceResult::Queued
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlaceResult {
    Placed,
    Queued,
}

fn take_smallest_sub_reqs(
    subs: &OutboxSubscriptions,
    data: &mut HashMap<RelayReqId, RelaySubData>,
) -> Option<(RelayReqId, RelaySubData)> {
    let mut smallest = usize::MAX;
    let mut res = None;

    for (id, d) in data.iter() {
        let cur_size = subs.json_size_sum(&d.requests.requests);
        if cur_size < smallest {
            smallest = cur_size;
            res = Some(id.clone());
        }
    }

    let id = res?;

    data.remove(&id).map(|r| (id, r))
}

#[derive(Default)]
struct CompactionSubSession {
    tasks: HashMap<RelayReqId, SubSessionTask>,
}

enum SubSessionTask {
    New,
    Touched,
    Removed,
}

enum CompactionCtx<'a> {
    Active(CompactionHandler<'a>),
    Inactive {
        data: &'a mut CompactionData,
        session: CompactionSubSession,
        subs: &'a OutboxSubscriptions,
    },
}

impl<'a> CompactionCtx<'a> {
    #[profiling::function]
    pub fn shared(&mut self) -> SharedCtx<'_> {
        match self {
            CompactionCtx::Active(compaction_handler) => SharedCtx {
                data: compaction_handler.data,
                session: &mut compaction_handler.session,
                subs: compaction_handler.subs,
            },
            CompactionCtx::Inactive {
                data,
                session,
                subs,
            } => SharedCtx {
                data,
                session,
                subs,
            },
        }
    }

    pub fn data(&mut self) -> &mut CompactionData {
        match self {
            CompactionCtx::Active(compaction_handler) => compaction_handler.data,
            CompactionCtx::Inactive {
                data,
                session: _,
                subs: _,
            } => data,
        }
    }
}
struct SharedCtx<'a> {
    data: &'a mut CompactionData,
    session: &'a mut CompactionSubSession,
    subs: &'a OutboxSubscriptions,
}

struct CompactionHandler<'a> {
    relay: &'a mut WebsocketRelay,
    data: &'a mut CompactionData,
    subs: &'a OutboxSubscriptions,
    pub session: CompactionSubSession,
}

impl<'a> Drop for CompactionHandler<'a> {
    #[profiling::function]
    fn drop(&mut self) {
        for (id, task) in &self.session.tasks {
            match task {
                SubSessionTask::Touched => {
                    let Some(data) = self.data.relay_subs.get_mut(id) else {
                        continue;
                    };

                    let filters = self.subs.filters_all(&data.requests.requests);

                    if filters.is_empty() {
                        self.relay.conn.send(&ClientMessage::close(id.0.clone()));
                    } else {
                        self.relay
                            .conn
                            .send(&ClientMessage::req(id.0.clone(), filters));
                    }
                }
                SubSessionTask::Removed => {
                    self.relay.conn.send(&ClientMessage::close(id.0.clone()));
                }
                SubSessionTask::New => {
                    let Some(data) = self.data.relay_subs.get(id) else {
                        continue;
                    };

                    let filters = self.subs.filters_all(&data.requests.requests);
                    self.relay
                        .conn
                        .send(&ClientMessage::req(id.0.clone(), filters));
                }
            }
        }
    }
}

fn are_filters_empty(filters: &Vec<Filter>) -> bool {
    if filters.is_empty() {
        return true;
    }

    for filter in filters {
        if filter.num_elements() != 0 {
            return false;
        }
    }

    true
}

impl<'a> CompactionHandler<'a> {
    pub fn new(
        relay: &'a mut WebsocketRelay,
        data: &'a mut CompactionData,
        subs: &'a OutboxSubscriptions,
    ) -> Self {
        Self {
            relay,
            data,
            session: CompactionSubSession::default(),
            subs,
        }
    }
}

/// Represents a singular REQ to a relay
struct RelaySubData {
    requests: SubRequests,
    status: RelayReqStatus,
    sub_pass: SubPass,
}

#[derive(Default)]
struct SubRequests {
    pub requests: HashSet<OutboxSubId>,
}

impl SubRequests {
    #[profiling::function]
    pub fn add(&mut self, id: OutboxSubId) {
        self.requests.insert(id);
    }

    pub fn remove(&mut self, id: &OutboxSubId) -> bool {
        self.requests.remove(id)
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    pub fn can_fit(
        &self,
        subs: &OutboxSubscriptions,
        new: &OutboxSubId,
        json_limit: usize,
    ) -> bool {
        let Some(new_size) = subs.json_size(new) else {
            return true;
        };

        let cur_json_size = subs.json_size_sum(&self.requests);

        // `["REQ","abc...123"]`;
        //  12345678  ...    90 -> 10 characters excluding the UUID
        cur_json_size + new_size + 10 + RelayReqId::byte_len() <= json_limit
    }
}

#[derive(Default)]
pub struct CompactionSession {
    // Number of subs which should be free after ingestion. Subs will compact enough to free up that number of subs
    // OR as much as possible without dropping any existing subs
    request_free: usize,
    tasks: HashMap<OutboxSubId, RelayTask>,
}

impl CompactionSession {
    pub fn request_free_subs(&mut self, num_free: usize) {
        self.request_free = num_free;
    }

    pub fn unsub(&mut self, unsub: OutboxSubId) {
        self.tasks.insert(unsub, RelayTask::Unsubscribe);
    }

    pub fn sub(&mut self, id: OutboxSubId) {
        self.tasks.insert(id, RelayTask::Subscribe);
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.request_free == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{RelayUrlPkgs, SubscribeTask};
    use hashbrown::HashSet;

    // ==================== CompactionData tests ====================

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
    fn compaction_data_set_req_status_ignores_unknown_sid() {
        let mut data = CompactionData::default();
        // Should not panic or error when setting status for unknown sid
        data.set_req_status("unknown-sid", RelayReqStatus::Eose);
    }

    #[test]
    fn compaction_data_ids_returns_sub_ids() {
        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();

        let id = OutboxSubId(7);
        let relay_id = RelayReqId::from("req-123");
        let mut requests = SubRequests::default();
        requests.add(id);
        data.relay_subs.insert(
            relay_id.clone(),
            RelaySubData {
                requests,
                status: RelayReqStatus::InitialQuery,
                sub_pass: pass,
            },
        );

        let ids = data.ids(&relay_id);
        assert!(ids.is_some());
        assert!(ids.unwrap().contains(&id));
    }

    #[test]
    fn compaction_data_set_req_status_updates_status() {
        let mut data = CompactionData::default();

        // Manually set up a relay subscription
        let relay_id = RelayReqId::from("test-sid");
        let mut guardian = SubPassGuardian::new(1);
        let pass = guardian.take_pass().unwrap();

        data.relay_subs.insert(
            relay_id.clone(),
            RelaySubData {
                requests: SubRequests::default(),
                status: RelayReqStatus::InitialQuery,
                sub_pass: pass,
            },
        );

        // Set EOSE should update status
        data.set_req_status("test-sid", RelayReqStatus::Eose);

        // Verify status was set
        let sub_data = data.relay_subs.get(&relay_id).unwrap();
        assert_eq!(sub_data.status, RelayReqStatus::Eose);
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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );

        let requests = SubRequests::default();

        assert!(requests.can_fit(&subs, &OutboxSubId(0), 1_000_000));
        assert!(!requests.can_fit(&subs, &OutboxSubId(0), 5));
    }

    // ==================== CompactionSession tests ====================

    #[test]
    fn compaction_session_default() {
        let session = CompactionSession::default();
        assert_eq!(session.request_free, 0);
        assert!(session.tasks.is_empty());
    }

    #[test]
    fn compaction_session_unsub() {
        let mut session = CompactionSession::default();
        session.unsub(OutboxSubId(42));

        assert!(session.tasks.contains_key(&OutboxSubId(42)));
        match session.tasks.get(&OutboxSubId(42)) {
            Some(RelayTask::Unsubscribe) => (),
            _ => panic!("Expected Unsubscribe task"),
        }
    }

    #[test]
    fn compaction_session_sub() {
        let mut session = CompactionSession::default();
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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new()
                    .kinds(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
                    .build()],
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );

        let mut guardian = SubPassGuardian::new(2);

        // Small relay sub contains id 0
        let mut small_requests = SubRequests::default();
        small_requests.add(OutboxSubId(0));

        // Large relay sub contains id 1
        let mut large_requests = SubRequests::default();
        large_requests.add(OutboxSubId(1));

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

    // ==================== CompactionRelay tests ====================

    /// Requesting free subs when there's nothing to compact has no effect.
    #[test]
    fn compact_returns_none_when_no_subs() {
        let subs = OutboxSubscriptions::default();
        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(5);
        let json_limit = 100000;

        let initial_passes = guardian.available_passes();

        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.request_free_subs(1);
        relay.ingest_session(session);

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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new()
                    .kinds(vec![2, 3, 4, 5, 6, 7, 8, 9, 10])
                    .build()],
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(5);
        let json_limit = 100000;

        // Create 2 relay subs
        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        relay.ingest_session(session);

        assert_eq!(data.relay_subs.len(), 2);
        assert_eq!(guardian.available_passes(), 3); // 5 - 2

        // Request 4 free passes - must compact 1
        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.request_free_subs(4);
        relay.ingest_session(session);

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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );

        // Set limit so combined filters won't fit in one REQ
        let size0 = subs.json_size(&OutboxSubId(0)).unwrap();
        let size1 = subs.json_size(&OutboxSubId(1)).unwrap();
        let json_limit = size0 + size1 - 1;

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(2);

        // Create 2 relay subs at capacity
        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        relay.ingest_session(session);

        assert_eq!(data.relay_subs.len(), 2);
        assert!(data.queue.is_empty());

        // Compact 1 - redistributed request won't fit
        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.request_free_subs(1);
        relay.ingest_session(session);

        assert_eq!(data.relay_subs.len(), 1);
        assert!(!data.queue.is_empty());
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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1); // Only 1 pass
        let json_limit = 100000;

        // Add 2 requests with only 1 pass - second must go on existing
        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        relay.ingest_session(session);

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
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );
        subs.new_subscription(
            OutboxSubId(1),
            SubscribeTask {
                filters: vec![Filter::new().kinds(vec![2]).build()],
                relays: RelayUrlPkgs::new(HashSet::new()),
            },
            false,
        );

        let mut data = CompactionData::default();
        let mut guardian = SubPassGuardian::new(1); // Force second sub onto existing REQ
        let json_limit = 100000;

        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.sub(OutboxSubId(0));
        session.sub(OutboxSubId(1));
        relay.ingest_session(session);

        assert_eq!(data.relay_subs.len(), 1);
        let relay_id = data.relay_subs.keys().next().cloned().unwrap();
        assert_eq!(data.request_to_sid.get(&OutboxSubId(0)), Some(&relay_id));
        assert_eq!(data.request_to_sid.get(&OutboxSubId(1)), Some(&relay_id));

        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        session.unsub(OutboxSubId(1));
        relay.ingest_session(session);

        assert!(data.queue.is_empty());
        assert_eq!(data.relay_subs.len(), 1);
        let sub = data.relay_subs.get(&relay_id).unwrap();
        assert_eq!(sub.requests.requests.len(), 1);
        assert!(sub.requests.requests.contains(&OutboxSubId(0)));
        assert!(!sub.requests.requests.contains(&OutboxSubId(1)));
        assert_eq!(data.request_to_sid.get(&OutboxSubId(0)), Some(&relay_id));
        assert!(!data.request_to_sid.contains_key(&OutboxSubId(1)));
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
                    filters: vec![Filter::new().kinds(vec![i as u64 + 1]).build()],
                    relays: RelayUrlPkgs::new(HashSet::new()),
                },
                false,
            );
        }

        // Create 3 subs and request 2 free in same session
        let relay = CompactionRelay::new(None, &mut data, json_limit, &mut guardian, &subs);
        let mut session = CompactionSession::default();
        for i in 0..3 {
            session.sub(OutboxSubId(i));
        }
        session.request_free_subs(2);
        relay.ingest_session(session);

        // Should compact down to 1 sub with all 3 requests
        assert_eq!(data.relay_subs.len(), 1);
        assert_eq!(guardian.available_passes(), 2);

        let sub = data.relay_subs.values().next().unwrap();
        assert_eq!(sub.requests.requests.len(), 3);
    }
}
