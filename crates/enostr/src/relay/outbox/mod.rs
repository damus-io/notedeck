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
