use std::collections::VecDeque;
use std::time::Instant;

use hashbrown::{HashMap, HashSet};

use super::{OutboxEvent, OutboxServiceOutput};
use crate::relay::outbox::{RelayDemandChanged, RelayTransportDemand};
use crate::relay::{
    MulticastTransportRuntime, NormRelayUrl, RelayConnectionPriority, RelayDemandPriority,
    RelayStatus, RelayUrlSource, WebsocketConn,
};
use crate::{ClientMessage, EventClientMessage};

pub(super) struct ServiceWebsocketLeg {
    pub(super) conn: WebsocketConn,
    pub(super) generation: u64,
    pub(super) last_ping: Instant,
    pub(super) last_pong: Instant,
}

impl ServiceWebsocketLeg {
    pub(super) fn new(conn: WebsocketConn, generation: u64) -> Self {
        let now = Instant::now();
        Self {
            conn,
            generation,
            last_ping: now,
            last_pong: now,
        }
    }

    pub(super) fn is_connected(&self) -> bool {
        self.conn.status == RelayStatus::Connected
    }

    pub(super) fn send_event(&mut self, msg: EventClientMessage) {
        self.conn.send(&ClientMessage::from(msg));
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RelayReconnectState {
    pub(super) attempt: u32,
    pub(super) retry_at: Instant,
}

#[derive(Default)]
pub(super) struct RelayTransportRuntime {
    pool_demands: HashMap<NormRelayUrl, RelayTransportDemand>,
    raw_full_history_pending_demands: HashMap<NormRelayUrl, RelayTransportDemand>,
    full_history_pending_demands: HashMap<NormRelayUrl, RelayTransportDemand>,
    raw_negentropy_demands: HashMap<NormRelayUrl, RelayTransportDemand>,
    negentropy_demands: HashMap<NormRelayUrl, RelayTransportDemand>,
    subids_unsupported: HashSet<NormRelayUrl>,
    next_generations: HashMap<NormRelayUrl, u64>,
    statuses: HashMap<NormRelayUrl, RelayStatus>,
    publish: RelayPublishRuntime,
    pub(super) websockets: HashMap<NormRelayUrl, ServiceWebsocketLeg>,
    pub(super) reconnects: HashMap<NormRelayUrl, RelayReconnectState>,
    pub(super) multicast: MulticastTransportRuntime,
}

impl RelayTransportRuntime {
    pub(super) fn apply_pool_demand(&mut self, change: RelayDemandChanged) {
        if let Some(demand) = change.demand {
            self.pool_demands.insert(change.relay, demand);
        } else {
            self.pool_demands.remove(&change.relay);
        }
    }

    pub(super) fn apply_full_history_pending_demand(
        &mut self,
        relay: NormRelayUrl,
        demand: Option<RelayTransportDemand>,
    ) -> bool {
        let previous = self.raw_full_history_pending_demands.get(&relay).copied();
        if previous == demand {
            return self.refresh_full_history_pending_projection(&relay);
        }

        if let Some(demand) = demand {
            self.raw_full_history_pending_demands
                .insert(relay.clone(), demand);
        } else {
            self.raw_full_history_pending_demands.remove(&relay);
        }
        self.refresh_full_history_pending_projection(&relay)
    }

    pub(super) fn apply_negentropy_demand(
        &mut self,
        relay: NormRelayUrl,
        demand: Option<RelayTransportDemand>,
    ) -> bool {
        let previous = self.raw_negentropy_demands.get(&relay).copied();
        if previous == demand {
            return self.refresh_negentropy_projection(&relay);
        }

        if let Some(demand) = demand {
            self.raw_negentropy_demands.insert(relay.clone(), demand);
        } else {
            self.raw_negentropy_demands.remove(&relay);
        }
        self.refresh_negentropy_projection(&relay)
    }

    pub(super) fn set_subids_supported(
        &mut self,
        relay: &NormRelayUrl,
        subids_supported: bool,
    ) -> bool {
        if subids_supported {
            self.subids_unsupported.remove(relay);
        } else {
            self.subids_unsupported.insert(relay.clone());
        }
        self.refresh_full_history_pending_projection(relay)
            | self.refresh_negentropy_projection(relay)
    }

    pub(super) fn subids_supported(&self, relay: &NormRelayUrl) -> bool {
        !self.subids_unsupported.contains(relay)
    }

    fn refresh_full_history_pending_projection(&mut self, relay: &NormRelayUrl) -> bool {
        let demand = self
            .subids_supported(relay)
            .then(|| self.raw_full_history_pending_demands.get(relay).copied())
            .flatten();
        let previous = self.full_history_pending_demands.get(relay).copied();
        if previous == demand {
            return false;
        }

        if let Some(demand) = demand {
            self.full_history_pending_demands
                .insert(relay.clone(), demand);
        } else {
            self.full_history_pending_demands.remove(relay);
        }
        true
    }

    fn refresh_negentropy_projection(&mut self, relay: &NormRelayUrl) -> bool {
        let demand = self
            .subids_supported(relay)
            .then(|| self.raw_negentropy_demands.get(relay).copied())
            .flatten();
        let previous = self.negentropy_demands.get(relay).copied();
        if previous == demand {
            return false;
        }

        if let Some(demand) = demand {
            self.negentropy_demands.insert(relay.clone(), demand);
        } else {
            self.negentropy_demands.remove(relay);
        }
        true
    }

    pub(super) fn set_status(
        &mut self,
        relay: NormRelayUrl,
        status: Option<RelayStatus>,
    ) -> OutboxServiceOutput {
        let changed = match status {
            Some(status) => self.statuses.insert(relay.clone(), status) != Some(status),
            None => self.statuses.remove(&relay).is_some(),
        };
        if !changed {
            return OutboxServiceOutput::NoEvents;
        }

        OutboxServiceOutput::Events(vec![OutboxEvent::RelayStatusChanged { relay, status }])
    }

    pub(super) fn queue_publish(&mut self, relay: NormRelayUrl, msg: EventClientMessage) {
        self.publish.queue(relay, msg);
    }

    pub(super) fn drain_publish_queue_for_generation(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
    ) {
        if let Some(leg) = self
            .websockets
            .get_mut(relay)
            .filter(|leg| leg.generation == generation)
        {
            self.publish.flush(relay, leg);
        }
    }

    pub(super) fn demanded_relays(&self) -> Vec<NormRelayUrl> {
        let mut relays = self.pool_demands.keys().cloned().collect::<Vec<_>>();
        relays.extend(self.full_history_pending_demands.keys().cloned());
        relays.extend(self.negentropy_demands.keys().cloned());
        relays.extend(self.publish.pending_relays());
        relays.sort_unstable();
        relays.dedup();
        relays
    }

    /// Return relays whose subscription-id-bearing work can consume NIP-11 limits.
    pub(super) fn nip11_candidate_relays(&self) -> Vec<NormRelayUrl> {
        let mut relays = self.pool_demands.keys().cloned().collect::<Vec<_>>();
        relays.extend(self.full_history_pending_demands.keys().cloned());
        relays.extend(self.negentropy_demands.keys().cloned());
        relays.sort_unstable();
        relays.dedup();
        relays
    }

    pub(super) fn demand_for(&self, relay: &NormRelayUrl) -> Option<RelayTransportDemand> {
        let publish_demand = self
            .publish
            .has_pending(relay)
            .then_some(RelayTransportDemand {
                priority: RelayConnectionPriority {
                    strongest_demand: RelayDemandPriority::Important,
                    request_count: 1,
                },
                source: RelayUrlSource::Explicit,
                connection_weight: 0,
            });
        RelayTransportDemand::merge_optional(
            RelayTransportDemand::merge_optional(
                self.pool_demands.get(relay).copied(),
                RelayTransportDemand::merge_optional(
                    self.full_history_pending_demands.get(relay).copied(),
                    self.negentropy_demands.get(relay).copied(),
                ),
            ),
            publish_demand,
        )
    }

    pub(super) fn connecting_websocket_count(&self) -> usize {
        self.websockets
            .values()
            .filter(|leg| leg.conn.status == RelayStatus::Connecting)
            .count()
    }

    pub(super) fn websocket_is_connecting(&self, relay: &NormRelayUrl) -> bool {
        self.websockets
            .get(relay)
            .is_some_and(|leg| leg.conn.status == RelayStatus::Connecting)
    }

    pub(super) fn next_generation(&mut self, relay: &NormRelayUrl) -> u64 {
        let next = self.next_generations.entry(relay.clone()).or_default();
        let generation = *next;
        *next = next
            .checked_add(1)
            .expect("relay transport generation overflow");
        generation
    }

    #[cfg(test)]
    pub(super) fn has_pending_publish(&self, relay: &NormRelayUrl) -> bool {
        self.publish.has_pending(relay)
    }
}

#[derive(Default)]
struct RelayPublishRuntime {
    queues: HashMap<NormRelayUrl, VecDeque<EventClientMessage>>,
}

impl RelayPublishRuntime {
    fn queue(&mut self, relay: NormRelayUrl, msg: EventClientMessage) {
        self.queues.entry(relay).or_default().push_back(msg);
    }

    fn flush(&mut self, relay: &NormRelayUrl, leg: &mut ServiceWebsocketLeg) {
        if !leg.is_connected() {
            return;
        }
        let Some(queue) = self.queues.get_mut(relay) else {
            return;
        };
        while let Some(msg) = queue.pop_front() {
            leg.send_event(msg);
        }
        if queue.is_empty() {
            self.queues.remove(relay);
        }
    }

    fn pending_relays(&self) -> Vec<NormRelayUrl> {
        let mut relays = self.queues.keys().cloned().collect::<Vec<_>>();
        relays.sort_unstable();
        relays
    }

    fn has_pending(&self, relay: &NormRelayUrl) -> bool {
        self.queues
            .get(relay)
            .is_some_and(|queue| !queue.is_empty())
    }
}
