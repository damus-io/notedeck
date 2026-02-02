use hashbrown::{hash_map::RawEntryMut, HashMap, HashSet};
use nostrdb::{Filter, Note};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    relay::{
        coordinator::{CoordinationData, CoordinationSession},
        websocket::WebsocketRelay,
        ModifyTask, MulticastRelayCache, NormRelayUrl, OutboxSubId, OutboxSubscriptions,
        OutboxTask, RawEventData, RelayId, RelayLimitations, RelayReqStatus, RelayStatus,
        RelayType,
    },
    EventClientMessage, Wakeup, WebsocketConn,
};

mod handler;
mod session;

pub use handler::OutboxSessionHandler;
pub use session::OutboxSession;

const KEEPALIVE_PING_RATE: Duration = Duration::from_secs(45);

/// OutboxPool owns the active relay coordinators and applies staged subscription
/// mutations to them each frame.
pub struct OutboxPool {
    registry: SubRegistry,
    relays: HashMap<NormRelayUrl, CoordinationData>,
    subs: OutboxSubscriptions,
    multicast: MulticastRelayCache,
}

impl Default for OutboxPool {
    fn default() -> Self {
        Self {
            registry: SubRegistry { next_request_id: 0 },
            relays: HashMap::new(),
            multicast: Default::default(),
            subs: Default::default(),
        }
    }
}

impl OutboxPool {
    #[profiling::function]
    fn ingest_session<W>(&mut self, session: OutboxSession, wakeup: &W)
    where
        W: Wakeup,
    {
        let sessions = self.collect_sessions(session);

        // Process relays with sessions
        let sessions_keys = if sessions.is_empty() {
            HashSet::new()
        } else {
            let sessions_keys: HashSet<NormRelayUrl> = sessions.keys().cloned().collect();
            self.process_sessions(sessions, wakeup);
            sessions_keys
        };

        let to_process: Vec<&mut CoordinationData> = self
            .relays
            .iter_mut()
            .filter(|(k, _)| !sessions_keys.contains(*k))
            .map(|(_, d)| d)
            .collect();

        // Also process EOSE for relays that have pending EOSE but no session tasks
        process_pending_eose(to_process, &mut self.subs);
    }

