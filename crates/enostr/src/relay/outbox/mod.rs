use hashbrown::{hash_map::RawEntryMut, HashMap, HashSet};
use nostrdb::{Filter, Note};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    relay::{
        backoff,
        coordinator::{CoordinationData, CoordinationSession, EoseIds},
        websocket::WebsocketRelay,
        ModifyTask, MulticastRelayCache, Nip11ApplyOutcome, Nip11FetchRequest, Nip11LimitationsRaw,
        NormRelayUrl, OutboxSubId, OutboxSubscriptions, OutboxTask, RawEventData, RelayId,
        RelayLimitations, RelayReqStatus, RelayStatus,
    },
    EventClientMessage, Wakeup, WebsocketConn,
};

mod handler;
mod session;

pub use handler::OutboxSessionHandler;
pub use session::OutboxSession;

const KEEPALIVE_PING_RATE: Duration = Duration::from_secs(45);
const PONG_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30 * 60); // 30 minutes
const NIP11_REFRESH_AFTER_SUCCESS: Duration = Duration::from_secs(60 * 60);

/// OutboxPool owns the active relay coordinators and applies staged subscription
/// mutations to them each frame.
pub struct OutboxPool {
    registry: SubRegistry,
    relays: HashMap<NormRelayUrl, CoordinationData>,
    subs: OutboxSubscriptions,
    multicast: MulticastRelayCache,
    pong_timeout: Duration,
}

impl Default for OutboxPool {
    fn default() -> Self {
        Self {
            registry: SubRegistry { next_request_id: 0 },
            relays: HashMap::new(),
            multicast: Default::default(),
            subs: Default::default(),
            pong_timeout: PONG_TIMEOUT,
        }
    }
}

impl OutboxPool {
    /// Overrides the maximum allowed time since the last websocket pong before
    /// a connected relay is marked disconnected by keepalive checks.
    pub fn set_pong_timeout(&mut self, timeout: Duration) {
        self.pong_timeout = timeout;
    }

    fn remove_completed_oneshots(&mut self, ids: HashSet<OutboxSubId>) {
        for id in ids {
            if self.all_have_eose(&id) {
                self.subs.remove(&id);
            }
        }
    }

