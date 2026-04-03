use hashbrown::{hash_map::RawEntryMut, HashMap, HashSet};
use nostrdb::{Filter, Note};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    relay::{
        backoff,
        coordinator::{CoordinationData, CoordinationSession, RelayEoseDelta},
        websocket::WebsocketRelay,
        ModifyTask, MulticastRelayCache, Nip11ApplyOutcome, Nip11FetchRequest, Nip11LimitationsRaw,
        NormRelayUrl, OutboxSubId, OutboxSubscriptions, OutboxTask, RawEventData, RelayId,
        RelayLimitations, RelayReqStatus, RelayRoutingPreference, RelayStatus, RelayType,
    },
    EventClientMessage, Wakeup,
};

mod eose;
mod handler;
mod session;

use eose::{
    plan_tracker_invalidation, ChangedRelayLeg, EoseTracker, FullyEosedEffectsPlan,
    TrackerInvalidationPlan,
};
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
    eose_tracker: EoseTracker,
    multicast: MulticastRelayCache,
    pong_timeout: Duration,
}

impl Default for OutboxPool {
    fn default() -> Self {
        Self {
            registry: SubRegistry { next_request_id: 0 },
            relays: HashMap::new(),
            eose_tracker: EoseTracker::default(),
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

    /// Applies an already planned set of post-EOSE subscription effects.
    fn apply_fully_eosed_effects(&mut self, plan: FullyEosedEffectsPlan) {
        for id in plan.remove_oneshots {
            self.subs.remove(&id);
            self.eose_tracker.remove_sub(&id);
        }

        let Some(now) = plan.optimize_since_at else {
            return;
        };
        for id in plan.optimize_since {
            let Some(sub) = self.subs.get_mut(&id) else {
                continue;
            };
            sub.see_all(now);
            sub.filters.since_optimize();
        }
    }

    /// Returns true when every currently requested relay leg for this
    /// subscription is owned by compaction, making `since` advancement safe.
    fn is_fully_compaction_routed(&self, id: OutboxSubId) -> bool {
        let Some(sub) = self.subs.get(&id) else {
            return false;
        };
        if sub.relays.is_empty() {
            return false;
        }

        sub.relays.iter().all(|relay_id| {
            self.relays
                .get(relay_id)
                .and_then(|relay| relay.route_type(&id))
                == Some(RelayType::Compaction)
        })
    }

    /// Classifies fully-EOSE subscriptions into concrete lifecycle effects.
    ///
    /// Fully completed oneshots are removed immediately. `since` advancement is
    /// only safe for subscriptions whose entire current relay set is routed
    /// through compaction.
    fn plan_fully_eosed_effects(&self, ids: HashSet<OutboxSubId>) -> FullyEosedEffectsPlan {
        let mut remove_oneshots = HashSet::new();
        let mut optimize_since = HashSet::new();

        for id in ids {
            if self.subs.is_oneshot(&id) {
                remove_oneshots.insert(id);
                continue;
            }

            if self.is_fully_compaction_routed(id) {
                optimize_since.insert(id);
            }
        }

        let optimize_since_at = if optimize_since.is_empty() {
            None
        } else {
            unix_now_secs()
        };

        FullyEosedEffectsPlan {
            remove_oneshots,
            optimize_since,
            optimize_since_at,
        }
    }

    /// Drains tracker-ready full-EOSE completions and applies their derived
    /// subscription side effects immediately.
    fn flush_fully_eosed_effects(&mut self) {
        let fully_eosed = self.eose_tracker.drain_fully_eosed();
        if fully_eosed.is_empty() {
            return;
        }

        let effects = self.plan_fully_eosed_effects(fully_eosed);
        if effects.is_empty() {
            return;
        }

        self.apply_fully_eosed_effects(effects);
    }

    #[profiling::function]
    fn ingest_session<W>(&mut self, session: OutboxSession, wakeup: &W)
    where
        W: Wakeup,
    {
        let session_delta = self.collect_sessions(session);

        self.apply_tracker_invalidation(plan_tracker_invalidation(
            &session_delta.changed_legs,
            &session_delta.removed_subs,
        ));
        self.flush_fully_eosed_effects();
        if !session_delta.sessions.is_empty() {
            self.process_relay_work(session_delta.sessions, wakeup);
            self.flush_fully_eosed_effects();
        }
    }

    /// Translates a session's queued tasks into per-relay coordination sessions.
    #[profiling::function]
    fn collect_sessions(&mut self, session: OutboxSession) -> SessionDelta {
        if session.tasks.is_empty() {
            return SessionDelta::default();
        }

        let mut delta = SessionDelta::default();
        'a: for (id, task) in session.tasks {
            match task {
                OutboxTask::Modify(modify) => {
                    let Some(sub) = self.subs.get(&id) else {
                        continue 'a;
                    };
                    let routing_preference = sub.routing_preference;
                    let mut remove_sub = false;

                    match &modify {
                        ModifyTask::Filters(_) => {
                            for relay in &sub.relays {
                                stage_subscribe_task(&mut delta, relay, id, routing_preference);
                            }
                        }
                        ModifyTask::Relays(modify_relays_task) => {
                            let relays_to_remove = sub.relays.difference(&modify_relays_task.0);
                            for relay in relays_to_remove {
                                stage_unsubscribe_task(&mut delta, relay, id);
                            }

                            let relays_to_add = modify_relays_task.0.difference(&sub.relays);
                            for relay in relays_to_add {
                                stage_subscribe_task(&mut delta, relay, id, routing_preference);
                            }
                        }
                        ModifyTask::Full(full_modification_task) => {
                            let new_relays = &full_modification_task.relays;
                            let relays_to_remove = sub.relays.difference(new_relays);
                            for relay in relays_to_remove {
                                stage_unsubscribe_task(&mut delta, relay, id);
                            }

                            if new_relays.is_empty() {
                                remove_sub = true;
                            } else {
                                for relay in new_relays {
                                    stage_subscribe_task(&mut delta, relay, id, routing_preference);
                                }
                            }
                        }
                    }

                    if remove_sub {
                        self.subs.remove(&id);
                        delta.removed_subs.insert(id);
                        continue 'a;
                    }

                    let Some(sub) = self.subs.get_mut(&id) else {
                        continue 'a;
                    };
                    sub.ingest_task(modify);
                }
                OutboxTask::Unsubscribe => {
                    let Some(sub) = self.subs.get(&id) else {
                        continue 'a;
                    };
                    for relay_id in &sub.relays {
                        stage_unsubscribe_task(&mut delta, relay_id, id);
                    }

                    self.subs.remove(&id);
                    delta.removed_subs.insert(id);
                }
                OutboxTask::Oneshot(subscribe) => {
                    for relay in &subscribe.relays.urls {
                        stage_subscribe_task(
                            &mut delta,
                            relay,
                            id,
                            subscribe.relays.routing_preference,
                        );
                    }
                    delta.removed_subs.insert(id);
                    self.subs.new_subscription(id, subscribe, true);
                }
                OutboxTask::Subscribe(subscribe) => {
                    for relay in &subscribe.relays.urls {
                        stage_subscribe_task(
                            &mut delta,
                            relay,
                            id,
                            subscribe.relays.routing_preference,
                        );
                    }

                    delta.removed_subs.insert(id);
                    self.subs.new_subscription(id, subscribe, false);
                }
            }
        }

        delta
    }

    /// Applies tracker invalidation changes prepared from the latest session delta.
    fn apply_tracker_invalidation(&mut self, plan: TrackerInvalidationPlan<'_>) {
        for leg in plan.changed_legs {
            self.eose_tracker
                .invalidate_relay_leg(&leg.relay, leg.sub_id, &self.subs);
        }
        for id in plan.removed_subs {
            self.eose_tracker.remove_sub(id);
        }
    }

    /// Runs coordinator ingest for relays with staged work only.
    #[profiling::function]
    fn process_relay_work<W>(
        &mut self,
        sessions: HashMap<NormRelayUrl, CoordinationSession>,
        wakeup: &W,
    ) where
        W: Wakeup,
    {
        for (relay_id, session) in sessions {
            let _ = self.ensure_relay(&relay_id, wakeup);
            let has_pending = self.ingest_relay_session(&relay_id, session);
            self.eose_tracker
                .set_relay_pending_effect_state(&relay_id, has_pending);
        }
    }

    /// Ingests one relay session with staged subscription work and applies the
    /// resulting resolved relay effects to outbox lifecycle state.
    fn ingest_relay_session(
        &mut self,
        relay_id: &NormRelayUrl,
        session: CoordinationSession,
    ) -> bool {
        let ingest = {
            let Some(relay) = self.relays.get_mut(relay_id) else {
                return false;
            };
            relay.ingest_session(&self.subs, session)
        };
        self.apply_ingest_result(relay_id, ingest)
    }

    /// Flushes one relay's pending coordinator-side effects without staging any
    /// new subscription work.
    fn flush_relay_pending_effects(&mut self, relay_id: &NormRelayUrl) -> bool {
        let ingest = {
            let Some(relay) = self.relays.get_mut(relay_id) else {
                return false;
            };
            relay.flush_pending_effects(&self.subs)
        };
        self.apply_ingest_result(relay_id, ingest)
    }

    /// Applies one relay ingest result and any follow-up oneshot unsubs until
    /// the relay's resolved effect stream is fully consumed.
    fn apply_ingest_result(
        &mut self,
        relay_id: &NormRelayUrl,
        mut ingest: crate::relay::coordinator::IngestSessionResult,
    ) -> bool {
        let mut has_pending_effects = false;
        loop {
            has_pending_effects |= ingest.has_pending_effects;
            let oneshot_unsubs = self.apply_relay_eose_delta(relay_id, ingest.eose_delta);
            if oneshot_unsubs.is_empty() {
                break;
            }

            let mut unsub_session = CoordinationSession::default();
            for id in oneshot_unsubs {
                unsub_session.unsubscribe(id);
            }

            ingest = {
                let Some(relay) = self.relays.get_mut(relay_id) else {
                    return has_pending_effects;
                };
                relay.ingest_session(&self.subs, unsub_session)
            };
        }

        has_pending_effects
    }

    /// Applies one relay's EOSE delta to the durable tracker and returns oneshot legs to close.
    fn apply_relay_eose_delta(
        &mut self,
        relay_id: &NormRelayUrl,
        delta: RelayEoseDelta,
    ) -> Vec<OutboxSubId> {
        let mut oneshot_unsubs = Vec::new();
        self.apply_relay_tracker_invalidations(relay_id, delta.invalidated_sub_ids);
        for id in delta.sub_ids {
            if self.subs.get(&id).is_none() {
                continue;
            }
            self.eose_tracker.mark_relay_eose(relay_id, id, &self.subs);

            if self.subs.is_oneshot(&id) {
                oneshot_unsubs.push(id);
            }
        }

        oneshot_unsubs
    }

    /// Clears durable EOSE state for relay legs coordinator reset internally.
    fn apply_relay_tracker_invalidations(
        &mut self,
        relay_id: &NormRelayUrl,
        invalidated_sub_ids: HashSet<OutboxSubId>,
    ) {
        for id in invalidated_sub_ids {
            self.eose_tracker
                .invalidate_relay_leg(relay_id, id, &self.subs);
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
        for (relay_id, relay) in &mut self.relays {
            relay
                .websocket
                .try_restore_with_fn(relay_id.clone().into(), wakeup.clone(), false);
            let now = Instant::now();

            let Some(websocket) = relay.websocket.as_mut() else {
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
        let (current, derived) = {
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
            (current, derived)
        };

        let has_pending = self.flush_relay_pending_effects(relay);
        self.eose_tracker
            .set_relay_pending_effect_state(relay, has_pending);
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
            RawEntryMut::Occupied(entry) => {
                let relay = entry.into_mut();
                relay.websocket.try_restore_with_wakeup(
                    relay_id.clone().into(),
                    wakeup.clone(),
                    true,
                );
                relay
            }
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
            let relay_status = if let Some(websocket) = relay.websocket.as_ref() {
                websocket.conn.status
            } else {
                RelayStatus::Disconnected
            };
            status.insert(url, relay_status);
        }

        status
    }

    pub fn has_eose(&self, id: &OutboxSubId) -> bool {
        if self.eose_tracker.has_any_eose(&self.subs, id) {
            return true;
        }

        for relay in self.relays.values() {
            if relay.req_status(id) == Some(RelayReqStatus::Eose) {
                return true;
            }
        }

        false
    }

    pub fn all_have_eose(&self, id: &OutboxSubId) -> bool {
        self.eose_tracker.is_fully_eosed(&self.subs, id)
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

            for (relay_id, relay) in &mut self.relays {
                let resp = relay.try_recv(&self.subs, &mut process);
                if resp.eose_enqueued || relay.has_pending_effects() {
                    self.eose_tracker.note_relay_pending_effects(relay_id);
                }

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

        self.process_pending_relay_effects();
        self.multicast.try_recv(process);
    }

    /// Processes relay-local pending effects accumulated during receive polling.
    fn process_pending_relay_effects(&mut self) {
        let relays = self.eose_tracker.drain_pending_effect_relays();
        for relay_id in relays {
            let has_pending = self.flush_relay_pending_effects(&relay_id);
            self.eose_tracker
                .set_relay_pending_effect_state(&relay_id, has_pending);
        }

        self.flush_fully_eosed_effects();
    }
}

/// Session translation output: per-relay tasks plus tracker-invalidating changes.
#[derive(Default)]
struct SessionDelta {
    sessions: HashMap<NormRelayUrl, CoordinationSession>,
    changed_legs: Vec<ChangedRelayLeg>,
    removed_subs: HashSet<OutboxSubId>,
}

#[cfg(test)]
impl SessionDelta {
    fn get(&self, relay: &NormRelayUrl) -> Option<&CoordinationSession> {
        self.sessions.get(relay)
    }
}

/// Stages a subscribe task and records a changed relay leg.
fn stage_subscribe_task(
    delta: &mut SessionDelta,
    relay: &NormRelayUrl,
    id: OutboxSubId,
    routing_preference: RelayRoutingPreference,
) {
    delta.changed_legs.push(ChangedRelayLeg {
        relay: relay.clone(),
        sub_id: id,
    });
    let session = get_session(&mut delta.sessions, relay);
    session.subscribe(id, routing_preference);
}

/// Stages an unsubscribe task and records a changed relay leg.
fn stage_unsubscribe_task(delta: &mut SessionDelta, relay: &NormRelayUrl, id: OutboxSubId) {
    delta.changed_legs.push(ChangedRelayLeg {
        relay: relay.clone(),
        sub_id: id,
    });
    let session = get_session(&mut delta.sessions, relay);
    session.unsubscribe(id);
}

fn unix_now_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
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
        coordinator::{CoordinationSession, CoordinationTask},
        test_utils::{filters_json, trivial_filter, MockWakeup},
        RelayRoutingPreference, RelayType, RelayUrlPkgs, SubscribeTask,
    };

    fn filter_has_since(filter: &Filter) -> bool {
        filter.json().expect("filter json").contains("\"since\"")
    }

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

    /// Existing relay coordinators with a missing websocket should be restored by ensure_relay.
    #[test]
    fn ensure_relay_restores_missing_websocket() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_id = NormRelayUrl::new("wss://relay-restore.example.com").unwrap();

        let relay = pool.ensure_relay(&relay_id, &wakeup);
        assert!(relay.websocket.as_ref().is_some());

        pool.relays
            .get_mut(&relay_id)
            .expect("relay exists")
            .websocket
            .clear_for_test();

        let restored = pool.ensure_relay(&relay_id, &wakeup);
        assert!(restored.websocket.as_ref().is_some());
    }

    /// EOSE from relays not currently routed for a subscription should be ignored.
    #[test]
    fn eose_tracker_ignores_non_routed_relays() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-routed.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-stale.example.com").unwrap();
        let id = OutboxSubId(1);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(relays),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.mark_relay_eose(&relay_b, id, &subs);
        assert!(
            !tracker.has_any_eose(&subs, &id),
            "stale relay should not mark any EOSE progress"
        );
        assert!(
            !tracker.is_fully_eosed(&subs, &id),
            "stale relay should not mark sub fully EOSE"
        );

        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(tracker.has_any_eose(&subs, &id));
        assert!(tracker.is_fully_eosed(&subs, &id));
        assert!(tracker.drain_fully_eosed().contains(&id));
    }

