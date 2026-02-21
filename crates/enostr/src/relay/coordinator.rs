use ewebsock::{WsEvent, WsMessage};
use hashbrown::{HashMap, HashSet};

use crate::{
    relay::{
        compaction::{CompactionData, CompactionRelay, CompactionSession},
        transparent::{revocate_transparent_subs, TransparentData, TransparentRelay},
        BroadcastCache, BroadcastRelay, NormRelayUrl, OutboxSubId, OutboxSubscriptions,
        RawEventData, RelayCoordinatorLimits, RelayImplType, RelayLimitations, RelayReqId,
        RelayReqStatus, RelayType, SubPassGuardian, SubPassRevocation, WebsocketRelay,
    },
    EventClientMessage, RelayMessage, RelayStatus, Wakeup, WebsocketConn,
};

/// RelayCoordinator routes each Outbox subscription to either the compaction or
/// transparent relay engine and tracks their status.
pub struct CoordinationData {
    limits: RelayCoordinatorLimits,
    pub(crate) websocket: Option<WebsocketRelay>,
    coordination: HashMap<OutboxSubId, RelayType>,
    compaction_data: CompactionData,
    transparent_data: TransparentData, // for outbox subs that prefer to be transparent
    broadcast_cache: BroadcastCache,
    eose_queue: Vec<RelayReqId>,
}

impl CoordinationData {
    pub fn new<W>(limits: RelayLimitations, norm_url: NormRelayUrl, wakeup: W) -> Self
    where
        W: Wakeup,
    {
        let websocket = match WebsocketConn::from_wakeup(norm_url.clone().into(), wakeup) {
            Ok(w) => Some(WebsocketRelay::new(w)),
            Err(e) => {
                tracing::error!("could not open websocket to {norm_url:?}: {e}");
                None
            }
        };
        let limits = RelayCoordinatorLimits::new(limits);
        let compaction_data = CompactionData::default();
        Self {
            limits,
            websocket,
            compaction_data,
            transparent_data: TransparentData::default(),
            coordination: Default::default(),
            broadcast_cache: Default::default(),
            eose_queue: Vec::new(),
        }
    }

