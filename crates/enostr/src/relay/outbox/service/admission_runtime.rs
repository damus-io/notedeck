use std::time::{Instant, SystemTime};

use super::{
    next_backoff_duration_from_base,
    nip11::{Nip11InterestResume, Nip11InterestState},
    relay_transport_value_cmp, ADMISSION_DEFER_BACKOFF_BASE, MAX_ADMISSION_DEFER_BACKOFF,
};
use crate::relay::outbox::{
    admission::{OutboxAdmissionPolicy, OutboxOpenAdmission},
    LowValueOpenBackoffReason, RelayAdmissionState, RelayTransportDemand,
};
use crate::relay::{backoff, NormRelayUrl};

#[derive(Default)]
pub(super) struct RelayAdmissionRuntime {
    pub(super) state: RelayAdmissionState,
}

/// Websocket counts used to project one relay-open admission turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RelayOpenAdmissionCounts {
    websocket_count: usize,
    connecting_websocket_count: usize,
    max_connecting_websockets: usize,
}

impl RelayOpenAdmissionCounts {
    pub(super) fn new(
        websocket_count: usize,
        connecting_websocket_count: usize,
        max_connecting_websockets: usize,
    ) -> Self {
        Self {
            websocket_count,
            connecting_websocket_count,
            max_connecting_websockets,
        }
    }
}

impl RelayAdmissionRuntime {
    pub(super) fn set_max_websocket_connections(&mut self, max: Option<usize>) {
        self.state.set_max_websocket_connections(max);
    }

    pub(super) fn start_open_admission_at(
        &mut self,
        counts: RelayOpenAdmissionCounts,
        now: Instant,
    ) -> OutboxOpenAdmission {
        let admission = self.state.fd_pressure.start_batch_at(
            counts.websocket_count,
            self.state.max_websocket_connections,
            now,
        );
        OutboxOpenAdmission::new(
            admission,
            counts.connecting_websocket_count,
            counts.max_connecting_websockets,
        )
    }

    pub(super) fn low_value_nip11_interest_state(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        counts: RelayOpenAdmissionCounts,
        service_now: Instant,
        fetch_now: SystemTime,
    ) -> Nip11InterestState {
        if !demand.low_value_remote_advertised() {
            return Nip11InterestState::Active;
        }

        if let Some(retry_at) = self.low_value_health_retry_at(relay, demand, service_now) {
            return Nip11InterestState::Suspended(Nip11InterestResume::At(instant_to_system_time(
                service_now,
                fetch_now,
                retry_at,
            )));
        }

        let admission = self.read_open_admission_at(counts, service_now);
        if let Some(retry_at) =
            self.deferral_retry_at(relay, demand, admission.policy(), service_now)
        {
            return Nip11InterestState::Suspended(Nip11InterestResume::At(instant_to_system_time(
                service_now,
                fetch_now,
                retry_at,
            )));
        }

        if admission.low_value_open_allowed_without_eviction(demand.priority) {
            return Nip11InterestState::Active;
        }

        if self.websocket_cap_blocks_without_timer(counts.websocket_count)
            || admission.connecting_limit_blocks_without_timer()
        {
            return Nip11InterestState::Suspended(Nip11InterestResume::OnRelayInput);
        }

        self.nip11_policy_suspension(service_now, fetch_now)
    }

    fn read_open_admission_at(
        &self,
        counts: RelayOpenAdmissionCounts,
        now: Instant,
    ) -> OutboxOpenAdmission {
        let admission = self.state.fd_pressure.read_batch_at(
            counts.websocket_count,
            self.state.max_websocket_connections,
            now,
        );
        OutboxOpenAdmission::new(
            admission,
            counts.connecting_websocket_count,
            counts.max_connecting_websockets,
        )
    }

    pub(super) fn low_value_health_rank(
        &self,
        relay: &NormRelayUrl,
        demand: Option<RelayTransportDemand>,
        now: Instant,
    ) -> (bool, u32) {
        if !demand.is_some_and(RelayTransportDemand::low_value_remote_advertised) {
            return (false, 0);
        }

        self.state
            .transport_health
            .get(relay)
            .map(|health| {
                (
                    health.blocks_low_value_open(now),
                    health.low_value_retry_attempts,
                )
            })
            .unwrap_or((false, 0))
    }

    fn low_value_health_retry_at(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        now: Instant,
    ) -> Option<Instant> {
        if !demand.low_value_remote_advertised() {
            return None;
        }

        self.state
            .transport_health
            .get(relay)
            .and_then(|health| health.low_value_retry_at)
            .filter(|retry_at| now < *retry_at)
    }

    pub(super) fn low_value_retry_attempts(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
    ) -> u32 {
        if !demand.low_value_remote_advertised() {
            return 0;
        }
        self.state
            .transport_health
            .get(relay)
            .map(|health| health.low_value_retry_attempts)
            .unwrap_or(0)
    }

    pub(super) fn transport_health_blocks_open(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        now: Instant,
    ) -> bool {
        demand.low_value_remote_advertised()
            && self
                .state
                .transport_health
                .get(relay)
                .is_some_and(|health| health.blocks_low_value_open(now))
    }

