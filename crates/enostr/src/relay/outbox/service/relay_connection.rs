use std::time::{Instant, SystemTime};

use tokio::sync::mpsc;

use super::admission_runtime::RelayAdmissionRuntime;
use super::admission_runtime::RelayOpenAdmissionCounts;
use super::nip11::{Nip11InterestRank, Nip11InterestRead, Nip11InterestState, Nip11ReadinessInput};
use super::transport::RelayTransportRuntime;
use super::{nip11_source_rank, OutboxServiceConfig, RelayTransportReady};
use crate::relay::{outbox::RelayTransportDemand, NormRelayUrl};

/// Owns relay transport state, admission state, and transport wake channels.
pub(super) struct RelayConnectionRuntime {
    pub(super) admission: RelayAdmissionRuntime,
    pub(super) transport: RelayTransportRuntime,
    pub(super) config: OutboxServiceConfig,
    pub(super) transport_ready_tx: mpsc::UnboundedSender<RelayTransportReady>,
    pub(super) transport_ready_rx: mpsc::UnboundedReceiver<RelayTransportReady>,
    pub(super) multicast_ready_tx: mpsc::UnboundedSender<()>,
    pub(super) multicast_ready_rx: mpsc::UnboundedReceiver<()>,
}

impl RelayConnectionRuntime {
    pub(super) fn new(config: OutboxServiceConfig) -> Self {
        let (transport_ready_tx, transport_ready_rx) = mpsc::unbounded_channel();
        let (multicast_ready_tx, multicast_ready_rx) = mpsc::unbounded_channel();
        Self {
            admission: RelayAdmissionRuntime::default(),
            transport: RelayTransportRuntime::default(),
            config,
            transport_ready_tx,
            transport_ready_rx,
            multicast_ready_tx,
            multicast_ready_rx,
        }
    }

    pub(super) fn nip11_readiness_input(
        &self,
        service_now: Instant,
        fetch_now: SystemTime,
    ) -> Nip11ReadinessInput {
        let mut relays = self.transport.nip11_candidate_relays();
        relays.extend(self.transport.websockets.keys().cloned());
        relays.sort_unstable();
        relays.dedup();

        let interests = relays
            .into_iter()
            .filter_map(|relay| self.nip11_interest_read(relay, service_now, fetch_now))
            .collect();

        Nip11ReadinessInput {
            now: fetch_now,
            interests,
        }
    }

    fn nip11_interest_read(
        &self,
        relay: NormRelayUrl,
        service_now: Instant,
        fetch_now: SystemTime,
    ) -> Option<Nip11InterestRead> {
        let demand = self.transport.demand_for(&relay);
        let has_websocket = self.transport.websockets.contains_key(&relay);
        if demand.is_none() && !has_websocket {
            return None;
        }

        Some(Nip11InterestRead {
            rank: self.nip11_interest_rank(&relay, demand, service_now),
            state: self.nip11_interest_state(&relay, demand, has_websocket, service_now, fetch_now),
            relay,
        })
    }

    fn nip11_interest_rank(
        &self,
        relay: &NormRelayUrl,
        demand: Option<RelayTransportDemand>,
        service_now: Instant,
    ) -> Nip11InterestRank {
        Nip11InterestRank {
            priority: demand.map(|demand| demand.priority),
            source_rank: demand.map(nip11_source_rank).unwrap_or(0),
            connection_weight: demand
                .map(|demand| demand.connection_weight)
                .unwrap_or_default(),
            health_rank: self
                .admission
                .low_value_health_rank(relay, demand, service_now),
        }
    }

    fn nip11_interest_state(
        &self,
        relay: &NormRelayUrl,
        demand: Option<RelayTransportDemand>,
        has_websocket: bool,
        service_now: Instant,
        fetch_now: SystemTime,
    ) -> Nip11InterestState {
        if has_websocket {
            return Nip11InterestState::Active;
        }

        let Some(demand) = demand else {
            return Nip11InterestState::Active;
        };
        if !demand.low_value_remote_advertised() {
            return Nip11InterestState::Active;
        }

        self.admission.low_value_nip11_interest_state(
            relay,
            demand,
            RelayOpenAdmissionCounts::new(
                self.transport.websockets.len(),
                self.transport.connecting_websocket_count(),
                self.config.max_connecting_websockets,
            ),
            service_now,
            fetch_now,
        )
    }
}