    #[profiling::function]
    fn ingest_session<W>(&mut self, session: OutboxSession, wakeup: &W)
    where
        W: Wakeup,
    {
        let sessions = self.collect_sessions(session);
        let mut pending_eose_ids = EoseIds::default();

        // Process relays with sessions
        let sessions_keys = if sessions.is_empty() {
            HashSet::new()
        } else {
            let sessions_keys: HashSet<NormRelayUrl> = sessions.keys().cloned().collect();
            let session_eose_ids = self.process_sessions(sessions, wakeup);
            pending_eose_ids.absorb(session_eose_ids);
            sessions_keys
        };

        // Also process EOSE for relays that have pending EOSE but no session
        // tasks. We only remove oneshots after all relay legs have reached EOSE.
        let mut eose_state = EoseState {
            relays: &mut self.relays,
            subs: &mut self.subs,
        };
        let extra_eose_ids =
            process_pending_eose_for_non_session_relays(&mut eose_state, &sessions_keys);
        pending_eose_ids.absorb(extra_eose_ids);

        optimize_since_for_fully_eosed_subs(&mut eose_state, pending_eose_ids.normal);
        self.remove_completed_oneshots(pending_eose_ids.oneshots);
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
                                    .subscribe(id, sub.routing_preference);
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
                                    .subscribe(id, sub.routing_preference);
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
                                    .subscribe(id, sub.routing_preference);
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
                            .subscribe(id, subscribe.relays.routing_preference);
                    }
                    self.subs.new_subscription(id, subscribe, true);
                }
                OutboxTask::Subscribe(subscribe) => {
                    for relay in &subscribe.relays.urls {
                        get_session(&mut sessions, relay)
                            .subscribe(id, subscribe.relays.routing_preference);
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
    ) -> EoseIds
    where
        W: Wakeup,
    {
        let mut pending_eoses = EoseIds::default();
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

            pending_eoses.absorb(eose_ids);
        }

        pending_eoses
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
                        websocket.reconnect_attempt = websocket.reconnect_attempt.saturating_add(1);
                        let jitter_seed =
                            backoff::jitter_seed(&websocket.conn.url, websocket.reconnect_attempt);
                        let next_duration = backoff::next_duration(
                            websocket.reconnect_attempt,
                            jitter_seed,
                            MAX_RECONNECT_DELAY,
                        );
                        tracing::debug!(
                            "reconnect attempt {}, backing off for {:?}",
                            websocket.reconnect_attempt,
                            next_duration,
                        );
                        websocket.retry_connect_after = next_duration;
                        if let Err(err) = websocket.conn.connect(wakeup.clone()) {
                            tracing::error!("error connecting to relay: {}", err);
                        }
                    }
                }
                RelayStatus::Connected => {
                    websocket.reconnect_attempt = 0;
                    websocket.retry_connect_after = WebsocketRelay::initial_reconnect_duration();

                    // Detect stale connections: if we've been pinging but no
                    // pong has come back within PONG_TIMEOUT, the connection
                    // is silently dead (e.g. laptop sleep, NAT timeout).
                    if now - websocket.last_pong > self.pong_timeout {
                        tracing::warn!(
                            "pong timeout on {}, marking disconnected",
                            websocket.conn.url
                        );
                        websocket.conn.set_status(RelayStatus::Disconnected);
                        continue;
                    }

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

    /// Drain relays that are ready for a NIP-11 fetch request.
    pub fn take_nip11_fetch_requests(
        &mut self,
        max: usize,
        now: SystemTime,
    ) -> Vec<Nip11FetchRequest> {
        let mut requests = Vec::new();
        if max == 0 {
            return requests;
        }

        for (relay_url, relay) in &mut self.relays {
            if requests.len() >= max {
                break;
            }

            if !relay.nip11.ready_to_fetch(now) {
                continue;
            }

            let attempt = relay.nip11.mark_dispatched();
            tracing::debug!("nip11: fetching {relay_url}");
            requests.push(Nip11FetchRequest {
                relay: relay_url.clone(),
                attempt,
                requested_at: now,
            });
        }

        requests
    }

    /// Convert raw NIP-11 limitations and apply relevant values for one relay.
    pub fn apply_nip11_limits(
        &mut self,
        relay: &NormRelayUrl,
        raw: Nip11LimitationsRaw,
        fetched_at: SystemTime,
    ) -> Nip11ApplyOutcome {
        let Some(coord) = self.relays.get_mut(relay) else {
            return Nip11ApplyOutcome::RelayUnknown;
        };

        let current = coord.current_limits();
        let derived = derive_relay_limitations_from_raw(&raw, current);
        coord
            .nip11
            .mark_success(fetched_at, NIP11_REFRESH_AFTER_SUCCESS);

        if derived == current {
            tracing::debug!("nip11: {relay} limits unchanged");
            return Nip11ApplyOutcome::Unchanged;
        }

        coord.set_limits(&self.subs, derived);
        tracing::info!(
            "nip11: {relay} limits updated — max_subs: {} -> {}, max_json_bytes: {} -> {}",
            current.maximum_subs,
            derived.maximum_subs,
            current.max_json_bytes,
            derived.max_json_bytes,
        );
        Nip11ApplyOutcome::Applied
    }

    /// Record a failed NIP-11 fetch so the relay can be retried later.
    pub fn record_nip11_failure(
        &mut self,
        relay: &NormRelayUrl,
        error: String,
        failed_at: SystemTime,
    ) {
        let Some(coord) = self.relays.get_mut(relay) else {
            return;
        };

        let attempt = coord.nip11.attempt();
        let jitter_seed = backoff::jitter_seed(relay, attempt);
        let retry_after = backoff::next_duration(attempt, jitter_seed, MAX_RECONNECT_DELAY);
        tracing::warn!("nip11: {relay} fetch failed: {error} (retry in {retry_after:?})");
        coord.nip11.mark_failure(failed_at, error, retry_after);
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
    pub fn try_recv<F>(&mut self, mut max_notes: usize, mut process: F)
    where
        for<'a> F: FnMut(RawEventData<'a>),
    {
        's: while max_notes > 0 {
            let mut received_any = false;

            for relay in self.relays.values_mut() {
                let resp = relay.try_recv(&self.subs, &mut process);

                if !resp.received_event {
                    continue;
                }

                received_any = true;

                if resp.event_was_nostr_note {
                    max_notes = max_notes.saturating_sub(1);
                    if max_notes == 0 {
                        break 's;
                    }
                }
            }

            if !received_any {
                break;
            }
        }

        self.multicast.try_recv(process);
    }
}

