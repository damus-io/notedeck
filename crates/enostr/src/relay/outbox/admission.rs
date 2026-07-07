use super::fd_pressure::{
    RelayAdmissionPolicy, RelayOpenContext, RelayOpenDecision, RelaySocketDemand,
};

/// Outbox-local websocket admission batch.
///
/// `pressure` owns process/caller websocket pressure. The outbox layer adds
/// relay demand policy that fd pressure must not know about.
pub(super) struct OutboxOpenAdmission {
    pressure: RelayOpenContext,
}

impl OutboxOpenAdmission {
    pub(super) fn new(pressure: RelayOpenContext) -> Self {
        Self { pressure }
    }

    pub(super) fn policy(&self) -> RelayAdmissionPolicy {
        self.pressure.policy()
    }

    pub(super) fn decide(&self, demand: RelaySocketDemand) -> RelayOpenDecision {
        self.pressure.decide(demand)
    }

    pub(super) fn websocket_limit_allows_open_after_evictions(&self, evictions: usize) -> bool {
        self.pressure
            .websocket_limit_allows_open_after_evictions(evictions)
    }

    pub(super) fn record_socket_open(&mut self) {
        self.pressure.record_socket_open();
    }

    pub(super) fn record_socket_eviction(&mut self) {
        self.pressure.record_socket_eviction();
    }

    pub(super) fn should_shed_for_websocket_limit(&self) -> bool {
        self.pressure.should_shed_for_websocket_limit()
    }
}