    /// Translates a session's queued tasks into per-relay coordination sessions.
    #[profiling::function]
    fn collect_sessions(
        &mut self,
        session: OutboxSession,
    ) -> HashMap<NormRelayUrl, CoordinationSession> {
        if session.tasks.is_empty() {
            return HashMap::new();
        }

        let mut sessions: HashMap<NormRelayUrl, CoordinationSession> = HashMap::new();
        'a: for (id, task) in session.tasks {
            match task {
                OutboxTask::Modify(modify) => 's: {
                    let Some(sub) = self.subs.get_mut(&id) else {
                        continue 'a;
                    };

                    match &modify {
                        ModifyTask::Filters(_) => {
                            for relay in &sub.relays {
                                get_session(&mut sessions, relay)
                                    .subscribe(id, sub.relay_type == RelayType::Transparent);
                            }
                        }
                        ModifyTask::Relays(modify_relays_task) => {
                            let relays_to_remove = sub.relays.difference(&modify_relays_task.0);
                            for relay in relays_to_remove {
                                get_session(&mut sessions, relay).unsubscribe(id);
                            }

                            let relays_to_add = modify_relays_task.0.difference(&sub.relays);
                            for relay in relays_to_add {
                                get_session(&mut sessions, relay)
                                    .subscribe(id, sub.relay_type == RelayType::Transparent);
                            }
                        }
                        ModifyTask::Full(full_modification_task) => {
                            let prev_relays = &sub.relays;
                            let new_relays = &full_modification_task.relays;
                            let relays_to_remove = prev_relays.difference(new_relays);
                            for relay in relays_to_remove {
                                get_session(&mut sessions, relay).unsubscribe(id);
                            }

                            if new_relays.is_empty() {
                                self.subs.remove(&id);
                                break 's;
                            }

                            for relay in new_relays {
                                get_session(&mut sessions, relay)
                                    .subscribe(id, sub.relay_type == RelayType::Transparent);
                            }
                        }
                    }

                    sub.ingest_task(modify);
                }
                OutboxTask::Unsubscribe => {
                    let Some(sub) = self.subs.get_mut(&id) else {
                        continue 'a;
                    };

                    for relay_id in &sub.relays {
                        get_session(&mut sessions, relay_id).unsubscribe(id);
                    }

                    self.subs.remove(&id);
                }
                OutboxTask::Oneshot(subscribe) => {
                    for relay in &subscribe.relays.urls {
                        get_session(&mut sessions, relay)
                            .subscribe(id, subscribe.relays.use_transparent);
                    }
                    self.subs.new_subscription(id, subscribe, true);
                }
                OutboxTask::Subscribe(subscribe) => {
                    for relay in &subscribe.relays.urls {
                        get_session(&mut sessions, relay)
                            .subscribe(id, subscribe.relays.use_transparent);
                    }

                    self.subs.new_subscription(id, subscribe, false);
                }
            }
        }

        sessions
    }

    /// Ensures relay coordinators exist and feed them the coordination sessions.
    #[profiling::function]
    fn process_sessions<W>(
        &mut self,
        sessions: HashMap<NormRelayUrl, CoordinationSession>,
        wakeup: &W,
    ) where
        W: Wakeup,
    {
        for (relay_id, session) in sessions {
            let relay = match self.relays.raw_entry_mut().from_key(&relay_id) {
                RawEntryMut::Occupied(e) => 's: {
                    let res = e.into_mut();

                    if res.websocket.is_some() {
                        break 's res;
                    }

                    let Ok(websocket) = WebsocketConn::from_wakeup(relay_id.into(), wakeup.clone())
                    else {
                        // still can't generate websocket
                        break 's res;
                    };

                    res.websocket = Some(WebsocketRelay::new(websocket));

                    res
                }
                RawEntryMut::Vacant(e) => {
                    let coordinator = build_relay(relay_id.clone(), wakeup.clone());
                    let (_, res) = e.insert(relay_id, coordinator);
                    res
                }
            };
            let eose_ids = relay.ingest_session(&self.subs, session);

            if !eose_ids.normal.is_empty() {
                // Update last_seen and optimize since for all subs that received EOSE
                if let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) {
                    let now = duration.as_secs();
                    for id in &eose_ids.normal {
                        if let Some(sub) = self.subs.get_mut(id) {
                            sub.see_all(now);
                            sub.filters.since_optimize();
                        }
                    }
                }
            }

            for id in &eose_ids.oneshots {
                self.subs.remove(id);
            }
        }
    }

    pub fn start_session<'a, W>(&'a mut self, wakeup: W) -> OutboxSessionHandler<'a, W>
    where
        W: Wakeup,
    {
        OutboxSessionHandler {
            outbox: self,
            session: OutboxSession::default(),
            wakeup,
        }
    }

    pub fn broadcast_note<W>(&mut self, note: &Note, relays: Vec<RelayId>, wakeup: &W)
    where
        W: Wakeup,
    {
        for relay_id in relays {
            let Ok(msg) = EventClientMessage::try_from(note) else {
                continue;
            };
            match relay_id {
                RelayId::Websocket(norm_relay_url) => {
                    let rel = self.ensure_relay(&norm_relay_url, wakeup);
                    rel.send_event(msg);
                }
                RelayId::Multicast => {
                    if !self.multicast.is_setup() {
                        self.multicast.try_setup(wakeup);
                    };

                    self.multicast.broadcast(msg);
                }
            }
        }
    }

    #[profiling::function]
    pub fn keepalive_ping(&mut self, wakeup: impl Fn() + Send + Sync + Clone + 'static) {
        for relay in self.relays.values_mut() {
            let now = Instant::now();

            let Some(websocket) = &mut relay.websocket else {
                continue;
            };

            match websocket.conn.status {
                RelayStatus::Disconnected => {
                    let reconnect_at =
                        websocket.last_connect_attempt + websocket.retry_connect_after;
                    if now > reconnect_at {
                        websocket.last_connect_attempt = now;
                        let next_duration = Duration::from_millis(3000);
                        tracing::debug!(
                            "bumping reconnect duration from {:?} to {:?} and retrying connect",
                            websocket.retry_connect_after,
                            next_duration
                        );
                        websocket.retry_connect_after = next_duration;
                        if let Err(err) = websocket.conn.connect(wakeup.clone()) {
                            tracing::error!("error connecting to relay: {}", err);
                        }
                    }
                }
                RelayStatus::Connected => {
                    websocket.retry_connect_after = WebsocketRelay::initial_reconnect_duration();

                    let should_ping = now - websocket.last_ping > KEEPALIVE_PING_RATE;
                    if should_ping {
                        tracing::trace!("pinging {}", websocket.conn.url);
                        websocket.conn.ping();
                        websocket.last_ping = Instant::now();
                    }
                }
                RelayStatus::Connecting => {}
            }
        }
    }

    fn ensure_relay<W>(&mut self, relay_id: &NormRelayUrl, wakeup: &W) -> &mut CoordinationData
    where
        W: Wakeup,
    {
        match self.relays.raw_entry_mut().from_key(relay_id) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => {
                let (_, res) = entry.insert(
                    relay_id.clone(),
                    build_relay(relay_id.clone(), wakeup.clone()),
                );
                res
            }
        }
    }

    pub fn status(&self, id: &OutboxSubId) -> HashMap<&NormRelayUrl, RelayReqStatus> {
        let mut status = HashMap::new();
        for (url, relay) in &self.relays {
            let Some(res) = relay.req_status(id) else {
                continue;
            };
            status.insert(url, res);
        }

        status
    }

    pub fn websocket_statuses(&self) -> BTreeMap<&NormRelayUrl, RelayStatus> {
        let mut status = BTreeMap::new();

        for (url, relay) in &self.relays {
            let relay_status = if let Some(websocket) = &relay.websocket {
                websocket.conn.status
            } else {
                RelayStatus::Disconnected
            };
            status.insert(url, relay_status);
        }

        status
    }

    pub fn has_eose(&self, id: &OutboxSubId) -> bool {
        for relay in self.relays.values() {
            if relay.req_status(id) == Some(RelayReqStatus::Eose) {
                return true;
            }
        }

        false
    }

    pub fn all_have_eose(&self, id: &OutboxSubId) -> bool {
        for relay in self.relays.values() {
            let Some(status) = relay.req_status(id) else {
                continue;
            };
            if status != RelayReqStatus::Eose {
                return false;
            }
        }

        true
    }

    /// Returns a clone of the filters for the given subscription ID.
    pub fn filters(&self, id: &OutboxSubId) -> Option<&Vec<Filter>> {
        self.subs.view(id).map(|v| v.filters.get_filters())
    }

    #[profiling::function]
    pub fn try_recv<F>(&mut self, max_recv: usize, mut process: F)
    where
        for<'a> F: FnMut(RawEventData<'a>),
    {
        for relay in self.relays.values_mut() {
            for _ in 0..max_recv {
                if !relay.try_recv(&self.subs, &mut process) {
                    break;
                }
            }
        }

        self.multicast.try_recv(process);
    }
}