    /// Clearing one routed relay should invalidate cached fully-EOSE completion for that sub.
    #[test]
    fn eose_tracker_invalidate_relay_leg_invalidates_cached_completion() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-clear-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-clear-b.example.com").unwrap();
        let id = OutboxSubId(2);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());
        relays.insert(relay_b.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(relays),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(!tracker.is_fully_eosed(&subs, &id));

        tracker.mark_relay_eose(&relay_b, id, &subs);
        assert!(tracker.is_fully_eosed(&subs, &id));

        tracker.invalidate_relay_leg(&relay_a, id, &subs);
        assert!(
            !tracker.is_fully_eosed(&subs, &id),
            "clearing one routed relay must drop cached completion"
        );
    }

    /// Shrinking a subscription's relay set can complete it immediately if all
    /// remaining relays had already reached EOSE.
    #[test]
    fn eose_tracker_reconciles_completion_when_relay_set_shrinks() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-shrink-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-shrink-b.example.com").unwrap();
        let id = OutboxSubId(22);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());
        relays.insert(relay_b.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(relays),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(!tracker.is_fully_eosed(&subs, &id));

        subs.get_mut(&id).unwrap().relays.remove(&relay_b);
        tracker.invalidate_relay_leg(&relay_b, id, &subs);

        assert!(tracker.is_fully_eosed(&subs, &id));
        assert!(
            tracker.drain_fully_eosed().contains(&id),
            "shrink-induced completion must queue ready_fully_eosed"
        );
    }

    /// Expanding a subscription's relay set should drop full completion until
    /// the newly added relay also reaches EOSE.
    #[test]
    fn eose_tracker_reconciles_incomplete_when_relay_set_expands() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-expand-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-expand-b.example.com").unwrap();
        let id = OutboxSubId(23);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(relays),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(tracker.is_fully_eosed(&subs, &id));
        assert!(tracker.drain_fully_eosed().contains(&id));

        subs.get_mut(&id).unwrap().relays.insert(relay_b.clone());
        tracker.invalidate_relay_leg(&relay_b, id, &subs);

        assert!(
            !tracker.is_fully_eosed(&subs, &id),
            "adding a new relay must invalidate full completion until it EOSEs"
        );
        assert!(
            !tracker.drain_fully_eosed().contains(&id),
            "expansion must not queue a ready transition"
        );

        tracker.mark_relay_eose(&relay_b, id, &subs);
        assert!(tracker.is_fully_eosed(&subs, &id));
        assert!(tracker.drain_fully_eosed().contains(&id));
    }

    /// Coordinator-reported relay-leg invalidations must clear stale durable
    /// EOSE completion before any fresh REQ on that relay can complete again.
    #[test]
    fn apply_relay_eose_delta_clears_invalidated_sub_ids() {
        let relay = NormRelayUrl::new("wss://relay-eose-delta-clear.example.com").unwrap();
        let id = OutboxSubId(3);
        let mut relays = HashSet::new();
        relays.insert(relay.clone());

        let mut pool = OutboxPool::default();
        pool.subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(relays),
            },
            false,
        );

        pool.eose_tracker.mark_relay_eose(&relay, id, &pool.subs);
        assert!(pool.all_have_eose(&id));

        let delta = RelayEoseDelta {
            sub_ids: HashSet::new(),
            invalidated_sub_ids: HashSet::from([id]),
        };
        let oneshot_unsubs = pool.apply_relay_eose_delta(&relay, delta);

        assert!(oneshot_unsubs.is_empty());
        assert!(
            !pool.all_have_eose(&id),
            "a fresh internally issued REQ must clear prior durable EOSE state"
        );
    }

    /// Receive-driven EOSE processing must apply ready fully-EOSE effects in
    /// the same frame, including oneshot cleanup.
    #[test]
    fn process_pending_relay_effects_applies_ready_fully_eosed_effects() {
        let relay = NormRelayUrl::new("wss://relay-eose-pending-effects.example.com").unwrap();
        let id = OutboxSubId(24);
        let mut relays = HashSet::new();
        relays.insert(relay.clone());

        let mut pool = OutboxPool::default();
        pool.subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(relays),
            },
            true,
        );

        pool.eose_tracker.mark_relay_eose(&relay, id, &pool.subs);
        pool.eose_tracker.note_relay_pending_effects(&relay);
        pool.process_pending_relay_effects();

        assert!(
            pool.subs.get(&id).is_none(),
            "oneshot should be removed as soon as receive-path EOSE processing completes"
        );
    }

    /// Unsaturated relays should place preferred dedicated requests on a
    /// dedicated leg rather than falling through to compaction.
    #[test]
    fn prefer_dedicated_request_uses_dedicated_when_unsaturated() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay =
            NormRelayUrl::new("wss://relay-prefer-dedicated-unsaturated.example.com").unwrap();

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(trivial_filter(), pkgs)
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
    }

    /// Unsaturated relays should also place no-preference requests on a
    /// dedicated leg before considering compaction fallback.
    #[test]
    fn no_preference_request_uses_dedicated_when_unsaturated() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-no-preference-unsaturated.example.com").unwrap();

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::NoPreference;
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(trivial_filter(), pkgs)
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
    }

    /// Fully EOSE'd dedicated routes should keep their original filters instead
    /// of advancing `since`, which is only safe for compaction.
    #[test]
    fn fully_eosed_dedicated_route_does_not_optimize_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-since-dedicated.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(vec![filter], pkgs)
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));

        pool.eose_tracker.mark_relay_eose(&relay, id, &pool.subs);
        pool.flush_fully_eosed_effects();

        let filter = &pool
            .subs
            .view(&id)
            .expect("subscription")
            .filters
            .get_filters()[0];
        assert!(
            !filter_has_since(filter),
            "dedicated routes must not rewrite filters with a synthetic since cursor"
        );
    }

    /// Fully EOSE'd compaction routes should advance `since` so future shared
    /// REQs do not re-fetch the same history.
    #[test]
    fn fully_eosed_compaction_route_optimizes_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-since-compaction.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let _ = pool.ensure_relay(&relay, &wakeup);
        {
            let (subs, relays) = (&pool.subs, &mut pool.relays);
            relays.get_mut(&relay).expect("coordinator").set_limits(
                subs,
                RelayLimitations {
                    maximum_subs: 0,
                    max_json_bytes: RelayLimitations::default().max_json_bytes,
                },
            );
        }

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(vec![filter], pkgs)
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Compaction));

        pool.eose_tracker.mark_relay_eose(&relay, id, &pool.subs);
        pool.flush_fully_eosed_effects();

        let filter = &pool
            .subs
            .view(&id)
            .expect("subscription")
            .filters
            .get_filters()[0];
        assert!(
            filter_has_since(filter),
            "compaction routes should advance since after fully catching up"
        );
    }

    /// Mixed routing should refuse `since` optimization until every relay leg
    /// for the subscription is owned by compaction.
    #[test]
    fn fully_eosed_mixed_routes_do_not_optimize_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_dedicated =
            NormRelayUrl::new("wss://relay-since-mixed-dedicated.example.com").unwrap();
        let relay_compaction =
            NormRelayUrl::new("wss://relay-since-mixed-compaction.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let _ = pool.ensure_relay(&relay_compaction, &wakeup);
        {
            let (subs, relays) = (&pool.subs, &mut pool.relays);
            relays
                .get_mut(&relay_compaction)
                .expect("coordinator")
                .set_limits(
                    subs,
                    RelayLimitations {
                        maximum_subs: 0,
                        max_json_bytes: RelayLimitations::default().max_json_bytes,
                    },
                );
        }

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay_dedicated.clone());
            relays.insert(relay_compaction.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(vec![filter], pkgs)
        };

        let dedicated = pool.relays.get(&relay_dedicated).expect("dedicated relay");
        assert_eq!(dedicated.route_type(&id), Some(RelayType::Transparent));
        let compaction = pool
            .relays
            .get(&relay_compaction)
            .expect("compaction relay");
        assert_eq!(compaction.route_type(&id), Some(RelayType::Compaction));

        pool.eose_tracker
            .mark_relay_eose(&relay_dedicated, id, &pool.subs);
        pool.eose_tracker
            .mark_relay_eose(&relay_compaction, id, &pool.subs);
        pool.flush_fully_eosed_effects();

        let filter = &pool
            .subs
            .view(&id)
            .expect("subscription")
            .filters
            .get_filters()[0];
        assert!(
            !filter_has_since(filter),
            "a mixed dedicated/compaction subscription must not advance since"
        );
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

    /// Oneshot requests use the default prefer-dedicated routing policy.
    #[test]
    fn oneshot_routes_to_prefer_dedicated() {
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

    /// Subscriptions with `PreferDedicated` policy route to dedicated-preferred mode.
    #[test]
    fn subscribe_dedicated_preferred_mode() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-transparent.example.com").unwrap();
        let id = OutboxSubId(5);

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let mut pkgs = RelayUrlPkgs::new(urls);
        pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;

        let mut session = OutboxSession::default();
        session.subscribe(id, trivial_filter(), pkgs);
        let sessions = pool.collect_sessions(session);

        let task = sessions.get(&relay).and_then(|s| s.tasks.get(&id));
        assert!(matches!(
            task,
            Some(CoordinationTask::Subscribe(
                RelayRoutingPreference::PreferDedicated
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

    /// Modifying filters should preserve the default dedicated retry policy.
    #[test]
    fn modify_filters_preserves_default_dedicated_retry_policy() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify-default-retry.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let sub_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(urls))
        };

        let sessions = {
            let mut handler = pool.start_session(wakeup);
            handler.modify_filters(sub_id, vec![Filter::new().kinds(vec![1]).limit(7).build()]);
            let session = handler.export();
            pool.collect_sessions(session)
        };

        let task = sessions
            .get(&relay)
            .and_then(|session| session.tasks.get(&sub_id))
            .expect("expected coordination task");
        assert!(matches!(
            task,
            CoordinationTask::Subscribe(RelayRoutingPreference::PreferDedicated)
        ));
    }

    /// Modifying filters should preserve the prefer-dedicated retry policy.
    #[test]
    fn modify_filters_preserves_preferred_dedicated_retry_policy() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify-preferred-retry.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let mut pkgs = RelayUrlPkgs::new(urls);
        pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
        let sub_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), pkgs)
        };

        let sessions = {
            let mut handler = pool.start_session(wakeup);
            handler.modify_filters(sub_id, vec![Filter::new().kinds(vec![1]).limit(9).build()]);
            let session = handler.export();
            pool.collect_sessions(session)
        };

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

    /// High churn of modify/unsubscribe operations should keep active and inactive
    /// subscription state consistent without leaking relay status entries.
    #[test]
    fn high_churn_modify_and_unsubscribe_keeps_consistent_state() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-churn-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-churn-b.example.com").unwrap();
        let relay_c = NormRelayUrl::new("wss://relay-churn-c.example.com").unwrap();

        let mut relays_ab = HashSet::new();
        relays_ab.insert(relay_a.clone());
        relays_ab.insert(relay_b.clone());

        let mut relays_bc = HashSet::new();
        relays_bc.insert(relay_b.clone());
        relays_bc.insert(relay_c.clone());

        let mut active_relays = relays_ab.clone();
        let mut active_id = {
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), RelayUrlPkgs::new(active_relays.clone()))
        };

        let mut inactive_ids = Vec::new();
        for i in 0..200usize {
            if i % 11 == 10 {
                let old_id = active_id;
                inactive_ids.push(old_id);

                active_relays = if i % 2 == 0 {
                    relays_ab.clone()
                } else {
                    relays_bc.clone()
                };

                active_id = {
                    let mut handler = pool.start_session(wakeup.clone());
                    handler.unsubscribe(old_id);
                    handler.subscribe(trivial_filter(), RelayUrlPkgs::new(active_relays.clone()))
                };
            } else {
                {
                    let mut handler = pool.start_session(wakeup.clone());
                    if i % 3 == 0 {
                        active_relays = if i % 2 == 0 {
                            relays_ab.clone()
                        } else {
                            relays_bc.clone()
                        };
                        handler.modify_relays(active_id, active_relays.clone());
                    }
                    handler.modify_filters(
                        active_id,
                        vec![Filter::new().kinds(vec![(i % 5) as u64]).limit(3).build()],
                    );
                }
            }

            let active_status = pool.status(&active_id);
            assert_eq!(active_status.len(), active_relays.len());
            for relay in &active_relays {
                assert!(active_status.contains_key(relay));
            }
            for old_id in &inactive_ids {
                assert!(
                    pool.status(old_id).is_empty(),
                    "inactive subscription should not retain relay state"
                );
            }
        }
    }

    /// Under relay saturation with only prefer-dedicated subscriptions, the
    /// existing preferred dedicated route should keep its dedicated slot and the
    /// incoming preferred request should compact instead.
    #[test]
    fn saturation_keeps_existing_preferred_dedicated_when_all_preferred_and_full() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-saturation-demotion.example.com").unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_250);

        let _ = pool.ensure_relay(&relay, &wakeup);
        let applied = pool.apply_nip11_limits(
            &relay,
            Nip11LimitationsRaw {
                max_subscriptions: Some(1),
                ..Default::default()
            },
            now,
        );
        assert!(matches!(
            applied,
            Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
        ));

        let id_first = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
            let mut handler = pool.start_session(wakeup.clone());
            handler.subscribe(trivial_filter(), pkgs)
        };

        let id_second = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let mut pkgs = RelayUrlPkgs::new(relays);
            pkgs.routing_preference = RelayRoutingPreference::PreferDedicated;
            let mut handler = pool.start_session(wakeup);
            handler.subscribe(trivial_filter(), pkgs)
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator should exist");
        assert_eq!(
            coordinator.route_type(&id_first),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_second),
            Some(RelayType::Compaction)
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