struct EoseState<'a> {
    relays: &'a mut HashMap<NormRelayUrl, CoordinationData>,
    subs: &'a mut OutboxSubscriptions,
}

fn unix_now_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn sub_all_relays_have_eose(state: &EoseState<'_>, id: &OutboxSubId) -> bool {
    let Some(sub) = state.subs.get(id) else {
        return false;
    };
    if sub.relays.is_empty() {
        return false;
    }

    for relay_id in &sub.relays {
        let Some(relay) = state.relays.get(relay_id) else {
            return false;
        };
        if relay.req_status(id) != Some(RelayReqStatus::Eose) {
            return false;
        }
    }

    true
}

fn optimize_since_for_fully_eosed_subs(state: &mut EoseState<'_>, ids: HashSet<OutboxSubId>) {
    let Some(now) = unix_now_secs() else {
        return;
    };

    for id in ids {
        // Since optimization is only safe after every relay leg for this
        // subscription has reached EOSE at least once.
        if !sub_all_relays_have_eose(state, &id) {
            continue;
        }

        if let Some(sub) = state.subs.get_mut(&id) {
            sub.see_all(now);
            sub.filters.since_optimize();
        }
    }
}

fn process_pending_eose_for_non_session_relays(
    state: &mut EoseState<'_>,
    sessions_keys: &HashSet<NormRelayUrl>,
) -> EoseIds {
    let mut pending_eoses = EoseIds::default();

    for (relay_id, relay) in state.relays.iter_mut() {
        if sessions_keys.contains(relay_id) {
            continue;
        }

        let eose_ids = relay.ingest_session(state.subs, CoordinationSession::default());
        pending_eoses.absorb(eose_ids);
    }

    pending_eoses
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

fn derive_relay_limitations_from_raw(
    raw: &Nip11LimitationsRaw,
    fallback: RelayLimitations,
) -> RelayLimitations {
    let mut out = fallback;

    if let Some(maximum_subs) = raw.max_subscriptions.and_then(valid_positive_usize) {
        out.maximum_subs = maximum_subs;
    }

    if let Some(max_json_bytes) = raw.max_message_length.and_then(valid_positive_usize) {
        out.max_json_bytes = max_json_bytes;
    }

    out
}

fn valid_positive_usize(value: i64) -> Option<usize> {
    if value <= 0 {
        return None;
    }

    usize::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use hashbrown::HashSet;
    use nostrdb::Filter;

    use super::*;
    use crate::relay::{
        coordinator::CoordinationTask,
        test_utils::{filters_json, trivial_filter, MockWakeup},
        RelayRoutingPreference, RelayUrlPkgs,
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

    #[test]
    fn derive_relay_limitations_uses_positive_raw_values() {
        let fallback = RelayLimitations {
            maximum_subs: 10,
            max_json_bytes: 200_000,
        };
        let raw = Nip11LimitationsRaw {
            max_subscriptions: Some(300),
            max_message_length: Some(131_072),
            ..Default::default()
        };

        let derived = derive_relay_limitations_from_raw(&raw, fallback);
        assert_eq!(derived.maximum_subs, 300);
        assert_eq!(derived.max_json_bytes, 131_072);
    }

    #[test]
    fn derive_relay_limitations_ignores_invalid_values() {
        let fallback = RelayLimitations {
            maximum_subs: 10,
            max_json_bytes: 200_000,
        };
        let raw = Nip11LimitationsRaw {
            max_subscriptions: Some(0),
            max_message_length: Some(-1),
            ..Default::default()
        };

        let derived = derive_relay_limitations_from_raw(&raw, fallback);
        assert_eq!(derived.maximum_subs, fallback.maximum_subs);
        assert_eq!(derived.max_json_bytes, fallback.max_json_bytes);
    }

    /// Ensures NIP-11 fetch requests respect in-flight and retry timing lifecycle gates.
    #[test]
    fn take_nip11_fetch_requests_respects_lifecycle_and_retry_schedule() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-nip11-gating.example.com").unwrap();
        let _ = pool.ensure_relay(&relay, &wakeup);

        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let requests = pool.take_nip11_fetch_requests(1, now);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].relay, relay);
        assert_eq!(requests[0].attempt, 1);

        let immediate = pool.take_nip11_fetch_requests(1, now);
        assert!(
            immediate.is_empty(),
            "in-flight relay should not be re-dispatched immediately"
        );

        pool.record_nip11_failure(&relay, "boom".to_owned(), now);

        // attempt 1 base = 10s; should not be ready before base delay
        let base = backoff::base_delay(1, MAX_RECONNECT_DELAY);
        let before_retry = now
            .checked_add(base - Duration::from_secs(1))
            .expect("retry check timestamp");
        let not_ready = pool.take_nip11_fetch_requests(1, before_retry);
        assert!(
            not_ready.is_empty(),
            "relay should remain blocked until retry deadline"
        );

        // should be ready after base + max jitter (25%)
        let after_jitter = now.checked_add(base + base / 4).expect("retry timestamp");
        let retry_ready = pool.take_nip11_fetch_requests(1, after_jitter);
        assert_eq!(retry_ready.len(), 1);
        assert_eq!(retry_ready[0].relay, relay);
        assert_eq!(retry_ready[0].attempt, 2);
    }

    /// Ensures applying NIP-11 limits reports all outcomes and refresh scheduling is enforced.
    #[test]
    fn apply_nip11_limits_reports_outcomes_and_updates_state() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let known = NormRelayUrl::new("wss://relay-nip11-known.example.com").unwrap();
        let unknown = NormRelayUrl::new("wss://relay-nip11-unknown.example.com").unwrap();
        let _ = pool.ensure_relay(&known, &wakeup);

        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_100);

        let unknown_outcome =
            pool.apply_nip11_limits(&unknown, Nip11LimitationsRaw::default(), now);
        assert_eq!(unknown_outcome, Nip11ApplyOutcome::RelayUnknown);

        let unchanged_outcome =
            pool.apply_nip11_limits(&known, Nip11LimitationsRaw::default(), now);
        assert_eq!(unchanged_outcome, Nip11ApplyOutcome::Unchanged);

        let immediate = pool.take_nip11_fetch_requests(1, now);
        assert!(
            immediate.is_empty(),
            "successful apply should defer next fetch until refresh interval"
        );

        let refresh_ready_at = now
            .checked_add(NIP11_REFRESH_AFTER_SUCCESS)
            .expect("refresh timestamp");
        let refresh_ready = pool.take_nip11_fetch_requests(1, refresh_ready_at);
        assert_eq!(refresh_ready.len(), 1);
        assert_eq!(refresh_ready[0].relay, known);
        assert_eq!(refresh_ready[0].attempt, 1);

        let applied_relay = NormRelayUrl::new("wss://relay-nip11-applied.example.com").unwrap();
        let _ = pool.ensure_relay(&applied_relay, &wakeup);
        let applied_raw = Nip11LimitationsRaw {
            max_subscriptions: Some(777),
            ..Default::default()
        };
        let applied_outcome = pool.apply_nip11_limits(&applied_relay, applied_raw, now);
        assert_eq!(applied_outcome, Nip11ApplyOutcome::Applied);

        let limits = pool
            .relays
            .get(&applied_relay)
            .expect("relay present")
            .current_limits();
        assert_eq!(limits.maximum_subs, 777);
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
            assert_eq!(
                sub.routing_preference,
                RelayRoutingPreference::PreferDedicated
            );
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
        assert!(matches!(
            new_task,
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
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
        assert!(matches!(
            relay_task,
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
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

    /// Subscriptions with RequireDedicated route to transparent mode.
    #[test]
    fn subscribe_transparent_mode() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-transparent.example.com").unwrap();
        let id = OutboxSubId(5);

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let mut pkgs = RelayUrlPkgs::new(urls);
        pkgs.routing_preference = RelayRoutingPreference::RequireDedicated;

        let mut session = OutboxSession::default();
        session.subscribe(id, trivial_filter(), pkgs);
        let sessions = pool.collect_sessions(session);

        let task = sessions.get(&relay).and_then(|s| s.tasks.get(&id));
        assert!(matches!(
            task,
            Some(CoordinationTask::Subscribe(
                RelayRoutingPreference::RequireDedicated
            ))
        ));
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
        assert!(matches!(
            task,
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
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
        assert!(matches!(
            sub_task,
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
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
        session.subscribe(OutboxSubId(0), RelayRoutingPreference::PreferDedicated);

        // Map should still have exactly one entry
        assert_eq!(map.len(), 1);
    }
}