/// Process pending EOSE for all relays, updating last_seen and since on filters.
fn process_pending_eose(to_process: Vec<&mut CoordinationData>, subs: &mut OutboxSubscriptions) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs());

    for relay in to_process {
        // Use ingest_session with empty session to process EOSE queue
        // This ensures CLOSE commands are sent for oneshots
        let eose_ids = relay.ingest_session(subs, CoordinationSession::default());

        if eose_ids.normal.is_empty() && eose_ids.oneshots.is_empty() {
            continue;
        }

        // Remove oneshot subs
        for id in &eose_ids.oneshots {
            subs.remove(id);
        }
        let Some(now) = now else {
            continue;
        };

        // Update last_seen and optimize since for normal subs
        for id in &eose_ids.normal {
            if let Some(sub) = subs.get_mut(id) {
                sub.see_all(now);
                sub.filters.since_optimize();
            }
        }
    }
}

struct SubRegistry {
    next_request_id: u64,
}

impl SubRegistry {
    pub fn next(&mut self) -> OutboxSubId {
        let i = self.next_request_id;
        self.next_request_id += 1;
        OutboxSubId(i)
    }
}

pub fn get_session<'a>(
    map: &'a mut HashMap<NormRelayUrl, CoordinationSession>,
    id: &NormRelayUrl,
) -> &'a mut CoordinationSession {
    match map.raw_entry_mut().from_key(id) {
        RawEntryMut::Occupied(e) => e.into_mut(),
        RawEntryMut::Vacant(e) => {
            let session = CoordinationSession::default();
            let (_, res) = e.insert(id.clone(), session);
            res
        }
    }
}

