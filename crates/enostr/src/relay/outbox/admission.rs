use super::fd_pressure::{
    RelayAdmissionPolicy, RelayOpenContext, RelayOpenDecision, RelaySocketDemand,
};
use crate::relay::RelayConnectionPriority;

/// Outbox-local websocket admission batch.
///
/// `pressure` owns process/caller websocket pressure. Connecting-leg projection
/// stays here because it is outbox transport admission state, not process
/// fd-pressure state.
pub(super) struct OutboxOpenAdmission {
    pressure: RelayOpenContext,
    connecting: ConnectingOpenContext,
}

/// Policy snapshot used to decide whether an admission deferral still applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct OutboxAdmissionPolicy {
    pressure: RelayAdmissionPolicy,
    connecting: ConnectingAdmissionPolicy,
}

/// Outbox transport projection for websocket legs still completing handshake.
struct ConnectingOpenContext {
    connecting_websockets: usize,
    max_connecting_websockets: usize,
    delta: isize,
}

/// Policy snapshot for Connecting-leg pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConnectingAdmissionPolicy {
    projected_connecting_websockets: usize,
    max_connecting_websockets: usize,
}

impl ConnectingOpenContext {
    fn new(connecting_websockets: usize, max_connecting_websockets: usize) -> Self {
        Self {
            connecting_websockets,
            max_connecting_websockets,
            delta: 0,
        }
    }

    fn policy(&self) -> ConnectingAdmissionPolicy {
        ConnectingAdmissionPolicy {
            projected_connecting_websockets: self.projected_count(0),
            max_connecting_websockets: self.max_connecting_websockets,
        }
    }

    fn allows_open_after_evictions(&self, evictions: usize) -> bool {
        let evictions = isize::try_from(evictions).unwrap_or(isize::MAX);
        let projected_open_delta = 1isize.saturating_sub(evictions);
        self.projected_count(projected_open_delta) <= self.max_connecting_websockets
    }

    fn blocks_without_timer(&self) -> bool {
        self.projected_count(0) >= self.max_connecting_websockets
    }

    fn record_open(&mut self) {
        self.delta = self.delta.saturating_add(1);
    }

    fn record_eviction(&mut self, was_connecting: bool) {
        if was_connecting {
            self.delta = self.delta.saturating_sub(1);
        }
    }

    fn projected_count(&self, new_delta: isize) -> usize {
        let delta = self.delta.saturating_add(new_delta);
        if delta >= 0 {
            return self.connecting_websockets.saturating_add(delta as usize);
        }

        self.connecting_websockets
            .saturating_sub(delta.unsigned_abs())
    }
}

impl OutboxOpenAdmission {
    pub(super) fn new(
        pressure: RelayOpenContext,
        connecting_websockets: usize,
        max_connecting_websockets: usize,
    ) -> Self {
        Self {
            pressure,
            connecting: ConnectingOpenContext::new(
                connecting_websockets,
                max_connecting_websockets,
            ),
        }
    }

    pub(super) fn policy(&self) -> OutboxAdmissionPolicy {
        OutboxAdmissionPolicy {
            pressure: self.pressure.policy(),
            connecting: self.connecting.policy(),
        }
    }

    pub(super) fn decide(&self, demand: RelaySocketDemand) -> RelayOpenDecision {
        self.pressure.decide(demand)
    }

    pub(super) fn websocket_limit_allows_open_after_evictions(&self, evictions: usize) -> bool {
        self.pressure
            .websocket_limit_allows_open_after_evictions(evictions)
    }

    pub(super) fn connecting_limit_allows_open_after_evictions(&self, evictions: usize) -> bool {
        self.connecting.allows_open_after_evictions(evictions)
    }

    pub(super) fn connecting_limit_blocks_without_timer(&self) -> bool {
        self.connecting.blocks_without_timer()
    }

    pub(super) fn low_value_open_allowed_without_eviction(
        &self,
        priority: RelayConnectionPriority,
    ) -> bool {
        self.pressure
            .low_value_open_allowed_without_eviction(priority)
            && self.connecting_limit_allows_open_after_evictions(0)
    }

    pub(super) fn record_socket_open(&mut self) {
        self.pressure.record_socket_open();
        self.connecting.record_open();
    }

    pub(super) fn record_socket_eviction(&mut self, was_connecting: bool) {
        self.pressure.record_socket_eviction();
        self.connecting.record_eviction(was_connecting);
    }

    pub(super) fn should_shed_for_websocket_limit(&self) -> bool {
        self.pressure.should_shed_for_websocket_limit()
    }
}