    /// Change if we found a new NIP-11 `max_subscriptions`
    pub fn set_max_size(&mut self, subs: &OutboxSubscriptions, max_size: usize) {
        let Some(revocations) = self.limits.new_total(max_size) else {
            return;
        };

        let mut trans_left = self.transparent_data.num_subs();
        let mut compact_left = self.compaction_data.num_subs();

        let (trans_revocations, compacts_revocations): (
            Vec<SubPassRevocation>,
            Vec<SubPassRevocation>,
        ) = revocations.into_iter().partition(|_| {
            let take_trans = (trans_left > compact_left && trans_left > 0) || (compact_left == 0);

            if take_trans {
                trans_left -= 1;
            } else {
                compact_left -= 1;
            }
            take_trans
        });

        if !trans_revocations.is_empty() {
            revocate_transparent_subs(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                trans_revocations,
            );
        }

        if !compacts_revocations.is_empty() {
            CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            )
            .revocate_all(compacts_revocations);
        }
    }

    #[profiling::function]
    pub fn ingest_session(
        &mut self,
        subs: &OutboxSubscriptions,
        session: CoordinationSession,
    ) -> EoseIds {
        let mut trans_unsubs: HashSet<OutboxSubId> = HashSet::new();
        let mut trans = HashSet::new();
        let mut compaction_session = CompactionSession::default();
        let mut eose_ids = EoseIds::default();

        for (id, task) in session.tasks {
            match task {
                CoordinationTask::TransparentSub => {
                    if let Some(RelayType::Compaction) = self.coordination.get(&id) {
                        compaction_session.unsub(id);
                    }
                    self.coordination.insert(id, RelayType::Transparent);
                    trans.insert(id);
                }
                CoordinationTask::CompactionSub => {
                    if let Some(RelayType::Transparent) = self.coordination.get(&id) {
                        trans_unsubs.insert(id);
                    }
                    self.coordination.insert(id, RelayType::Compaction);
                    compaction_session.sub(id);
                }
                CoordinationTask::Unsubscribe => {
                    let Some(rtype) = self.coordination.remove(&id) else {
                        continue;
                    };

                    match rtype {
                        RelayType::Compaction => {
                            compaction_session.unsub(id);
                        }
                        RelayType::Transparent => {
                            trans_unsubs.insert(id);
                        }
                    }
                }
            }
        }

        // Drain EOSE queue and collect IDs
        for sid in self.eose_queue.drain(..) {
            // Try compaction first
            let Some(compaction_ids) = self.compaction_data.ids(&sid) else {
                let Some(transparent_id) = self.transparent_data.id(&sid) else {
                    continue;
                };

                if subs.is_oneshot(&transparent_id) {
                    trans_unsubs.insert(transparent_id);
                    eose_ids.oneshots.insert(transparent_id);
                } else {
                    eose_ids.normal.insert(transparent_id);
                }
                continue;
            };

            let oneshots = subs.subset_oneshot(compaction_ids);

            for id in compaction_ids {
                if oneshots.contains(id) {
                    compaction_session.unsub(*id);
                    eose_ids.oneshots.insert(*id);
                } else {
                    eose_ids.normal.insert(*id);
                }
            }
        }

        if !trans_unsubs.is_empty() {
            let mut transparent = TransparentRelay::new(
                self.websocket.as_mut(),
                &mut self.transparent_data,
                &mut self.limits.sub_guardian,
            );
            for unsub in trans_unsubs {
                transparent.unsubscribe(unsub);
            }
        }

        if !trans.is_empty() {
            compaction_session.request_free_subs(trans.len());
        }

        if !compaction_session.is_empty() {
            CompactionRelay::new(
                self.websocket.as_mut(),
                &mut self.compaction_data,
                self.limits.max_json_bytes,
                &mut self.limits.sub_guardian,
                subs,
            )
            .ingest_session(compaction_session);
        }

        let mut transparent = TransparentRelay::new(
            self.websocket.as_mut(),
            &mut self.transparent_data,
            &mut self.limits.sub_guardian,
        );
        for id in trans {
            let Some(view) = subs.view(&id) else {
                continue;
            };
            transparent.subscribe(view);
        }

        transparent.try_flush_queue(subs);
        tracing::trace!(
            "Using {} of {} subs",
            self.limits.sub_guardian.total_passes() - self.limits.sub_guardian.available_passes(),
            self.limits.sub_guardian.total_passes()
        );

        eose_ids
    }

    pub fn send_event(&mut self, msg: EventClientMessage) {
        BroadcastRelay::websocket(self.websocket.as_mut(), &mut self.broadcast_cache)
            .broadcast(msg);
    }

    pub fn set_req_status(&mut self, sid: &str, status: RelayReqStatus) {
        // the compaction & transparent data only act on sids that they already know, so whichever
        // this sid belongs to, it'll make it to its rightful home
        self.compaction_data.set_req_status(sid, status);
        self.transparent_data.set_req_status(sid, status);
    }

    pub fn req_status(&self, id: &OutboxSubId) -> Option<RelayReqStatus> {
        match self.coordination.get(id)? {
            RelayType::Compaction => self.compaction_data.req_status(id),
            RelayType::Transparent => self.transparent_data.req_status(id),
        }
    }

    pub fn has_req_status(&self, id: &OutboxSubId, status: RelayReqStatus) -> bool {
        self.req_status(id) == Some(status)
    }

    fn url(&self) -> &str {
        let Some(websocket) = &self.websocket else {
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
                handle_relay_open(
                    websocket,
                    &mut self.broadcast_cache,
                    &mut self.compaction_data,
                    &mut self.transparent_data,
                    self.limits.max_json_bytes,
                    &mut self.limits.sub_guardian,
                    subs,
                );
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
pub struct RecvResponse {
    pub received_event: bool,
    pub event_was_nostr_note: bool,
}

impl RecvResponse {
    pub fn received() -> Self {
        RecvResponse {
            received_event: true,
            event_was_nostr_note: false,
        }
    }
}

#[derive(Default)]
pub struct EoseIds {
    pub oneshots: HashSet<OutboxSubId>,
    pub normal: HashSet<OutboxSubId>,
}

impl EoseIds {
    /// Merges IDs from `other` into `self`, preserving set uniqueness.
    pub fn absorb(&mut self, other: EoseIds) {
        self.oneshots.extend(other.oneshots);
        self.normal.extend(other.normal);
    }
}

fn handle_relay_open(
    websocket: &mut WebsocketRelay,
    broadcast_cache: &mut BroadcastCache,
    compaction: &mut CompactionData,
    transparent: &mut TransparentData,
    max_json: usize,
    guardian: &mut SubPassGuardian,
    subs: &OutboxSubscriptions,
) {
    BroadcastRelay::websocket(Some(websocket), broadcast_cache).try_flush_queue();
    let mut transparent = TransparentRelay::new(Some(websocket), transparent, guardian);
    transparent.handle_relay_open(subs);
    let mut compaction =
        CompactionRelay::new(Some(websocket), compaction, max_json, guardian, subs);
    compaction.handle_relay_open();
}

#[derive(Default)]
pub struct CoordinationSession {
    pub tasks: HashMap<OutboxSubId, CoordinationTask>,
}

pub enum CoordinationTask {
    TransparentSub,
    CompactionSub,
    Unsubscribe,
}

impl CoordinationSession {
    pub fn subscribe(&mut self, id: OutboxSubId, use_transparent: bool) {
        self.tasks.insert(
            id,
            if use_transparent {
                CoordinationTask::TransparentSub
            } else {
                CoordinationTask::CompactionSub
            },
        );
    }

    pub fn unsubscribe(&mut self, id: OutboxSubId) {
        self.tasks.insert(id, CoordinationTask::Unsubscribe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the task held for `id`, panicking when no matching task exists.
    #[track_caller]
    fn expect_task<'a>(session: &'a CoordinationSession, id: OutboxSubId) -> &'a CoordinationTask {
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

    /// Transparent subscriptions should be recorded as TransparentSub tasks.
    #[test]
    fn coordination_session_subscribe_transparent() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), true); // use_transparent = true

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::TransparentSub
        ));
    }

    /// Compaction mode subscriptions should be recorded as CompactionSub tasks.
    #[test]
    fn coordination_session_subscribe_compaction() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), false); // use_transparent = false means compaction

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::CompactionSub
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

        // First subscribe as transparent
        session.subscribe(OutboxSubId(0), true);

        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::TransparentSub
        ));

        // Then as compaction
        session.subscribe(OutboxSubId(0), false);

        // Should be compaction now
        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::CompactionSub
        ));
    }

    /// Unsubscribe should override any prior subscribe entries.
    #[test]
    fn coordination_session_unsubscribe_overwrites_subscribe() {
        let mut session = CoordinationSession::default();

        session.subscribe(OutboxSubId(0), true);
        assert!(matches!(
            expect_task(&session, OutboxSubId(0)),
            CoordinationTask::TransparentSub
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

        session.subscribe(OutboxSubId(0), true);
        session.subscribe(OutboxSubId(1), false);
        session.unsubscribe(OutboxSubId(2));

        assert_eq!(session.tasks.len(), 3);
    }

    // ==================== EoseIds tests ====================

    #[test]
    fn eose_ids_default_empty() {
        let eose_ids = EoseIds::default();
        assert!(eose_ids.oneshots.is_empty());
        assert!(eose_ids.normal.is_empty());
    }

    /// absorb merges oneshot and normal ID sets into the target accumulator.
    #[test]
    fn eose_ids_absorb_merges_both_sets() {
        let mut acc = EoseIds::default();
        let mut incoming = EoseIds::default();

        acc.oneshots.insert(OutboxSubId(1));
        incoming.oneshots.insert(OutboxSubId(2));
        incoming.normal.insert(OutboxSubId(3));

        acc.absorb(incoming);

        assert!(acc.oneshots.contains(&OutboxSubId(1)));
        assert!(acc.oneshots.contains(&OutboxSubId(2)));
        assert!(acc.normal.contains(&OutboxSubId(3)));
    }
}