fn build_relay<W>(relay_id: NormRelayUrl, wakeup: W) -> CoordinationData
where
    W: Wakeup,
{
    CoordinationData::new(
        RelayLimitations::default(), // TODO(kernelkind): add actual limitations
        relay_id,
        wakeup,
    )
}

#[cfg(test)]
mod tests {
    use hashbrown::HashSet;
    use nostrdb::Filter;

    use super::*;
    use crate::relay::{
        coordinator::CoordinationTask,
        test_utils::{filters_json, trivial_filter, MockWakeup},
        RelayUrlPkgs,
    };

    /// Ensures the subscription registry always yields unique IDs.
    #[test]
    fn registry_generates_unique_ids() {
        let mut registry = SubRegistry { next_request_id: 0 };

        let id1 = registry.next();
        let id2 = registry.next();
        let id3 = registry.next();

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    // ==================== OutboxPool tests ====================

    /// Default pool has no relays or subscriptions.
    #[test]
    fn outbox_pool_default_empty() {
        let pool = OutboxPool::default();
        assert!(pool.relays.is_empty());
        // Verify no subscriptions by checking that a lookup returns empty status
        assert!(pool.status(&OutboxSubId(0)).is_empty());
    }

    /// has_eose returns false when no relays are tracking the request.
    #[test]
    fn outbox_pool_has_eose_false_when_empty() {
        let pool = OutboxPool::default();
        assert!(!pool.has_eose(&OutboxSubId(0)));
    }

    /// status() returns empty map for unknown request IDs.
    #[test]
    fn outbox_pool_status_empty_for_unknown() {
        let pool = OutboxPool::default();
        let status = pool.status(&OutboxSubId(999));
        assert!(status.is_empty());
    }

    /// websocket_statuses() is empty before any relays connect.
    #[test]
    fn outbox_pool_websocket_statuses_empty_initially() {
        let pool = OutboxPool::default();
        let statuses = pool.websocket_statuses();
        assert!(statuses.is_empty());
    }

    /// Full modifications should unsubscribe old relays and resubscribe new ones using the updated filters.
    #[test]
    fn full_modification_updates_sessions_with_new_filters() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-b.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay_a.clone());
        let new_sub_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(urls))
        };

        {
            let sub = pool
                .subs
                .get_mut(&new_sub_id)
                .expect("subscription should be registered");
            assert_eq!(sub.relays.len(), 1);
            assert!(sub.relays.contains(&relay_a));
            assert!(!sub.is_oneshot);
            assert_eq!(sub.relay_type, RelayType::Compaction);
        }

        let sessions = {
            let mut updated_relays = HashSet::new();
            updated_relays.insert(relay_b.clone());

            let mut handler = pool.start_session(wakeup);
            handler.modify_filters(
                new_sub_id,
                vec![Filter::new().kinds(vec![3]).limit(1).build()],
            );
            handler.modify_relays(new_sub_id, updated_relays);
            let session = handler.export();
            pool.collect_sessions(session)
        };

        let old_task = sessions
            .get(&relay_a)
            .and_then(|session| session.tasks.get(&new_sub_id))
            .expect("expected a task for relay relay_a");
        assert!(matches!(old_task, CoordinationTask::Unsubscribe));

        let new_task = sessions
            .get(&relay_b)
            .and_then(|session| session.tasks.get(&new_sub_id))
            .expect("expected a task for relay relay_b");
        assert!(matches!(new_task, CoordinationTask::CompactionSub));
    }

    /// Oneshot requests route to compaction mode by default.
    #[test]
    fn oneshot_routes_to_compaction() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-oneshot.example.com").unwrap();
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let filters = vec![Filter::new().kinds(vec![1]).limit(2).build()];
        let id = OutboxSubId(42);

        let mut session = OutboxSession::default();
        session.oneshot(id, filters.clone(), RelayUrlPkgs::new(relays));

        let sessions = pool.collect_sessions(session);

        let relay_task = sessions
            .get(&relay)
            .and_then(|session| session.tasks.get(&id))
            .expect("expected task for oneshot relay");
        assert!(matches!(relay_task, CoordinationTask::CompactionSub));
    }

    /// Unsubscribing from a multi-relay subscription emits unsubscribe tasks for each relay.
    #[test]
    fn unsubscribe_targets_all_relays() {
        let mut pool = OutboxPool::default();
        let relay_a = NormRelayUrl::new("wss://relay-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-b.example.com").unwrap();
        let id = OutboxSubId(42);

        // Subscribe to both relays
        let mut urls = HashSet::new();
        urls.insert(relay_a.clone());
        urls.insert(relay_b.clone());

        let mut session = OutboxSession::default();
        session.subscribe(id, trivial_filter(), RelayUrlPkgs::new(urls));
        pool.collect_sessions(session);

        // Unsubscribe
        let mut session = OutboxSession::default();
        session.unsubscribe(id);
        let sessions = pool.collect_sessions(session);

        // Both relays should receive unsubscribe tasks
        let task_a = sessions.get(&relay_a).and_then(|s| s.tasks.get(&id));
        let task_b = sessions.get(&relay_b).and_then(|s| s.tasks.get(&id));

        assert!(matches!(task_a, Some(CoordinationTask::Unsubscribe)));
        assert!(matches!(task_b, Some(CoordinationTask::Unsubscribe)));
    }

    /// Subscriptions with use_transparent=true route to transparent mode.
    #[test]
    fn subscribe_transparent_mode() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-transparent.example.com").unwrap();
        let id = OutboxSubId(5);

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let mut pkgs = RelayUrlPkgs::new(urls);
        pkgs.use_transparent = true;

        let mut session = OutboxSession::default();
        session.subscribe(id, trivial_filter(), pkgs);
        let sessions = pool.collect_sessions(session);

        let task = sessions.get(&relay).and_then(|s| s.tasks.get(&id));
        assert!(matches!(task, Some(CoordinationTask::TransparentSub)));
    }

    /// Modifying filters should re-subscribe the routed relays with the new filters.
    #[test]
    fn modify_filters_reissues_subscribe_for_existing_relays() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let sub_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(urls))
        };

        let (sessions, expected_json) = {
            let mut handler = pool.start_session(wakeup);
            let updated_filters = vec![Filter::new().kinds(vec![7]).limit(2).build()];
            let expected_json = filters_json(&updated_filters);
            handler.modify_filters(sub_id, updated_filters);
            let session = handler.export();
            (pool.collect_sessions(session), expected_json)
        };

        let view = pool.subs.view(&sub_id).expect("updated subscription view");
        let stored_json = filters_json(view.filters.get_filters());
        assert_eq!(stored_json, expected_json);

        let task = sessions
            .get(&relay)
            .and_then(|session| session.tasks.get(&sub_id))
            .expect("expected coordination task");
        assert!(matches!(task, CoordinationTask::CompactionSub));
    }

    /// Modifying relays should unsubscribe removed relays and subscribe new ones.
    #[test]
    fn modify_relays_differs_routing_sets() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-diff-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-diff-b.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay_a.clone());
        let sub_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(urls))
        };

        let sessions = {
            let mut handler = pool.start_session(wakeup);
            let mut new_urls = HashSet::new();
            new_urls.insert(relay_b.clone());
            handler.modify_relays(sub_id, new_urls);
            let session = handler.export();
            pool.collect_sessions(session)
        };

        let unsub_task = sessions
            .get(&relay_a)
            .and_then(|session| session.tasks.get(&sub_id))
            .expect("missing relay_a task");
        assert!(matches!(unsub_task, CoordinationTask::Unsubscribe));

        let sub_task = sessions
            .get(&relay_b)
            .and_then(|session| session.tasks.get(&sub_id))
            .expect("missing relay_b task");
        assert!(matches!(sub_task, CoordinationTask::CompactionSub));
    }

    /// Full modifications that end up with no relays should drop the subscription entirely.
    #[test]
    fn modify_full_with_empty_relays_removes_subscription() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-empty.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let sub_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(urls))
        };

        let sessions = {
            let mut handler = pool.start_session(wakeup);
            handler.modify_filters(sub_id, vec![Filter::new().kinds(vec![9]).limit(1).build()]);
            handler.modify_relays(sub_id, HashSet::new());
            let session = handler.export();
            pool.collect_sessions(session)
        };

        let task = sessions
            .get(&relay)
            .and_then(|session| session.tasks.get(&sub_id))
            .expect("expected unsubscribe for relay");
        assert!(matches!(task, CoordinationTask::Unsubscribe));
        assert!(
            pool.subs.get_mut(&sub_id).is_none(),
            "subscription metadata should be removed"
        );
    }

    // ==================== OutboxSessionHandler tests ====================

    /// The first subscribe issued via handler should return SubRequestId(0).
    #[test]
    fn outbox_session_handler_subscribe_returns_id() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();

        let id = {
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(HashSet::new()))
        };

        assert_eq!(id, OutboxSubId(0));
    }

    /// Separate sessions should continue incrementing subscription IDs globally.
    #[test]
    fn outbox_session_handler_multiple_subscribes_unique_ids() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();

        let id1 = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(HashSet::new()))
        };

        let id2 = {
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(HashSet::new()))
        };

        assert_ne!(id1, id2);
        assert_eq!(id1, OutboxSubId(0));
        assert_eq!(id2, OutboxSubId(1));
    }

    /// Exporting/importing a session should carry over any pending tasks intact.
    #[test]
    fn outbox_session_handler_export_and_import() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();

        // Create a handler and export its session
        let handler = pool.start_session(wakeup.clone());
        let session = handler.export();

        // Should be empty since we didn't do anything
        assert!(session.tasks.is_empty());

        // Import the session back
        let _handler = OutboxSessionHandler::import(&mut pool, session, wakeup);
    }

    // ==================== get_session tests ====================

    /// get_session should create a new coordination entry when missing.
    #[test]
    fn get_session_creates_new_if_missing() {
        let mut map: HashMap<NormRelayUrl, CoordinationSession> = HashMap::new();
        let url = NormRelayUrl::new("wss://relay.example.com").unwrap();

        let _session = get_session(&mut map, &url);

        // Should have created a new session
        assert!(map.contains_key(&url));
    }

    /// get_session returns the pre-existing coordination session.
    #[test]
    fn get_session_returns_existing() {
        let mut map: HashMap<NormRelayUrl, CoordinationSession> = HashMap::new();
        let url = NormRelayUrl::new("wss://relay.example.com").unwrap();

        let session = get_session(&mut map, &url);
        session.subscribe(OutboxSubId(0), false);

        // Map should still have exactly one entry
        assert_eq!(map.len(), 1);
    }
}
