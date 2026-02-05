use hashbrown::HashMap;
use uuid::Uuid;

use crate::{
    relay::{
        subscription::SubscriptionView, MetadataFilters, OutboxSubId, OutboxSubscriptions,
        QueuedTasks, RelayReqId, RelayReqStatus, RelayTask, SubPass, SubPassGuardian,
        SubPassRevocation, WebsocketRelay,
    },
    ClientMessage,
};

/// TransparentData tracks the outstanding transparent REQs and their metadata.
#[derive(Default)]
pub struct TransparentData {
    request_to_sid: HashMap<OutboxSubId, RelayReqId>,
    sid_status: HashMap<RelayReqId, SubData>,
    queue: QueuedTasks,
}

impl TransparentData {
    pub fn num_subs(&self) -> usize {
        self.sid_status.len()
    }

    pub fn contains(&self, id: &OutboxSubId) -> bool {
        self.request_to_sid.contains_key(id)
    }

    pub fn set_req_status(&mut self, sid: &str, status: RelayReqStatus) {
        let Some(entry) = self.sid_status.get_mut(sid) else {
            return;
        };
        entry.status = status;
    }

    pub fn req_status(&self, req_id: &OutboxSubId) -> Option<RelayReqStatus> {
        let sid = self.request_to_sid.get(req_id)?;
        Some(self.sid_status.get(sid)?.status)
    }

    /// Returns the OutboxSubId associated with the given relay subscription ID.
    pub fn id(&self, sid: &RelayReqId) -> Option<OutboxSubId> {
        self.sid_status.get(sid).map(|d| d.sub_req_id)
    }
}

pub struct TransparentRelay<'a> {
    relay: Option<&'a mut WebsocketRelay>,
    data: &'a mut TransparentData,
    sub_guardian: &'a mut SubPassGuardian,
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

    pub fn try_flush_queue(&mut self, subs: &OutboxSubscriptions) {
        while self.sub_guardian.available_passes() > 0 && !self.data.queue.is_empty() {
            let Some(next) = self.data.queue.pop() else {
                return;
            };

            let Some(view) = subs.view(&next) else {
                continue;
            };

            self.subscribe(view);
        }
    }

    pub fn subscribe(&mut self, view: SubscriptionView) {
        let req_id = view.id;
        let Some(existing_sid) = self.data.request_to_sid.get(&req_id) else {
            let Some(new_pass) = self.sub_guardian.take_pass() else {
                self.data.queue.add(req_id, RelayTask::Subscribe);
                return;
            };
            tracing::debug!("Transparent took pass for {req_id:?}");
            let sid: RelayReqId = Uuid::new_v4().into();
            self.data.request_to_sid.insert(req_id, sid.clone());
            send_req(&mut self.relay, &sid, view.filters);
            self.data.sid_status.insert(
                sid,
                SubData {
                    status: RelayReqStatus::InitialQuery,
                    sub_pass: new_pass,
                    sub_req_id: req_id,
                },
            );
            return;
        };

        let Some(sub_data) = self.data.sid_status.get_mut(existing_sid) else {
            return;
        };

        // we're replacing the existing sub with new filters
        sub_data.status = RelayReqStatus::InitialQuery;

        send_req(&mut self.relay, existing_sid, view.filters);
    }

    pub fn unsubscribe(&mut self, req_id: OutboxSubId) {
        let Some(sid) = self.data.request_to_sid.remove(&req_id) else {
            self.data.queue.add(req_id, RelayTask::Unsubscribe);
            return;
        };

        let Some(removed) = self.data.sid_status.remove(&sid) else {
            return;
        };

        self.sub_guardian.return_pass(removed.sub_pass);

        let Some(relay) = &mut self.relay else {
            return;
        };

        if relay.is_connected() {
            relay.conn.send(&ClientMessage::close(sid.to_string()));
        }
    }

    #[profiling::function]
    pub fn handle_relay_open(&mut self, subs: &OutboxSubscriptions) {
        let Some(relay) = &mut self.relay else {
            return;
        };

        if !relay.is_connected() {
            return;
        }

        for (sid, data) in &self.data.sid_status {
            let Some(view) = subs.view(&data.sub_req_id) else {
                continue;
            };

            relay.conn.send(&ClientMessage::req(
                sid.to_string(),
                view.filters.get_filters().clone(),
            ));
        }
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

pub fn revocate_transparent_subs(
    mut relay: Option<&mut WebsocketRelay>,
    data: &mut TransparentData,
    revocations: Vec<SubPassRevocation>,
) {
    // Snapshot the pairs we intend to process (can't mutate while iterating).
    let pairs: Vec<(OutboxSubId, RelayReqId)> = data
        .request_to_sid
        .iter()
        .take(revocations.len())
        .map(|(id, sid)| (*id, sid.clone()))
        .collect();

    for (mut revocation, (id, sid)) in revocations.into_iter().zip(pairs) {
        // If we fail to remove the mapping, skip without consuming other state.
        if data.request_to_sid.remove(&id).is_none() {
            continue;
        }

        let Some(status) = data.sid_status.remove(&sid) else {
            continue;
        };

        revocation.revocate(status.sub_pass);
        data.queue.add(id, RelayTask::Subscribe);

        let Some(relay) = &mut relay else {
            continue;
        };

        if relay.is_connected() {
            relay.conn.send(&ClientMessage::close(sid.to_string()));
        }
    }
}

struct SubData {
    pub status: RelayReqStatus,
    pub sub_pass: SubPass,
    pub sub_req_id: OutboxSubId,
}