    pub(super) fn deferral_blocks_demand(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        admission: &OutboxOpenAdmission,
        now: Instant,
    ) -> bool {
        let Some(deferral) = self.state.deferrals.get(relay) else {
            return false;
        };

        now < deferral.retry_at
            && relay_transport_value_cmp(demand, deferral.demand) != std::cmp::Ordering::Greater
            && admission.policy() == deferral.policy
            && self.state.generation == deferral.generation
    }

    fn deferral_retry_at(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        admission_policy: OutboxAdmissionPolicy,
        now: Instant,
    ) -> Option<Instant> {
        let deferral = self.state.deferrals.get(relay)?;
        (now < deferral.retry_at
            && relay_transport_value_cmp(demand, deferral.demand) != std::cmp::Ordering::Greater
            && admission_policy == deferral.policy
            && self.state.generation == deferral.generation)
            .then_some(deferral.retry_at)
    }

    fn websocket_cap_blocks_without_timer(&self, websocket_count: usize) -> bool {
        self.state
            .max_websocket_connections
            .is_some_and(|limit| websocket_count >= limit)
    }

    fn nip11_policy_suspension(
        &self,
        service_now: Instant,
        fetch_now: SystemTime,
    ) -> Nip11InterestState {
        self.state
            .fd_pressure
            .next_policy_refresh_deadline(service_now)
            .map(|deadline| {
                Nip11InterestState::Suspended(Nip11InterestResume::At(instant_to_system_time(
                    service_now,
                    fetch_now,
                    deadline,
                )))
            })
            .unwrap_or(Nip11InterestState::Suspended(
                Nip11InterestResume::OnRelayInput,
            ))
    }

    pub(super) fn clear_deferral(&mut self, relay: &NormRelayUrl) {
        self.state.deferrals.remove(relay);
    }

    pub(super) fn bump_generation(&mut self) {
        self.state.bump_generation();
    }

    pub(super) fn record_deferral(
        &mut self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        admission: &OutboxOpenAdmission,
        now: Instant,
    ) {
        let policy = admission.policy();
        let attempt = self
            .state
            .deferrals
            .get(relay)
            .filter(|deferral| {
                deferral.policy == policy
                    && deferral.demand == demand
                    && deferral.generation == self.state.generation
            })
            .map(|deferral| deferral.attempt.saturating_add(1))
            .unwrap_or(0);
        let retry_after = next_backoff_duration_from_base(
            attempt,
            ADMISSION_DEFER_BACKOFF_BASE,
            backoff::jitter_seed(relay, attempt),
            MAX_ADMISSION_DEFER_BACKOFF,
        );

        self.state.deferrals.insert(
            relay.clone(),
            crate::relay::outbox::RelayAdmissionDeferral {
                retry_at: now + retry_after,
                attempt,
                demand,
                policy,
                generation: self.state.generation,
            },
        );
    }

    pub(super) fn apply_transport_health_deadline(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        transport_deadline: Instant,
        now: Instant,
    ) -> Instant {
        if !demand.low_value_remote_advertised() {
            return transport_deadline;
        }

        let Some(health) = self.state.transport_health.get(relay) else {
            return transport_deadline;
        };
        let Some(retry_at) = health.low_value_retry_at else {
            return transport_deadline;
        };

        if now < retry_at {
            transport_deadline.max(retry_at)
        } else {
            transport_deadline
        }
    }

    pub(super) fn apply_deferral_deadline(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        transport_deadline: Instant,
        admission_policy: OutboxAdmissionPolicy,
        now: Instant,
    ) -> Instant {
        if transport_deadline > now {
            return transport_deadline;
        }

        let Some(deferral) = self.state.deferrals.get(relay) else {
            return transport_deadline;
        };
        if now < deferral.retry_at
            && relay_transport_value_cmp(demand, deferral.demand) != std::cmp::Ordering::Greater
            && admission_policy == deferral.policy
            && self.state.generation == deferral.generation
        {
            return self
                .state
                .fd_pressure
                .next_policy_refresh_deadline(now)
                .map(|refresh_at| refresh_at.min(deferral.retry_at))
                .unwrap_or(deferral.retry_at);
        }

        transport_deadline
    }

    pub(super) fn note_transport_connected(&mut self, relay: &NormRelayUrl) {
        self.state.fd_pressure.clear_hard_failure_on_open_success();
        let should_remove = if let Some(health) = self.state.transport_health.get_mut(relay) {
            health.note_success();
            *health == Default::default()
        } else {
            false
        };
        if should_remove {
            self.state.transport_health.remove(relay);
        }
    }

    pub(super) fn enter_hard_failure_from_websocket_error(
        &mut self,
        error: &crate::WebSocketError,
    ) -> bool {
        self.state.enter_hard_failure_from_websocket_error(error)
    }

    pub(super) fn note_low_value_transport_retry(
        &mut self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        now: Instant,
        reason: LowValueOpenBackoffReason,
    ) {
        if !demand.low_value_remote_advertised() {
            return;
        }

        self.state
            .transport_health
            .entry(relay.clone())
            .or_default()
            .note_low_value_retry(relay, now, reason);
    }
}

fn instant_to_system_time(
    reference_instant: Instant,
    reference_system: SystemTime,
    instant: Instant,
) -> SystemTime {
    if instant >= reference_instant {
        return reference_system
            .checked_add(instant.duration_since(reference_instant))
            .unwrap_or(reference_system);
    }

    reference_system
        .checked_sub(reference_instant.duration_since(instant))
        .unwrap_or(reference_system)
}
