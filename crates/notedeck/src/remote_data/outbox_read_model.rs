use std::collections::BTreeMap;

use enostr::{
    NormRelayUrl, OutboxEvent, OutboxSubId, OutboxSubRelayEose, RelayReqStatus, RelayStatus,
};
use hashbrown::HashMap;

/// UI-thread snapshot of committed outbox facts emitted by the bridge.
#[derive(Default)]
pub(crate) struct RemoteOutboxReadModel {
    relay_statuses: BTreeMap<NormRelayUrl, RelayStatus>,
    relay_req_statuses: HashMap<(OutboxSubId, NormRelayUrl), RelayReqStatus>,
    sub_relay_eose: HashMap<OutboxSubId, OutboxSubRelayEose>,
}

impl RemoteOutboxReadModel {
    pub(crate) fn apply_event(&mut self, event: OutboxEvent) {
        match event {
            OutboxEvent::RelayStatusChanged { relay, status } => {
                if let Some(status) = status {
                    self.relay_statuses.insert(relay, status);
                } else {
                    self.relay_statuses.remove(&relay);
                }
            }
            OutboxEvent::RelayReqStatusChanged { id, relay, status } => {
                if let Some(status) = status {
                    self.relay_req_statuses.insert((id, relay), status);
                } else {
                    self.relay_req_statuses.remove(&(id, relay));
                }
            }
            OutboxEvent::OutboxSubRelayEoseChanged { id, relay_eose } => {
                if let Some(relay_eose) = relay_eose {
                    self.sub_relay_eose.insert(id, relay_eose);
                } else {
                    self.sub_relay_eose.remove(&id);
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn committed_relay_req_statuses(
        &self,
        id: &OutboxSubId,
    ) -> HashMap<NormRelayUrl, RelayReqStatus> {
        self.relay_req_statuses
            .iter()
            .filter_map(|((status_id, relay), status)| {
                (status_id == id).then_some((relay.clone(), *status))
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn outbox_sub_relay_eose(&self, id: &OutboxSubId) -> Option<OutboxSubRelayEose> {
        self.sub_relay_eose.get(id).copied()
    }

    pub(crate) fn websocket_statuses(
        &self,
    ) -> impl Iterator<Item = (&NormRelayUrl, RelayStatus)> + '_ {
        self.relay_statuses
            .iter()
            .map(|(relay, status)| (relay, *status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_model_applies_bridge_facts_and_derives_active_websockets() {
        let relay_a = NormRelayUrl::new("wss://relay-a-read-model.example.com").expect("relay");
        let relay_b = NormRelayUrl::new("wss://relay-b-read-model.example.com").expect("relay");
        let id = OutboxSubId(42);
        let relay_eose = OutboxSubRelayEose {
            tracked_relays: 1,
            unsupported_relays: 0,
            any_eose: true,
            all_eosed: true,
        };

        let mut model = RemoteOutboxReadModel::default();
        model.apply_event(OutboxEvent::RelayStatusChanged {
            relay: relay_a.clone(),
            status: Some(RelayStatus::Connected),
        });
        model.apply_event(OutboxEvent::RelayStatusChanged {
            relay: relay_b.clone(),
            status: Some(RelayStatus::Disconnected),
        });
        model.apply_event(OutboxEvent::RelayReqStatusChanged {
            id,
            relay: relay_a.clone(),
            status: Some(RelayReqStatus::Eose),
        });
        model.apply_event(OutboxEvent::OutboxSubRelayEoseChanged {
            id,
            relay_eose: Some(relay_eose),
        });

        assert_eq!(
            model
                .websocket_statuses()
                .collect::<BTreeMap<_, _>>()
                .get(&relay_b)
                .copied(),
            Some(RelayStatus::Disconnected)
        );
        assert_eq!(
            model
                .committed_relay_req_statuses(&id)
                .get(&relay_a)
                .copied(),
            Some(RelayReqStatus::Eose)
        );
        assert_eq!(model.outbox_sub_relay_eose(&id), Some(relay_eose));

        model.apply_event(OutboxEvent::RelayReqStatusChanged {
            id,
            relay: relay_a.clone(),
            status: None,
        });
        model.apply_event(OutboxEvent::OutboxSubRelayEoseChanged {
            id,
            relay_eose: None,
        });
        model.apply_event(OutboxEvent::RelayStatusChanged {
            relay: relay_a.clone(),
            status: None,
        });

        assert!(model.committed_relay_req_statuses(&id).is_empty());
        assert_eq!(model.outbox_sub_relay_eose(&id), None);
        assert!(model
            .websocket_statuses()
            .collect::<BTreeMap<_, _>>()
            .get(&relay_a)
            .copied()
            .is_none());
    }
}
