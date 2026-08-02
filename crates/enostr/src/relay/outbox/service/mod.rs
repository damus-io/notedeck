mod admission_runtime;
mod capability_runtime;
mod effect_turn;
pub(in crate::relay::outbox) mod full_history_runtime;
mod nip11;
mod relay_connection;
mod transport;

use std::{
    cmp::Ordering,
    future::{self, Future},
    time::{Duration, Instant, SystemTime},
};

use admission_runtime::RelayOpenAdmissionCounts;
use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;

use super::{
    admission::{OutboxAdmissionPolicy, OutboxOpenAdmission},
    fd_pressure::{RelayOpenDecision, RelaySocketDemand},
};
use super::{
    FullHistoryOutput, FullHistoryRuntime, LowValueOpenBackoffReason, OutboxFullHistoryEffect,
    OutboxIdRegistry, OutboxPool, OutboxPoolFact, OutboxPoolOutput, OutboxSubRelayEose,
    OutboxTransportEffect, RelayConnectionDropReason, RelayTransportDemand,
    DEFAULT_KEEPALIVE_PING_RATE, DEFAULT_RECONNECT_BACKOFF_BASE, DEFAULT_RECONNECT_DELAY,
    MAX_RECONNECT_DELAY, PONG_TIMEOUT,
};
use crate::relay::limits::RelayLimitCaps;
use crate::relay::negentropy::NegentropyRuntime;
use crate::relay::{
    backoff,
    message::RelayMessage,
    normalize_full_history_targets,
    subscription::{FullHistoryTask, FullHistoryUpsertTask},
    FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult, FullHistoryLocalSetRequest,
    FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
    FullHistorySubId, FullHistoryTarget, Nip11ApplyOutcome, Nip11LimitationsRaw, NormRelayUrl,
    OutboxSubId, RelayConnectionPriority, RelayDemandPriority, RelayId, RelayImplType,
    RelayLimitations, RelayReqId, RelayReqStatus, RelayStatus, RelayUrlPkgs, RelayUrlSource,
    WebsocketConn,
};
use crate::EventClientMessage;
use capability_runtime::CapabilityRuntime;
use effect_turn::OutboxServiceOutputs;
use ewebsock::{WsEvent, WsMessage};
use full_history_runtime::{
    FullHistoryRuntimeDeadline, FullHistoryRuntimeDeadlineInput, FullHistoryRuntimeOutput,
};
use nip11::{Nip11ApplyAck, Nip11Service, Nip11ServiceOutput};
pub use nip11::{Nip11Capability, Nip11FetchRequest};
use relay_connection::RelayConnectionRuntime;
use transport::{RelayReconnectState, ServiceWebsocketLeg};

const ADMISSION_DEFER_BACKOFF_BASE: Duration = Duration::from_secs(1);
const MAX_ADMISSION_DEFER_BACKOFF: Duration = Duration::from_secs(30);
const DEFAULT_MAX_CONNECTING_WEBSOCKETS: usize = 32;

pub struct FullHistoryLocalSetResult {
    pub history_id: FullHistorySubId,
    pub request_id: u64,
    pub result: Option<negentropy::NegentropyStorageVector>,
}

pub trait FullHistoryCapability: Send + Sync + 'static {
    type LocalSetOutput;
    type LocalSetFuture: Future<Output = Self::LocalSetOutput> + Send + 'static;
    type LocalPresenceOutput;
    type LocalPresenceFuture: Future<Output = Self::LocalPresenceOutput> + Send + 'static;
    type PendingIngestionPresenceOutput;
    type PendingIngestionPresenceFuture: Future<Output = Self::PendingIngestionPresenceOutput>
        + Send
        + 'static;

    fn build_local_set(&self, request: FullHistoryLocalSetRequest) -> Self::LocalSetFuture;
    fn check_local_presence(
        &self,
        request: FullHistoryLocalPresenceRequest,
    ) -> Self::LocalPresenceFuture;
    fn check_pending_ingestion_presence(
        &self,
        request: FullHistoryPendingIngestionPresenceRequest,
    ) -> Self::PendingIngestionPresenceFuture;
}

pub struct EventIngestRequest {
    pub relay_url: String,
    pub relay_type: RelayImplType,
    pub ingest_json: String,
}

pub trait EventIngestCapability: Send + Sync + 'static {
    type Future: Future<Output = ()> + Send + 'static;

    fn ingest_event(&self, request: EventIngestRequest) -> Self::Future;
}

enum CapabilityResult {
    FullHistoryLocalSet(FullHistoryLocalSetResult),
    FullHistoryLocalPresence(FullHistoryLocalPresenceResult),
    FullHistoryPendingIngestionPresence(FullHistoryPendingIngestionPresenceResult),
    EventIngest,
}

struct RelayTransportReady {
    relay: NormRelayUrl,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelayAdmissionRank {
    priority: RelayConnectionPriority,
    source_rank: u8,
    connection_weight: u32,
    health_rank: (bool, u32),
}

/// Public service event emitted after pool output is activated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutboxEvent {
    RelayStatusChanged {
        relay: NormRelayUrl,
        status: Option<RelayStatus>,
    },
    RelayReqStatusChanged {
        id: OutboxSubId,
        relay: NormRelayUrl,
        status: Option<RelayReqStatus>,
    },
    OutboxSubRelayEoseChanged {
        id: OutboxSubId,
        relay_eose: Option<OutboxSubRelayEose>,
    },
}

/// Public output from one service method or one `OutboxService::next()` turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutboxServiceOutput {
    NoEvents,
    Events(Vec<OutboxEvent>),
}

impl OutboxServiceOutput {
    fn from_events(events: Vec<OutboxEvent>) -> Self {
        if events.is_empty() {
            Self::NoEvents
        } else {
            Self::Events(events)
        }
    }

    fn has_events(&self) -> bool {
        matches!(self, Self::Events(_))
    }
}

fn merge_service_outputs(
    first: OutboxServiceOutput,
    second: OutboxServiceOutput,
) -> OutboxServiceOutput {
    match (first, second) {
        (OutboxServiceOutput::NoEvents, output) | (output, OutboxServiceOutput::NoEvents) => output,
        (OutboxServiceOutput::Events(mut first), OutboxServiceOutput::Events(second)) => {
            first.extend(second);
            OutboxServiceOutput::Events(first)
        }
    }
}

async fn sleep_until_deadline(deadline: Option<Instant>) {
    let Some(deadline) = deadline else {
        future::pending::<()>().await;
        return;
    };
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
}

fn system_time_deadline_to_instant(deadline: SystemTime, now: Instant) -> Instant {
    let delay = deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    now + delay
}

fn relay_notice_requires_auth(message: &str) -> bool {
    let message = message.trim_start();
    starts_with_ignore_ascii_case(message, "auth-required:")
        || starts_with_ignore_ascii_case(message, "auth required:")
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn unsupported_max_subid_length(raw: &Nip11LimitationsRaw) -> Option<usize> {
    let max_subid_length = raw
        .max_subid_length
        .and_then(valid_nonnegative_subid_length)?;
    (max_subid_length < RelayReqId::byte_len()).then_some(max_subid_length)
}

fn derive_relay_limitations_from_raw(
    raw: &Nip11LimitationsRaw,
    fallback: RelayLimitations,
) -> RelayLimitations {
    let mut out = fallback;
    let caps = RelayLimitCaps::default();

    if let Some(maximum_subs) = raw.max_subscriptions.and_then(valid_positive_usize) {
        out.maximum_subs = maximum_subs;
    }

    if let Some(max_json_bytes) = raw.max_message_length.and_then(valid_positive_usize) {
        out.max_json_bytes = max_json_bytes;
    }

    clamp_relay_limitations_to_caps(out, caps)
}

fn clamp_relay_limitations_to_caps(
    limitations: RelayLimitations,
    caps: RelayLimitCaps,
) -> RelayLimitations {
    let clamped = caps.clamp(limitations);

    if clamped.maximum_subs != limitations.maximum_subs {
        tracing::debug!(
            field = "max_subscriptions",
            value = limitations.maximum_subs,
            cap = caps.maximum_subs,
            "nip11: clamped remote relay limitation to local cap"
        );
    }

    if clamped.max_json_bytes != limitations.max_json_bytes {
        tracing::debug!(
            field = "max_message_length",
            value = limitations.max_json_bytes,
            cap = caps.max_json_bytes,
            "nip11: clamped remote relay limitation to local cap"
        );
    }

    clamped
}

fn valid_positive_usize(value: i64) -> Option<usize> {
    if value <= 0 {
        return None;
    }

    usize::try_from(value).ok()
}

fn valid_nonnegative_subid_length(value: i64) -> Option<usize> {
    if value < 0 {
        return None;
    }

    usize::try_from(value).ok()
}

fn nip11_source_rank(demand: RelayTransportDemand) -> u8 {
    match demand.source {
        RelayUrlSource::RemoteAdvertised => 0,
        RelayUrlSource::Explicit => 1,
    }
}

fn relay_transport_value_cmp(left: RelayTransportDemand, right: RelayTransportDemand) -> Ordering {
    left.priority
        .strongest_demand
        .cmp(&right.priority.strongest_demand)
        .then_with(|| nip11_source_rank(left).cmp(&nip11_source_rank(right)))
        .then_with(|| left.connection_weight.cmp(&right.connection_weight))
        .then_with(|| {
            left.priority
                .request_count
                .cmp(&right.priority.request_count)
        })
}

/// Construction-time settings for [`OutboxService`].
#[derive(Clone, Copy, Debug)]
pub struct OutboxServiceConfig {
    keepalive_ping_rate: Duration,
    pong_timeout: Duration,
    keepalive_reconnect_delay: Duration,
    keepalive_reconnect_backoff_base: Duration,
    max_connecting_websockets: usize,
}

impl Default for OutboxServiceConfig {
    fn default() -> Self {
        Self {
            keepalive_ping_rate: DEFAULT_KEEPALIVE_PING_RATE,
            pong_timeout: PONG_TIMEOUT,
            keepalive_reconnect_delay: DEFAULT_RECONNECT_DELAY,
            keepalive_reconnect_backoff_base: DEFAULT_RECONNECT_BACKOFF_BASE,
            max_connecting_websockets: DEFAULT_MAX_CONNECTING_WEBSOCKETS,
        }
    }
}

impl OutboxServiceConfig {
    /// Configure the ping cadence used by service-owned websocket liveness.
    pub fn with_keepalive_ping_rate(mut self, rate: Duration) -> Self {
        self.keepalive_ping_rate = rate;
        self
    }

    /// Configure the pong timeout used by service-owned websocket liveness.
    pub fn with_pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = timeout;
        self
    }

    /// Configure the base reconnect delay for disconnected relay transports.
    pub fn with_keepalive_reconnect_delay(mut self, delay: Duration) -> Self {
        self.keepalive_reconnect_delay = delay;
        self
    }

    /// Configure the reconnect backoff base for disconnected relay transports.
    pub fn with_keepalive_reconnect_backoff_base(mut self, base: Duration) -> Self {
        self.keepalive_reconnect_backoff_base = base;
        self
    }

    /// Configure the maximum number of service-owned websocket legs that may
    /// be in the Connecting state at once.
    pub fn with_max_connecting_websockets(mut self, max: usize) -> Self {
        self.max_connecting_websockets = max.max(1);
        self
    }
}

/// Enostr-owned outbox execution boundary.
pub struct OutboxService<N, F, E> {
    pub(in crate::relay::outbox) pool: OutboxPool,
    pub(in crate::relay::outbox) negentropy: NegentropyRuntime,
    relay: RelayConnectionRuntime,
    nip11: Nip11Service<N>,
    pub(in crate::relay::outbox) full_history: FullHistoryRuntime,
    capabilities: CapabilityRuntime<F, E>,
    outputs: OutboxServiceOutputs,
}

impl<N, F, E> OutboxService<N, F, E>
where
    N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
    F: FullHistoryCapability<
        LocalSetOutput = FullHistoryLocalSetResult,
        LocalPresenceOutput = FullHistoryLocalPresenceResult,
        PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
    >,
    E: EventIngestCapability,
{
    /// Build a service with one fresh id namespace and host capabilities.
    pub fn with_capabilities(
        nip11_capability: N,
        full_history_capability: F,
        event_ingest_capability: E,
    ) -> Self {
        Self::with_capabilities_and_config(
            nip11_capability,
            full_history_capability,
            event_ingest_capability,
            OutboxServiceConfig::default(),
        )
    }

    /// Build a service with host capabilities and service configuration.
    pub fn with_capabilities_and_config(
        nip11_capability: N,
        full_history_capability: F,
        event_ingest_capability: E,
        config: OutboxServiceConfig,
    ) -> Self {
        let ids = OutboxIdRegistry::new();
        Self {
            pool: OutboxPool::with_id_registry(ids.clone()),
            negentropy: NegentropyRuntime::default(),
            relay: RelayConnectionRuntime::new(config),
            nip11: Nip11Service::new(nip11_capability),
            full_history: FullHistoryRuntime::default(),
            capabilities: CapabilityRuntime::new(full_history_capability, event_ingest_capability),
            outputs: OutboxServiceOutputs::default(),
        }
    }

    /// Clone the service-owned concrete outbox id namespace.
    pub fn id_registry(&self) -> OutboxIdRegistry {
        self.pool.id_registry.clone()
    }

    /// Begin one effect turn that records pool output until `end_effect_turn`.
    pub fn begin_effect_turn(&mut self) {
        self.outputs.begin_effect_turn();
    }

    /// Finish one effect turn and activate its reduced output.
    pub fn end_effect_turn(&mut self) -> OutboxServiceOutput {
        let Some(output) = self.outputs.end_effect_turn() else {
            return OutboxServiceOutput::NoEvents;
        };
        self.activate_pool_output(output)
    }

    /// Configure websocket capacity through the currently converted pool
    /// transition.
    pub fn set_max_websocket_connections(&mut self, max: Option<usize>) -> OutboxServiceOutput {
        self.relay.admission.set_max_websocket_connections(max);
        self.enforce_service_websocket_connection_limit(Instant::now())
    }

    /// Return multicast deadline from current service-owned transport state.
    fn next_multicast_deadline(&self) -> Option<Instant> {
        self.relay.transport.multicast.next_maintenance_deadline()
    }

    /// Return the next service-owned NIP-11 deadline.
    fn next_nip11_deadline(
        &mut self,
        service_now: Instant,
        fetch_now: SystemTime,
    ) -> Option<SystemTime> {
        let input = self.relay.nip11_readiness_input(service_now, fetch_now);
        self.nip11.next_readiness_deadline(input)
    }

    /// Start currently ready NIP-11 capability work from service candidates.
    fn apply_nip11_readiness(&mut self) -> OutboxServiceOutput {
        let service_now = Instant::now();
        let fetch_now = SystemTime::now();
        let input = self.relay.nip11_readiness_input(service_now, fetch_now);
        self.nip11.apply_readiness(input);
        OutboxServiceOutput::NoEvents
    }

    fn nip11_health_rank(
        &self,
        relay: &NormRelayUrl,
        demand: Option<RelayTransportDemand>,
        now: Instant,
    ) -> (bool, u32) {
        self.relay
            .admission
            .low_value_health_rank(relay, demand, now)
    }

    fn start_service_open_admission_at(&mut self, now: Instant) -> OutboxOpenAdmission {
        self.relay.admission.start_open_admission_at(
            RelayOpenAdmissionCounts::new(
                self.relay.transport.websockets.len(),
                self.relay.transport.connecting_websocket_count(),
                self.relay.config.max_connecting_websockets,
            ),
            now,
        )
    }

    fn authorize_service_relay_open(
        &mut self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        admission: &mut OutboxOpenAdmission,
        now: Instant,
    ) -> Option<OutboxServiceOutput> {
        if self
            .relay
            .admission
            .transport_health_blocks_open(relay, demand, now)
        {
            return None;
        }
        if self
            .relay
            .admission
            .deferral_blocks_demand(relay, demand, admission, now)
        {
            return None;
        }

        let mut victims = Vec::new();
        if !admission.connecting_limit_allows_open_after_evictions(0) {
            let Some(victim) =
                self.select_service_connecting_eviction_candidate(relay, demand, &victims)
            else {
                self.relay
                    .admission
                    .record_deferral(relay, demand, admission, now);
                return None;
            };
            victims.push(victim);
        }

        match admission.decide(RelaySocketDemand::Prioritized(demand.priority)) {
            RelayOpenDecision::Open => {
                if !admission.websocket_limit_allows_open_after_evictions(victims.len()) {
                    self.relay
                        .admission
                        .record_deferral(relay, demand, admission, now);
                    return None;
                }
            }
            RelayOpenDecision::TryEvictThenOpen => {
                if victims.is_empty() {
                    if let Some(victim) =
                        self.select_service_eviction_candidate_excluding(relay, demand, &victims)
                    {
                        victims.push(victim);
                    }
                }
                if !admission.websocket_limit_allows_open_after_evictions(victims.len()) {
                    self.relay
                        .admission
                        .record_deferral(relay, demand, admission, now);
                    return None;
                }
            }
            RelayOpenDecision::RequireEviction => {
                if !admission.websocket_limit_allows_open_after_evictions(victims.len()) {
                    let Some(victim) =
                        self.select_service_eviction_candidate_excluding(relay, demand, &victims)
                    else {
                        self.relay
                            .admission
                            .record_deferral(relay, demand, admission, now);
                        return None;
                    };
                    victims.push(victim);
                }
                if !admission.websocket_limit_allows_open_after_evictions(victims.len()) {
                    self.relay
                        .admission
                        .record_deferral(relay, demand, admission, now);
                    return None;
                }
            }
            RelayOpenDecision::Defer => {
                self.relay
                    .admission
                    .record_deferral(relay, demand, admission, now);
                return None;
            }
        }

        let output = self.apply_service_admission_evictions(victims, admission, now);
        admission.record_socket_open();
        self.relay.admission.clear_deferral(relay);
        Some(output)
    }

    fn apply_service_admission_evictions(
        &mut self,
        victims: Vec<NormRelayUrl>,
        admission: &mut OutboxOpenAdmission,
        now: Instant,
    ) -> OutboxServiceOutput {
        if victims.is_empty() {
            return OutboxServiceOutput::NoEvents;
        }

        let mut output = OutboxServiceOutput::NoEvents;
        for victim in victims {
            let was_connecting = self.relay.transport.websocket_is_connecting(&victim);
            output = merge_service_outputs(
                output,
                self.evict_service_websocket_for_admission(&victim, now),
            );
            admission.record_socket_eviction(was_connecting);
        }
        self.relay.admission.bump_generation();
        output
    }

    fn enforce_service_websocket_connection_limit(&mut self, now: Instant) -> OutboxServiceOutput {
        let mut admission = self.start_service_open_admission_at(now);
        let mut output = OutboxServiceOutput::NoEvents;
        while admission.should_shed_for_websocket_limit() {
            let Some(victim) = self.select_lowest_service_websocket() else {
                break;
            };
            let was_connecting = self.relay.transport.websocket_is_connecting(&victim);
            output = merge_service_outputs(
                output,
                self.evict_service_websocket_for_admission(&victim, now),
            );
            admission.record_socket_eviction(was_connecting);
            self.relay.admission.bump_generation();
        }
        output
    }

    fn select_service_eviction_candidate_excluding(
        &self,
        incoming_relay: &NormRelayUrl,
        incoming: RelayTransportDemand,
        excluded: &[NormRelayUrl],
    ) -> Option<NormRelayUrl> {
        self.relay
            .transport
            .websockets
            .keys()
            .filter(|relay| *relay != incoming_relay)
            .filter(|relay| !excluded.iter().any(|excluded| excluded == *relay))
            .filter_map(|relay| {
                let candidate = self.service_relay_eviction_demand(relay)?;
                (relay_transport_value_cmp(candidate, incoming) == Ordering::Less).then_some(relay)
            })
            .min_by(|left, right| self.cmp_service_eviction_candidate(left, right))
            .cloned()
    }

    fn select_service_connecting_eviction_candidate(
        &self,
        incoming_relay: &NormRelayUrl,
        incoming: RelayTransportDemand,
        excluded: &[NormRelayUrl],
    ) -> Option<NormRelayUrl> {
        self.relay
            .transport
            .websockets
            .keys()
            .filter(|relay| *relay != incoming_relay)
            .filter(|relay| !excluded.iter().any(|excluded| excluded == *relay))
            .filter(|relay| self.relay.transport.websocket_is_connecting(relay))
            .filter_map(|relay| {
                let candidate = self.service_relay_eviction_demand(relay)?;
                (relay_transport_value_cmp(candidate, incoming) == Ordering::Less).then_some(relay)
            })
            .min_by(|left, right| self.cmp_service_eviction_candidate(left, right))
            .cloned()
    }

    fn select_lowest_service_websocket(&self) -> Option<NormRelayUrl> {
        self.relay
            .transport
            .websockets
            .keys()
            .filter_map(|relay| self.service_relay_eviction_demand(relay).map(|_| relay))
            .min_by(|left, right| self.cmp_service_eviction_candidate(left, right))
            .cloned()
    }

    fn service_relay_eviction_demand(&self, relay: &NormRelayUrl) -> Option<RelayTransportDemand> {
        self.relay.transport.demand_for(relay).or_else(|| {
            self.relay
                .transport
                .websockets
                .contains_key(relay)
                .then_some(RelayTransportDemand {
                    priority: RelayConnectionPriority {
                        strongest_demand: RelayDemandPriority::BestEffort,
                        request_count: 0,
                    },
                    source: RelayUrlSource::RemoteAdvertised,
                    connection_weight: 0,
                })
        })
    }

    fn cmp_service_eviction_candidate(
        &self,
        left_relay: &NormRelayUrl,
        right_relay: &NormRelayUrl,
    ) -> Ordering {
        let left = self
            .service_relay_eviction_demand(left_relay)
            .expect("left eviction candidate should have demand");
        let right = self
            .service_relay_eviction_demand(right_relay)
            .expect("right eviction candidate should have demand");

        relay_transport_value_cmp(left, right)
            .then_with(|| {
                self.service_relay_eviction_health_rank(right_relay, right)
                    .cmp(&self.service_relay_eviction_health_rank(left_relay, left))
            })
            .then_with(|| left_relay.cmp(right_relay))
    }

    fn service_relay_eviction_health_rank(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
    ) -> u32 {
        self.relay.admission.low_value_retry_attempts(relay, demand)
    }

    fn evict_idle_websockets_after_unsubscribes(
        &mut self,
        candidates: HashSet<NormRelayUrl>,
    ) -> OutboxServiceOutput {
        let mut output = OutboxServiceOutput::NoEvents;
        for relay in candidates {
            if self.relay.transport.demand_for(&relay).is_some() {
                continue;
            }
            output = merge_service_outputs(
                output,
                self.apply_relay_connection_eviction(
                    &relay,
                    RelayConnectionDropReason::IdleAfterUnsubscribe,
                ),
            );
        }
        output
    }

    fn evict_service_websocket_for_admission(
        &mut self,
        relay: &NormRelayUrl,
        now: Instant,
    ) -> OutboxServiceOutput {
        let Some(leg) = self.relay.transport.websockets.remove(relay) else {
            return OutboxServiceOutput::NoEvents;
        };
        self.relay.transport.reconnects.remove(relay);
        let status = self
            .relay
            .transport
            .demand_for(relay)
            .map(|_| RelayStatus::Disconnected);
        let status_output = self.relay.transport.set_status(relay.clone(), status);
        let service_output = self.apply_relay_transport_closed(relay, leg.generation, now);
        merge_service_outputs(status_output, service_output)
    }

    /// Create or replace live demand for one retained subscription id.
    pub fn set_live(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    ) -> OutboxServiceOutput {
        let output = self.pool.set_live(id, filters, relay_pkgs);
        self.handle_pool_output(output)
    }

    /// Remove live demand for one retained subscription id.
    pub fn clear_live(&mut self, id: OutboxSubId) -> OutboxServiceOutput {
        let output = self.pool.clear_live(id);
        self.handle_pool_output(output)
    }

    /// Replace full-history targets for one retained full-history id.
    pub fn set_full_history_targets(
        &mut self,
        id: FullHistorySubId,
        targets: Vec<FullHistoryTarget>,
    ) -> OutboxServiceOutput {
        let targets = normalize_full_history_targets(targets);
        let task = if targets.is_empty() {
            FullHistoryTask::Remove
        } else {
            FullHistoryTask::Upsert(FullHistoryUpsertTask { targets })
        };
        let (idle_websocket_eviction_candidates, full_history_output) =
            self.apply_full_history_tasks(HashMap::from([(id, task)]));
        let mut output = self.handle_full_history_output(full_history_output);
        let eviction_output =
            self.evict_idle_websockets_after_unsubscribes(idle_websocket_eviction_candidates);
        output = merge_service_outputs(output, eviction_output);
        output
    }

    /// Remove full-history demand for one retained full-history id.
    pub fn clear_full_history(&mut self, id: FullHistorySubId) -> OutboxServiceOutput {
        let (idle_websocket_eviction_candidates, full_history_output) =
            self.apply_full_history_tasks(HashMap::from([(id, FullHistoryTask::Remove)]));
        let mut output = self.handle_full_history_output(full_history_output);
        let eviction_output =
            self.evict_idle_websockets_after_unsubscribes(idle_websocket_eviction_candidates);
        output = merge_service_outputs(output, eviction_output);
        output
    }

    /// Start one concrete transient fetch.
    pub fn start_fetch(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    ) -> OutboxServiceOutput {
        let output = self.pool.start_fetch(id, filters, relay_pkgs);
        self.handle_pool_output(output)
    }

    /// Remove one retained transient fetch.
    pub fn clear_fetch(&mut self, id: OutboxSubId) -> OutboxServiceOutput {
        let output = self.pool.clear_fetch(id);
        self.handle_pool_output(output)
    }

    /// Publish one event message to the requested relays.
    pub fn publish(
        &mut self,
        msg: EventClientMessage,
        relays: Vec<RelayId>,
    ) -> OutboxServiceOutput {
        let mut output = OutboxServiceOutput::NoEvents;
        let mut relays = relays.into_iter().peekable();
        let mut msg = Some(msg);

        while let Some(relay) = relays.next() {
            let msg_for_relay = if relays.peek().is_some() {
                let Some(msg) = msg.as_ref() else {
                    break;
                };
                msg.clone()
            } else {
                let Some(msg) = msg.take() else {
                    break;
                };
                msg
            };

            match relay {
                RelayId::Websocket(relay) => {
                    output =
                        merge_service_outputs(output, self.publish_websocket(relay, msg_for_relay));
                }
                RelayId::Multicast => self.send_multicast_frame(msg_for_relay),
            }
        }

        output
    }

    fn publish_websocket(
        &mut self,
        relay: NormRelayUrl,
        msg: EventClientMessage,
    ) -> OutboxServiceOutput {
        if let Some(leg) = self.relay.transport.websockets.get_mut(&relay) {
            if leg.is_connected() {
                leg.send_event(msg);
                return OutboxServiceOutput::NoEvents;
            }
        }

        self.relay.transport.queue_publish(relay.clone(), msg);
        OutboxServiceOutput::NoEvents
    }

    /// Service relay connection work from service-owned timers and wakeups.
    fn apply_relay_connection_readiness(&mut self) -> OutboxServiceOutput {
        let now = Instant::now();
        self.apply_relay_transport_connections(now)
    }

    fn apply_relay_transport_connections(&mut self, now: Instant) -> OutboxServiceOutput {
        let demanded = self
            .relay
            .transport
            .demanded_relays()
            .into_iter()
            .collect::<HashSet<_>>();
        let mut output = OutboxServiceOutput::NoEvents;

        let mut open_relays = self
            .relay
            .transport
            .websockets
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        open_relays.sort_unstable();
        for relay in open_relays {
            if demanded.contains(&relay) {
                continue;
            }
            let Some(leg) = self.relay.transport.websockets.remove(&relay) else {
                continue;
            };
            self.relay.transport.reconnects.remove(&relay);
            let service_output = self.apply_relay_transport_closed(&relay, leg.generation, now);
            output = merge_service_outputs(
                output,
                merge_service_outputs(self.relay.transport.set_status(relay, None), service_output),
            );
        }

        output =
            merge_service_outputs(output, self.enforce_service_websocket_connection_limit(now));

        let mut admission = self.start_service_open_admission_at(now);
        for relay in self.service_relay_admission_work(now) {
            if self.relay.transport.websockets.contains_key(&relay) {
                continue;
            }
            if self
                .relay
                .transport
                .reconnects
                .get(&relay)
                .is_some_and(|state| state.retry_at > now)
            {
                continue;
            }
            let Some(demand) = self.relay.transport.demand_for(&relay) else {
                continue;
            };
            let Some(admission_output) =
                self.authorize_service_relay_open(&relay, demand, &mut admission, now)
            else {
                continue;
            };
            let generation = self.relay.transport.next_generation(&relay);
            output = merge_service_outputs(output, admission_output);
            output = merge_service_outputs(output, self.open_relay_transport(relay, generation));
        }

        output
    }

    fn service_relay_admission_work(&self, now: Instant) -> Vec<NormRelayUrl> {
        let mut work = self
            .relay
            .transport
            .demanded_relays()
            .into_iter()
            .filter_map(|relay| {
                let demand = self.relay.transport.demand_for(&relay)?;
                let rank = self.service_relay_admission_rank(&relay, demand, now);
                Some((relay, rank))
            })
            .collect::<Vec<_>>();
        work.sort_by(|(left_relay, left_rank), (right_relay, right_rank)| {
            Self::cmp_service_relay_admission_rank(*left_rank, *right_rank, left_relay, right_relay)
        });
        work.into_iter().map(|(relay, _)| relay).collect()
    }

    fn service_relay_admission_rank(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        now: Instant,
    ) -> RelayAdmissionRank {
        RelayAdmissionRank {
            priority: demand.priority,
            source_rank: nip11_source_rank(demand),
            connection_weight: demand.connection_weight,
            health_rank: self.nip11_health_rank(relay, Some(demand), now),
        }
    }

    fn cmp_service_relay_admission_rank(
        left: RelayAdmissionRank,
        right: RelayAdmissionRank,
        left_relay: &NormRelayUrl,
        right_relay: &NormRelayUrl,
    ) -> Ordering {
        right
            .priority
            .strongest_demand
            .cmp(&left.priority.strongest_demand)
            .then_with(|| right.source_rank.cmp(&left.source_rank))
            .then_with(|| right.connection_weight.cmp(&left.connection_weight))
            .then_with(|| {
                right
                    .priority
                    .request_count
                    .cmp(&left.priority.request_count)
            })
            .then_with(|| left.health_rank.cmp(&right.health_rank))
            .then_with(|| left_relay.cmp(right_relay))
    }

    fn handle_full_history_output(
        &mut self,
        output: FullHistoryRuntimeOutput,
    ) -> OutboxServiceOutput {
        let FullHistoryRuntimeOutput {
            full_history,
            negentropy_demand_changes,
            pool,
        } = output;
        let FullHistoryOutput {
            local_set_requests,
            local_presence_requests,
            pending_ingestion_presence_requests,
            fetch_requests,
            relay_demand_changes,
        } = full_history;
        self.apply_full_history_demand_changes(relay_demand_changes);
        self.apply_negentropy_demand_changes(negentropy_demand_changes);
        let mut service_output = self.handle_pool_output(pool);
        for request in local_set_requests {
            self.start_full_history_local_set(request);
        }
        for request in local_presence_requests {
            self.start_full_history_local_presence(request);
        }
        for request in pending_ingestion_presence_requests {
            self.start_full_history_pending_ingestion_presence(request);
        }

        let fetch_requests = fetch_requests
            .into_iter()
            .map(|request| (self.pool.next_sub_id(), request))
            .collect::<Vec<_>>();
        let transition = self.pool.start_full_history_fetches(fetch_requests);
        service_output = merge_service_outputs(service_output, self.handle_pool_output(transition));
        service_output
    }

    fn apply_full_history_demand_changes(
        &mut self,
        changes: HashMap<NormRelayUrl, Option<RelayTransportDemand>>,
    ) -> bool {
        let mut changed = false;
        for (relay, demand) in changes {
            changed |= self
                .relay
                .transport
                .apply_full_history_pending_demand(relay, demand);
        }
        changed
    }

    fn apply_negentropy_demand_changes(
        &mut self,
        changes: HashMap<NormRelayUrl, Option<RelayTransportDemand>>,
    ) -> bool {
        let mut changed = false;
        for (relay, demand) in changes {
            changed |= self.relay.transport.apply_negentropy_demand(relay, demand);
        }
        changed
    }

    /// Apply currently ready service-owned multicast events.
    fn apply_multicast_ready(&mut self) -> OutboxServiceOutput {
        let mut requests = Vec::new();
        self.relay.transport.multicast.try_recv(|event| {
            requests.push(EventIngestRequest {
                relay_url: event.url.to_owned(),
                relay_type: event.relay_type,
                ingest_json: event.event_json.to_owned(),
            });
        });
        for request in requests {
            self.start_event_ingest(request);
        }
        OutboxServiceOutput::NoEvents
    }

    /// Drive one ready service-owned async consequence.
    pub async fn next(&mut self) -> OutboxServiceOutput {
        loop {
            if let Some(output) = self.outputs.pop_ready() {
                return output;
            }
            let now = Instant::now();
            let relay_deadline = self.next_relay_connection_work_deadline(now);
            let liveness_deadline = self.next_websocket_liveness_deadline();
            let multicast_deadline = self.next_multicast_deadline();
            let full_history_deadline = self.next_full_history_runtime_deadline();
            let full_history_deadline_instant = full_history_deadline
                .as_ref()
                .map(|deadline| deadline.deadline);
            let nip11_deadline = self
                .next_nip11_deadline(now, SystemTime::now())
                .map(|deadline| system_time_deadline_to_instant(deadline, now));
            tokio::select! {
                result = self.nip11.recv() => {
                    if let Some(result) = result {
                        let output = self.apply_nip11_service_output(result);
                        if output.has_events() {
                            return output;
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                }
                result = self.capabilities.recv() => {
                    if let Some(result) = result {
                        let output = self.apply_capability_result(result);
                        if output.has_events() {
                            return output;
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                }
                ready = self.relay.transport_ready_rx.recv() => {
                    if let Some(ready) = ready {
                        let output = self.apply_relay_transport_ready(ready);
                        if output.has_events() {
                            return output;
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                }
                ready = self.relay.multicast_ready_rx.recv() => {
                    if ready.is_some() {
                        let output = self.apply_multicast_ready();
                        if output.has_events() {
                            return output;
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                }
                _ = sleep_until_deadline(relay_deadline), if relay_deadline.is_some() => {
                    let output = self.apply_relay_connection_readiness();
                    if output.has_events() {
                        return output;
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                _ = sleep_until_deadline(liveness_deadline), if liveness_deadline.is_some() => {
                    let output = self.apply_websocket_liveness_timer();
                    if output.has_events() {
                        return output;
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                _ = sleep_until_deadline(multicast_deadline), if multicast_deadline.is_some() => {
                    let output = self.apply_multicast_ready();
                    if output.has_events() {
                        return output;
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                _ = sleep_until_deadline(full_history_deadline_instant), if full_history_deadline.is_some() => {
                    let output = self.apply_full_history_deadline_due(
                        full_history_deadline.expect("full-history deadline branch is guarded"),
                    );
                    if output.has_events() {
                        return output;
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                _ = sleep_until_deadline(nip11_deadline), if nip11_deadline.is_some() => {
                    let output = self.apply_nip11_readiness();
                    if output.has_events() {
                        return output;
                    }
                    continue;
                }
            }
        }
    }

    fn handle_pool_output(&mut self, output: OutboxPoolOutput) -> OutboxServiceOutput {
        let Some(output) = self.outputs.handle_pool_output(output) else {
            return OutboxServiceOutput::NoEvents;
        };
        self.activate_pool_output(output)
    }

    fn next_relay_connection_work_deadline(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let demanded = self
            .relay
            .transport
            .demanded_relays()
            .into_iter()
            .collect::<HashSet<_>>();
        if self
            .relay
            .transport
            .websockets
            .keys()
            .any(|relay| !demanded.contains(relay))
        {
            next = Some(now);
        }
        let transport_deadlines = self.relay_transport_deadlines(now);
        for (relay_id, transport_deadline) in &transport_deadlines {
            let Some(demand) = self.relay.transport.demand_for(relay_id) else {
                continue;
            };
            let admission = self.start_service_open_admission_at(now);
            let deadline = self.apply_service_admission_deadlines(
                relay_id,
                demand,
                *transport_deadline,
                admission.policy(),
                now,
            );
            next = Some(
                next.map(|current| current.min(deadline))
                    .unwrap_or(deadline),
            );
        }
        next
    }

    fn apply_service_admission_deadlines(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        transport_deadline: Instant,
        admission_policy: OutboxAdmissionPolicy,
        now: Instant,
    ) -> Instant {
        let transport_deadline =
            self.apply_service_transport_health_deadline(relay, demand, transport_deadline, now);
        self.apply_service_admission_deferral_deadline(
            relay,
            demand,
            transport_deadline,
            admission_policy,
            now,
        )
    }

    fn apply_service_transport_health_deadline(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        transport_deadline: Instant,
        now: Instant,
    ) -> Instant {
        self.relay
            .admission
            .apply_transport_health_deadline(relay, demand, transport_deadline, now)
    }

    fn apply_service_admission_deferral_deadline(
        &self,
        relay: &NormRelayUrl,
        demand: RelayTransportDemand,
        transport_deadline: Instant,
        admission_policy: OutboxAdmissionPolicy,
        now: Instant,
    ) -> Instant {
        self.relay.admission.apply_deferral_deadline(
            relay,
            demand,
            transport_deadline,
            admission_policy,
            now,
        )
    }

    fn relay_transport_deadlines(&self, now: Instant) -> HashMap<NormRelayUrl, Instant> {
        self.relay
            .transport
            .demanded_relays()
            .into_iter()
            .filter_map(|relay| {
                if self.relay.transport.websockets.contains_key(&relay) {
                    return None;
                }
                Some((
                    relay.clone(),
                    self.relay
                        .transport
                        .reconnects
                        .get(&relay)
                        .map(|state| state.retry_at)
                        .unwrap_or(now),
                ))
            })
            .collect()
    }

    fn next_websocket_liveness_deadline(&self) -> Option<Instant> {
        self.relay
            .transport
            .websockets
            .values()
            .map(|leg| {
                (leg.last_ping + self.relay.config.keepalive_ping_rate)
                    .min(leg.last_pong + self.relay.config.pong_timeout)
            })
            .min()
    }

    fn activate_pool_output(&mut self, output: OutboxPoolOutput) -> OutboxServiceOutput {
        let OutboxPoolOutput {
            facts,
            relay_demand_changes,
            transport_effects,
            full_history_effects,
        } = output;
        for change in relay_demand_changes {
            self.relay.transport.apply_pool_demand(change);
        }
        let mut service_output = OutboxServiceOutput::NoEvents;
        for effect in transport_effects {
            service_output =
                merge_service_outputs(service_output, self.activate_transport_effect(effect));
        }
        let events = facts.into_iter().map(OutboxEvent::from).collect::<Vec<_>>();
        service_output = merge_service_outputs(
            service_output,
            self.activate_full_history_effects(full_history_effects),
        );
        merge_service_outputs(service_output, OutboxServiceOutput::from_events(events))
    }

    fn activate_full_history_effects(
        &mut self,
        effects: Vec<OutboxFullHistoryEffect>,
    ) -> OutboxServiceOutput {
        let mut service_output = OutboxServiceOutput::NoEvents;
        for effect in effects {
            let output = match effect {
                OutboxFullHistoryEffect::NegentropyCapacityGranted { relay, grant } => {
                    self.apply_full_history_negentropy_capacity_grant(relay, grant)
                }
                OutboxFullHistoryEffect::NegentropyEffect { relay, effect } => {
                    self.apply_negentropy_effect(&relay, effect)
                }
            };
            service_output =
                merge_service_outputs(service_output, self.handle_full_history_output(output));
        }
        service_output
    }

    fn apply_websocket_liveness_timer(&mut self) -> OutboxServiceOutput {
        let now = Instant::now();
        let mut relays = self
            .relay
            .transport
            .websockets
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        relays.sort_unstable();

        for relay in relays {
            let Some(leg) = self.relay.transport.websockets.get_mut(&relay) else {
                continue;
            };

            if now.saturating_duration_since(leg.last_pong) >= self.relay.config.pong_timeout {
                let generation = leg.generation;
                let was_connecting = leg.conn.status == RelayStatus::Connecting;
                tracing::warn!(
                    relay = %relay,
                    generation,
                    was_connecting,
                    last_pong_elapsed_ms = now.saturating_duration_since(leg.last_pong).as_millis(),
                    pong_timeout_ms = self.relay.config.pong_timeout.as_millis(),
                    last_ping_elapsed_ms = now.saturating_duration_since(leg.last_ping).as_millis(),
                    "pong timeout, marking disconnected"
                );
                self.relay.transport.websockets.remove(&relay);
                self.note_relay_transport_disconnected(&relay, was_connecting, now);
                let status_output = self
                    .relay
                    .transport
                    .set_status(relay.clone(), Some(RelayStatus::Disconnected));
                let output = self.apply_relay_transport_closed(&relay, generation, now);
                return merge_service_outputs(status_output, output);
            }

            if now.saturating_duration_since(leg.last_ping) >= self.relay.config.keepalive_ping_rate
            {
                leg.conn.ping();
                leg.last_ping = now;
                return OutboxServiceOutput::NoEvents;
            }
        }

        OutboxServiceOutput::NoEvents
    }

    fn note_relay_transport_open_attempt(&mut self, relay: &NormRelayUrl, now: Instant) {
        let Some(state) = self.relay.transport.reconnects.get_mut(relay) else {
            return;
        };
        state.attempt = state.attempt.saturating_add(1);
        state.retry_at = now;
    }

    fn note_relay_transport_connected(&mut self, relay: &NormRelayUrl) {
        self.relay.transport.reconnects.remove(relay);
        self.relay.admission.note_transport_connected(relay);
    }

    fn note_relay_transport_disconnected(
        &mut self,
        relay: &NormRelayUrl,
        was_connecting: bool,
        now: Instant,
    ) {
        let state = self
            .relay
            .transport
            .reconnects
            .entry(relay.clone())
            .or_insert(RelayReconnectState {
                attempt: 0,
                retry_at: now + self.relay.config.keepalive_reconnect_delay,
            });
        if was_connecting && state.attempt > 0 {
            let retry_after = backoff::next_duration_from_base(
                state.attempt,
                self.relay.config.keepalive_reconnect_backoff_base,
                backoff::jitter_seed(relay, state.attempt),
                MAX_RECONNECT_DELAY,
            );
            state.retry_at = now + retry_after;
            return;
        }

        state.attempt = 0;
        state.retry_at = now + self.relay.config.keepalive_reconnect_delay;
        self.note_low_value_transport_retry(
            relay,
            now,
            LowValueOpenBackoffReason::TransportFailure,
        );
    }

    fn note_relay_transport_auth_required(&mut self, relay: &NormRelayUrl, now: Instant) {
        self.note_low_value_transport_retry(relay, now, LowValueOpenBackoffReason::AuthRequired);
    }

    fn note_low_value_transport_retry(
        &mut self,
        relay: &NormRelayUrl,
        now: Instant,
        reason: LowValueOpenBackoffReason,
    ) {
        let Some(demand) = self.relay.transport.demand_for(relay) else {
            return;
        };
        self.relay
            .admission
            .note_low_value_transport_retry(relay, demand, now, reason);
    }

    fn apply_full_history_deadline_due(
        &mut self,
        deadline: FullHistoryRuntimeDeadline,
    ) -> OutboxServiceOutput {
        let now = Instant::now();
        let output = match deadline.input {
            FullHistoryRuntimeDeadlineInput::Workflow => {
                self.apply_full_history_workflow_deadline_due(now)
            }
            FullHistoryRuntimeDeadlineInput::NegentropyTimeout { relay } => {
                self.apply_negentropy_timeout_due(relay, now)
            }
        };
        self.handle_full_history_output(output)
    }

    fn start_full_history_local_set(&mut self, request: FullHistoryLocalSetRequest) {
        self.capabilities.start_full_history_local_set(request);
    }

    fn start_full_history_local_presence(&mut self, request: FullHistoryLocalPresenceRequest) {
        self.capabilities.start_full_history_local_presence(request);
    }

    fn start_full_history_pending_ingestion_presence(
        &mut self,
        request: FullHistoryPendingIngestionPresenceRequest,
    ) {
        self.capabilities
            .start_full_history_pending_ingestion_presence(request);
    }

    fn start_event_ingest(&mut self, request: EventIngestRequest) {
        self.capabilities.start_event_ingest(request);
    }

    fn activate_transport_effect(&mut self, effect: OutboxTransportEffect) -> OutboxServiceOutput {
        match effect {
            OutboxTransportEffect::SendRelayFrame {
                relay,
                generation,
                message,
            } => {
                if let Some(leg) = self.current_websocket_leg_mut(&relay, generation) {
                    leg.conn.send(&message);
                    return OutboxServiceOutput::NoEvents;
                }

                let now = Instant::now();
                self.note_relay_transport_disconnected(&relay, false, now);
                self.relay
                    .transport
                    .set_status(relay, Some(RelayStatus::Disconnected))
            }
        }
    }

    fn send_multicast_frame(&mut self, message: EventClientMessage) {
        if !self.relay.transport.multicast.is_setup() {
            let tx = self.relay.multicast_ready_tx.clone();
            self.relay.transport.multicast.try_setup_fn(move || {
                let _ = tx.send(());
            });
        }
        self.relay.transport.multicast.broadcast(message);
    }

    fn open_relay_transport(
        &mut self,
        relay: NormRelayUrl,
        generation: u64,
    ) -> OutboxServiceOutput {
        let now = Instant::now();
        self.note_relay_transport_open_attempt(&relay, now);
        let tx = self.relay.transport_ready_tx.clone();
        let relay_for_wakeup = relay.clone();
        let conn = WebsocketConn::new(relay.clone().into(), move || {
            let _ = tx.send(RelayTransportReady {
                relay: relay_for_wakeup.clone(),
                generation,
            });
        });
        match conn {
            Ok(mut conn) => {
                conn.set_send_generation(generation);
                self.relay
                    .transport
                    .websockets
                    .insert(relay.clone(), ServiceWebsocketLeg::new(conn, generation));
                self.relay
                    .transport
                    .set_status(relay, Some(RelayStatus::Connecting))
            }
            Err(error) => {
                self.note_relay_transport_disconnected(&relay, true, now);
                let status_output = self
                    .relay
                    .transport
                    .set_status(relay.clone(), Some(RelayStatus::Disconnected));
                if let crate::Error::WebSocket(websocket_error) = &error {
                    self.relay
                        .admission
                        .enter_hard_failure_from_websocket_error(websocket_error);
                }
                let output =
                    self.apply_relay_transport_error(&relay, generation, error.to_string(), now);
                merge_service_outputs(status_output, output)
            }
        }
    }

    fn current_websocket_leg_mut(
        &mut self,
        relay: &NormRelayUrl,
        generation: u64,
    ) -> Option<&mut ServiceWebsocketLeg> {
        self.relay
            .transport
            .websockets
            .get_mut(relay)
            .filter(|leg| leg.generation == generation)
    }

    fn drain_publish_queue(&mut self, relay: &NormRelayUrl, generation: u64) {
        self.relay
            .transport
            .drain_publish_queue_for_generation(relay, generation);
    }

    fn apply_relay_transport_ready(&mut self, ready: RelayTransportReady) -> OutboxServiceOutput {
        let Some(event) = self
            .current_websocket_leg_mut(&ready.relay, ready.generation)
            .and_then(|leg| leg.conn.receiver.try_recv())
        else {
            return OutboxServiceOutput::NoEvents;
        };

        match event {
            WsEvent::Opened => {
                if let Some(leg) = self.current_websocket_leg_mut(&ready.relay, ready.generation) {
                    leg.conn.set_status(RelayStatus::Connected);
                    let now = Instant::now();
                    leg.last_ping = now;
                    leg.last_pong = now;
                }
                self.note_relay_transport_connected(&ready.relay);
                let status_output = self
                    .relay
                    .transport
                    .set_status(ready.relay.clone(), Some(RelayStatus::Connected));
                let output =
                    self.apply_relay_transport_opened(ready.relay.clone(), ready.generation);
                self.drain_publish_queue(&ready.relay, ready.generation);
                merge_service_outputs(status_output, output)
            }
            WsEvent::Closed => {
                let was_connecting = self
                    .relay
                    .transport
                    .websockets
                    .get(&ready.relay)
                    .is_some_and(|leg| leg.conn.status == RelayStatus::Connecting);
                self.relay.transport.websockets.remove(&ready.relay);
                let now = Instant::now();
                self.note_relay_transport_disconnected(&ready.relay, was_connecting, now);
                let status_output = self
                    .relay
                    .transport
                    .set_status(ready.relay.clone(), Some(RelayStatus::Disconnected));
                let output = self.apply_relay_transport_closed(&ready.relay, ready.generation, now);
                merge_service_outputs(status_output, output)
            }
            WsEvent::Error(error) => {
                let was_connecting = self
                    .relay
                    .transport
                    .websockets
                    .get(&ready.relay)
                    .is_some_and(|leg| leg.conn.status == RelayStatus::Connecting);
                self.relay.transport.websockets.remove(&ready.relay);
                let now = Instant::now();
                self.note_relay_transport_disconnected(&ready.relay, was_connecting, now);
                let status_output = self
                    .relay
                    .transport
                    .set_status(ready.relay.clone(), Some(RelayStatus::Disconnected));
                let output = self.apply_relay_transport_error(
                    &ready.relay,
                    ready.generation,
                    error.to_string(),
                    now,
                );
                merge_service_outputs(status_output, output)
            }
            WsEvent::Message(message) => self.apply_relay_ws_message(ready, message),
        }
    }

    fn apply_relay_ws_message(
        &mut self,
        ready: RelayTransportReady,
        message: WsMessage,
    ) -> OutboxServiceOutput {
        match message {
            #[cfg(not(target_arch = "wasm32"))]
            WsMessage::Ping(bytes) => {
                if let Some(leg) = self.current_websocket_leg_mut(&ready.relay, ready.generation) {
                    leg.conn.sender.send(WsMessage::Pong(bytes));
                }
                OutboxServiceOutput::NoEvents
            }
            WsMessage::Pong(_) => {
                if let Some(leg) = self.current_websocket_leg_mut(&ready.relay, ready.generation) {
                    leg.last_pong = Instant::now();
                }
                let output = self
                    .pool
                    .apply_relay_transport_pong(&ready.relay, ready.generation);
                self.activate_pool_output(output)
            }
            WsMessage::Text(text) => self.apply_relay_ws_text(ready, text),
            _ => OutboxServiceOutput::NoEvents,
        }
    }

    fn apply_relay_ws_text(
        &mut self,
        ready: RelayTransportReady,
        text: String,
    ) -> OutboxServiceOutput {
        match RelayMessage::from_json(&text) {
            Ok(RelayMessage::Event(_, _)) => {
                self.start_event_ingest(EventIngestRequest {
                    relay_url: ready.relay.to_string(),
                    relay_type: RelayImplType::Websocket,
                    ingest_json: text,
                });
                OutboxServiceOutput::NoEvents
            }
            Ok(RelayMessage::Eose(sid)) => {
                let output = self
                    .pool
                    .apply_relay_eose(&ready.relay, ready.generation, sid);
                self.activate_pool_output(output)
            }
            Ok(RelayMessage::Closed(sid, message)) => {
                if relay_notice_requires_auth(message) {
                    self.note_relay_transport_auth_required(&ready.relay, Instant::now());
                }
                let output = self
                    .pool
                    .apply_relay_closed(&ready.relay, ready.generation, sid);
                self.activate_pool_output(output)
            }
            Ok(RelayMessage::Auth(_)) => {
                self.note_relay_transport_auth_required(&ready.relay, Instant::now());
                OutboxServiceOutput::NoEvents
            }
            Ok(RelayMessage::Notice(message)) => {
                if relay_notice_requires_auth(message) {
                    self.note_relay_transport_auth_required(&ready.relay, Instant::now());
                }
                self.apply_relay_notice(&ready.relay, ready.generation, message)
            }
            Ok(RelayMessage::NegMsg(sub_id, payload)) => {
                let output = self.apply_relay_neg_msg(
                    &ready.relay,
                    ready.generation,
                    sub_id.as_ref(),
                    payload.as_ref(),
                );
                output
            }
            Ok(RelayMessage::NegErr(sub_id, reason)) => {
                let output = self.apply_relay_neg_err(
                    &ready.relay,
                    ready.generation,
                    sub_id.as_ref(),
                    reason.as_ref(),
                );
                output
            }
            Ok(RelayMessage::OK(command_result)) => {
                tracing::info!("OK {:?}", command_result);
                OutboxServiceOutput::NoEvents
            }
            Err(err) => {
                tracing::error!("relay {} message decode error: {:?}", ready.relay, err);
                OutboxServiceOutput::NoEvents
            }
        }
    }

    fn apply_capability_result(&mut self, result: CapabilityResult) -> OutboxServiceOutput {
        match result {
            CapabilityResult::FullHistoryLocalSet(result) => {
                self.apply_full_history_local_set_result(result)
            }
            CapabilityResult::FullHistoryLocalPresence(result) => {
                self.apply_full_history_local_presence_result(result)
            }
            CapabilityResult::FullHistoryPendingIngestionPresence(result) => {
                self.apply_full_history_pending_ingestion_presence_result(result)
            }
            CapabilityResult::EventIngest => OutboxServiceOutput::NoEvents,
        }
    }

    fn apply_nip11_service_output(&mut self, result: Nip11ServiceOutput) -> OutboxServiceOutput {
        match result {
            Nip11ServiceOutput::Stale => OutboxServiceOutput::NoEvents,
            Nip11ServiceOutput::FetchFailed => OutboxServiceOutput::NoEvents,
            Nip11ServiceOutput::Limits { request, raw } => {
                let relay = request.relay.clone();
                let (outcome, output) = self.apply_nip11_raw_limits(&relay, *raw);
                let ack = match outcome {
                    Nip11ApplyOutcome::RelayUnknown => Nip11ApplyAck::Deferred,
                    Nip11ApplyOutcome::Applied
                    | Nip11ApplyOutcome::Unchanged
                    | Nip11ApplyOutcome::UnsupportedSubIdLength { .. } => Nip11ApplyAck::Consumed,
                };
                self.nip11.apply_limits_ack(request, ack, SystemTime::now());
                if matches!(
                    outcome,
                    Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::UnsupportedSubIdLength { .. }
                ) {
                    tracing::debug!("applied NIP-11 limits for {relay}");
                }

                output
            }
        }
    }

    fn apply_full_history_local_set_result(
        &mut self,
        result: FullHistoryLocalSetResult,
    ) -> OutboxServiceOutput {
        let (_, full_history_output) = if let Some(storage) = result.result {
            self.apply_full_history_local_set_ready(result.history_id, result.request_id, storage)
        } else {
            self.apply_full_history_local_set_failed(result.history_id, result.request_id)
        };
        self.handle_full_history_output(full_history_output)
    }

    fn apply_full_history_local_presence_result(
        &mut self,
        result: FullHistoryLocalPresenceResult,
    ) -> OutboxServiceOutput {
        let (_, full_history_output) = self.apply_full_history_local_presence_ready(result);
        self.handle_full_history_output(full_history_output)
    }

    fn apply_full_history_pending_ingestion_presence_result(
        &mut self,
        result: FullHistoryPendingIngestionPresenceResult,
    ) -> OutboxServiceOutput {
        let (_, full_history_output) = self.apply_pending_ingestion_presence_result(result);
        self.handle_full_history_output(full_history_output)
    }

    fn apply_nip11_raw_limits(
        &mut self,
        relay: &NormRelayUrl,
        raw: Nip11LimitationsRaw,
    ) -> (Nip11ApplyOutcome, OutboxServiceOutput) {
        if let Some(max_subid_length) = unsupported_max_subid_length(&raw) {
            return self.apply_unsupported_subid_length(relay, max_subid_length);
        }

        let Some(current) = self.pool.relay_limitations(relay) else {
            return (
                Nip11ApplyOutcome::RelayUnknown,
                OutboxServiceOutput::NoEvents,
            );
        };
        let limitations = derive_relay_limitations_from_raw(&raw, current);
        self.apply_relay_limit_update(relay, limitations)
    }
}

fn next_backoff_duration_from_base(
    attempt: u32,
    base: Duration,
    jitter_seed: u64,
    max: Duration,
) -> Duration {
    let base = base_delay_from(attempt, base, max);
    let jitter_ceiling = base / 4;
    let jitter = if jitter_ceiling.is_zero() {
        Duration::ZERO
    } else {
        Duration::from_nanos(jitter_seed % jitter_ceiling.as_nanos() as u64)
    };
    (base + jitter).min(max)
}

fn base_delay_from(attempt: u32, base: Duration, max: Duration) -> Duration {
    let base_nanos = base.as_nanos() as u64;
    let nanos = base_nanos.checked_shl(attempt).unwrap_or(u64::MAX);
    Duration::from_nanos(nanos).min(max)
}

impl From<OutboxPoolFact> for OutboxEvent {
    fn from(fact: OutboxPoolFact) -> Self {
        match fact {
            OutboxPoolFact::RelayReqStatus { id, relay, status } => {
                Self::RelayReqStatusChanged { id, relay, status }
            }
            OutboxPoolFact::OutboxSubRelayEose { id, relay_eose } => {
                Self::OutboxSubRelayEoseChanged { id, relay_eose }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::effect_turn::OutboxEffectAccumulator;
    use super::nip11::{
        nip11_failure_retry_after, Nip11ApplyAck, Nip11Driver, Nip11InterestRank,
        Nip11InterestRead, Nip11InterestResume, Nip11InterestState, Nip11ReadinessInput,
        Nip11RelayFetchState, MAX_NIP11_APPLY_DEFERRED_BACKOFF, MAX_NIP11_FAILURE_BACKOFF,
        NIP11_APPLY_DEFERRED_BACKOFF_BASE, NIP11_FAILURE_BACKOFF_BASE, NIP11_FETCH_CONCURRENCY,
        NIP11_REFRESH_AFTER_SUCCESS,
    };
    use super::*;
    use crate::test_support::outbox::{
        test_outbox_service, TestEventIngestCapability, TestFullHistoryCapability,
        TestNip11Capability, TestOutboxService,
    };
    use crate::{
        relay::test_utils::{
            create_filtered_capture_relay_with_handler, CaptureNotify, CaptureRelayResponse,
            CapturedTextFrames,
        },
        FullKeypair,
    };
    use futures_util::{SinkExt, StreamExt};
    use negentropy::{Negentropy, NegentropyStorageVector};
    use nostrdb::NoteBuilder;
    use std::{collections::HashMap as StdHashMap, future, sync::Arc, time::UNIX_EPOCH};
    use tokio::net::TcpListener;
    use tokio::sync::Notify;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    fn relay(name: &str) -> NormRelayUrl {
        NormRelayUrl::new(&format!("wss://relay-nip11-service-{name}.example.com"))
            .expect("valid relay url")
    }

    fn nip11_interest(relay: NormRelayUrl, state: Nip11InterestState) -> Nip11InterestRead {
        Nip11InterestRead {
            relay,
            rank: Nip11InterestRank {
                priority: None,
                source_rank: 0,
                connection_weight: 0,
                health_rank: (false, 0),
            },
            state,
        }
    }

    fn active_nip11_interest(relay: NormRelayUrl) -> Nip11InterestRead {
        nip11_interest(relay, Nip11InterestState::Active)
    }

    fn websocket_leg(relay: &NormRelayUrl, generation: u64) -> ServiceWebsocketLeg {
        let mut conn =
            WebsocketConn::new(relay.clone().into(), || {}).expect("test websocket conn");
        conn.set_send_generation(generation);
        conn.set_status(RelayStatus::Connected);
        ServiceWebsocketLeg::new(conn, generation)
    }

    fn close_message(name: &str) -> crate::ClientMessage {
        crate::ClientMessage::close(name.to_owned())
    }

    fn explicit_relay_pkgs(relay: NormRelayUrl) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            HashSet::from([relay]),
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::PreferDedicated,
            ),
        )
    }

    fn explicit_relay_pkgs_with_priority(
        relay: NormRelayUrl,
        demand_priority: crate::relay::RelayDemandPriority,
    ) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            HashSet::from([relay]),
            crate::relay::RelayUrlPolicy::explicit(
                demand_priority,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        )
    }

    fn remote_relay_pkgs(
        relay: NormRelayUrl,
        demand_priority: crate::relay::RelayDemandPriority,
        routing_preference: crate::relay::RelayRoutingPreference,
    ) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            HashSet::from([relay]),
            crate::relay::RelayUrlPolicy::remote_advertised(demand_priority, routing_preference),
        )
    }

    fn trivial_filter() -> Vec<Filter> {
        vec![Filter::new().kinds([1]).limit(1).build()]
    }

    fn install_connected_service_websocket<N, F, E>(
        service: &mut OutboxService<N, F, E>,
        relay: NormRelayUrl,
        generation: u64,
    ) {
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), websocket_leg(&relay, generation));
        let _ = service
            .relay
            .transport
            .set_status(relay.clone(), Some(RelayStatus::Connected));
        let _ = service.pool.apply_relay_transport_opened(relay, generation);
    }

    fn install_connecting_service_websocket<N, F, E>(
        service: &mut OutboxService<N, F, E>,
        relay: NormRelayUrl,
        generation: u64,
    ) {
        let mut leg = websocket_leg(&relay, generation);
        leg.conn.set_status(RelayStatus::Connecting);
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), leg);
        let _ = service
            .relay
            .transport
            .set_status(relay, Some(RelayStatus::Connecting));
    }

    async fn poll_service_progress<N, F, E>(service: &mut OutboxService<N, F, E>)
    where
        N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
        F: FullHistoryCapability<
            LocalSetOutput = FullHistoryLocalSetResult,
            LocalPresenceOutput = FullHistoryLocalPresenceResult,
            PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
        >,
        E: EventIngestCapability,
    {
        let _ = tokio::time::timeout(Duration::from_millis(20), service.next()).await;
    }

    async fn create_eose_relay() -> (tokio::task::JoinHandle<()>, NormRelayUrl, Arc<Notify>) {
        let sent = Arc::new(Notify::new());
        let sent_factory = Arc::clone(&sent);
        let (handle, url, _captured, _captured_notify) =
            create_filtered_capture_relay_with_handler(
                |_| false,
                move || {
                    let sent = Arc::clone(&sent_factory);
                    move |text: &str| {
                        let parts: serde_json::Value =
                            serde_json::from_str(text).expect("parse relay client frame");
                        if parts[0] != "REQ" {
                            return CaptureRelayResponse::none();
                        }

                        let sid = parts[1].as_str().expect("REQ sid");
                        sent.notify_one();
                        CaptureRelayResponse {
                            send_text: vec![serde_json::json!(["EOSE", sid]).to_string()],
                            close: false,
                        }
                    }
                },
            )
            .await;

        (handle, url, sent)
    }

    fn service() -> TestOutboxService {
        test_outbox_service()
    }

    #[derive(Clone, Copy)]
    struct EmptyFullHistoryCapability;

    impl FullHistoryCapability for EmptyFullHistoryCapability {
        type LocalSetOutput = FullHistoryLocalSetResult;
        type LocalSetFuture = future::Ready<Self::LocalSetOutput>;
        type LocalPresenceOutput = FullHistoryLocalPresenceResult;
        type LocalPresenceFuture = future::Ready<Self::LocalPresenceOutput>;
        type PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult;
        type PendingIngestionPresenceFuture = future::Ready<Self::PendingIngestionPresenceOutput>;

        fn build_local_set(&self, request: FullHistoryLocalSetRequest) -> Self::LocalSetFuture {
            let mut storage = NegentropyStorageVector::new();
            storage.seal().expect("seal empty storage");
            future::ready(FullHistoryLocalSetResult {
                history_id: request.history_id,
                request_id: request.request_id,
                result: Some(storage),
            })
        }

        fn check_local_presence(
            &self,
            request: FullHistoryLocalPresenceRequest,
        ) -> Self::LocalPresenceFuture {
            future::ready(FullHistoryLocalPresenceResult {
                request_id: request.request_id,
                missing_ids: request.candidate_ids,
                already_local_ids: HashSet::new(),
            })
        }

        fn check_pending_ingestion_presence(
            &self,
            _request: FullHistoryPendingIngestionPresenceRequest,
        ) -> Self::PendingIngestionPresenceFuture {
            future::ready(FullHistoryPendingIngestionPresenceResult {
                stored_ids: HashSet::new(),
            })
        }
    }

    #[derive(Clone)]
    struct CapturingNip11Capability {
        sender: std::sync::mpsc::Sender<Nip11FetchRequest>,
    }

    impl Nip11Capability for CapturingNip11Capability {
        type Output = Result<Nip11LimitationsRaw, String>;
        type Future = future::Ready<Self::Output>;

        fn fetch_nip11(&self, request: Nip11FetchRequest) -> Self::Future {
            self.sender.send(request).expect("send NIP-11 request");
            future::ready(Err("captured NIP-11 request".to_owned()))
        }
    }

    #[derive(Clone)]
    struct StaticNip11Capability {
        result: Result<Nip11LimitationsRaw, String>,
    }

    impl Nip11Capability for StaticNip11Capability {
        type Output = Result<Nip11LimitationsRaw, String>;
        type Future = future::Ready<Self::Output>;

        fn fetch_nip11(&self, _request: Nip11FetchRequest) -> Self::Future {
            future::ready(self.result.clone())
        }
    }

    #[derive(Clone)]
    struct CapturingEventIngestCapability {
        sender: std::sync::mpsc::Sender<EventIngestRequest>,
    }

    impl EventIngestCapability for CapturingEventIngestCapability {
        type Future = future::Ready<()>;

        fn ingest_event(&self, request: EventIngestRequest) -> Self::Future {
            self.sender
                .send(request)
                .expect("send captured event ingest request");
            future::ready(())
        }
    }

    async fn create_negentropy_eose_relay() -> (tokio::task::JoinHandle<()>, NormRelayUrl) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind negentropy eose relay");
        let addr = listener.local_addr().expect("negentropy eose relay addr");
        let url = NormRelayUrl::new(&format!("ws://{addr}")).expect("valid relay url");

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept eose relay");
            let mut websocket = accept_async(stream).await.expect("upgrade eose relay");
            let mut sessions =
                StdHashMap::<String, Negentropy<'static, NegentropyStorageVector>>::new();
            while let Some(msg) = websocket.next().await {
                let Message::Text(text) = msg.expect("read eose relay message") else {
                    continue;
                };
                let parts: serde_json::Value =
                    serde_json::from_str(&text).expect("parse relay client frame");
                let Some(command) = parts[0].as_str() else {
                    continue;
                };

                match command {
                    "REQ" => {
                        let sid = parts[1].as_str().expect("REQ sid");
                        websocket
                            .send(Message::Text(serde_json::json!(["EOSE", sid]).to_string()))
                            .await
                            .expect("send eose");
                    }
                    "NEG-OPEN" => {
                        let sid = parts[1].as_str().expect("NEG-OPEN sid");
                        let initial_message = parts[3].as_str().expect("NEG-OPEN initial message");
                        let mut storage = NegentropyStorageVector::new();
                        storage.seal().expect("seal empty relay storage");
                        let mut session = Negentropy::owned(storage, 0).expect("empty negentropy");
                        let query = hex::decode(initial_message).expect("decode initial message");
                        let reply = session
                            .reconcile(&query)
                            .expect("reconcile initial message");
                        sessions.insert(sid.to_owned(), session);
                        websocket
                            .send(Message::Text(
                                serde_json::json!(["NEG-MSG", sid, hex::encode(reply)]).to_string(),
                            ))
                            .await
                            .expect("send negentropy reply");
                    }
                    "NEG-MSG" => {
                        let sid = parts[1].as_str().expect("NEG-MSG sid");
                        let message = parts[2].as_str().expect("NEG-MSG payload");
                        let Some(session) = sessions.get_mut(sid) else {
                            continue;
                        };
                        let query = hex::decode(message).expect("decode negentropy message");
                        let reply = session.reconcile(&query).expect("reconcile message");
                        websocket
                            .send(Message::Text(
                                serde_json::json!(["NEG-MSG", sid, hex::encode(reply)]).to_string(),
                            ))
                            .await
                            .expect("send negentropy reply");
                    }
                    "NEG-CLOSE" => {
                        let sid = parts[1].as_str().expect("NEG-CLOSE sid");
                        sessions.remove(sid);
                    }
                    _ => {}
                }
            }
        });

        (handle, url)
    }

    async fn wait_for_service_event<N, F, E>(
        service: &mut OutboxService<N, F, E>,
        timeout: Duration,
        mut predicate: impl FnMut(&OutboxEvent) -> bool,
    ) -> OutboxEvent
    where
        N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
        F: FullHistoryCapability<
            LocalSetOutput = FullHistoryLocalSetResult,
            LocalPresenceOutput = FullHistoryLocalPresenceResult,
            PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
        >,
        E: EventIngestCapability,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for outbox service event");
            let remaining = deadline
                .checked_duration_since(now)
                .expect("remaining service wait");
            let output = tokio::time::timeout(remaining, service.next())
                .await
                .expect("outbox service should make progress before timeout");
            let OutboxServiceOutput::Events(events) = output else {
                continue;
            };
            for event in events {
                if predicate(&event) {
                    return event;
                }
            }
        }
    }

    async fn wait_for_nip11_request_relays<N, F, E>(
        service: &mut OutboxService<N, F, E>,
        receiver: &std::sync::mpsc::Receiver<Nip11FetchRequest>,
        expected: usize,
        timeout: Duration,
    ) -> HashSet<NormRelayUrl>
    where
        N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
        F: FullHistoryCapability<
            LocalSetOutput = FullHistoryLocalSetResult,
            LocalPresenceOutput = FullHistoryLocalPresenceResult,
            PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
        >,
        E: EventIngestCapability,
    {
        let deadline = Instant::now() + timeout;
        let mut relays = HashSet::new();
        loop {
            while let Ok(request) = receiver.try_recv() {
                relays.insert(request.relay);
            }
            if relays.len() == expected {
                return relays;
            }

            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {expected} NIP-11 requests; got {relays:?}"
            );
            let remaining = deadline
                .checked_duration_since(now)
                .expect("remaining NIP-11 request wait");
            let _ = tokio::time::timeout(remaining, service.next()).await;
        }
    }

    async fn wait_for_service_capture<N, F, E>(
        service: &mut OutboxService<N, F, E>,
        captured: &CapturedTextFrames,
        notify: &CaptureNotify,
        timeout: Duration,
        context: &str,
        predicate: impl Fn(&str) -> bool,
    ) -> String
    where
        N: Nip11Capability<Output = Result<Nip11LimitationsRaw, String>>,
        F: FullHistoryCapability<
            LocalSetOutput = FullHistoryLocalSetResult,
            LocalPresenceOutput = FullHistoryLocalPresenceResult,
            PendingIngestionPresenceOutput = FullHistoryPendingIngestionPresenceResult,
        >,
        E: EventIngestCapability,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = captured
                .lock()
                .expect("lock captured text frames")
                .iter()
                .find(|text| predicate(text))
                .cloned()
            {
                return frame;
            }

            let now = Instant::now();
            let snapshot = captured.lock().expect("lock captured text frames").clone();
            assert!(
                now < deadline,
                "timed out waiting for {context}; captured {snapshot:?}"
            );
            let remaining = deadline
                .checked_duration_since(now)
                .expect("remaining service capture wait");

            tokio::select! {
                output = service.next() => {
                    let _ = output;
                }
                _ = notify.notified() => {}
                _ = tokio::time::sleep(remaining) => {
                    let snapshot = captured.lock().expect("lock captured text frames").clone();
                    panic!("timed out waiting for {context}; captured {snapshot:?}");
                }
            }
        }
    }

    fn service_with_config(config: OutboxServiceConfig) -> TestOutboxService {
        OutboxService::with_capabilities_and_config(
            TestNip11Capability,
            TestFullHistoryCapability,
            TestEventIngestCapability,
            config,
        )
    }

    #[test]
    fn effect_accumulator_preserves_transport_sends() {
        let relay = relay("accumulator-sends");
        let mut accumulator = OutboxEffectAccumulator::default();
        accumulator.record(OutboxPoolOutput {
            transport_effects: vec![
                OutboxTransportEffect::SendRelayFrame {
                    relay: relay.clone(),
                    generation: 9,
                    message: close_message("first"),
                },
                OutboxTransportEffect::SendRelayFrame {
                    relay: relay.clone(),
                    generation: 9,
                    message: close_message("second"),
                },
            ],
            ..Default::default()
        });

        let output = accumulator.finish();

        match output.transport_effects.as_slice() {
            [OutboxTransportEffect::SendRelayFrame {
                relay: first_relay,
                generation: 9,
                message: first_message,
            }, OutboxTransportEffect::SendRelayFrame {
                relay: second_relay,
                generation: 9,
                message: second_message,
            }] => {
                assert_eq!(first_relay, &relay);
                assert_eq!(second_relay, &relay);
                assert_eq!(
                    first_message.to_json().expect("serialize first close"),
                    close_message("first")
                        .to_json()
                        .expect("serialize expected")
                );
                assert_eq!(
                    second_message.to_json().expect("serialize second close"),
                    close_message("second")
                        .to_json()
                        .expect("serialize expected")
                );
            }
            other => panic!("unexpected transport effects: {other:?}"),
        }
    }

    #[test]
    fn nip11_fetch_state_blocks_in_flight_and_retries_after_failure_deadline() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut state = Nip11RelayFetchState::default();

        assert!(state.ready_to_fetch(now));
        let attempt = state.next_attempt();
        assert_eq!(attempt, 1);
        state.mark_dispatched(attempt);
        assert!(!state.ready_to_fetch(now));

        let retry_after = nip11_failure_retry_after(&relay("retry"), state.attempt);
        state.in_flight_attempt = None;
        state.next_fetch_at = now.checked_add(retry_after);

        assert!(!state.ready_to_fetch(now + retry_after - Duration::from_nanos(1)));
        assert!(state.ready_to_fetch(now + retry_after));
    }

    #[test]
    fn nip11_success_refreshes_after_one_hour_and_resets_attempts() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_100);
        let relay = relay("success");
        let mut service = Nip11Driver::default();
        let state = service.relays.entry(relay.clone()).or_default();
        let attempt = state.next_attempt();
        assert_eq!(attempt, 1);
        state.mark_dispatched(attempt);

        service.mark_success(&relay, now);

        let state = service.relays.get(&relay).expect("relay state");
        assert_eq!(state.in_flight_attempt, None);
        assert_eq!(state.attempt, 0);
        assert_eq!(
            state.next_fetch_at,
            now.checked_add(NIP11_REFRESH_AFTER_SUCCESS)
        );
    }

    #[test]
    fn nip11_driver_prunes_state_for_relays_without_candidates() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_150);
        let retained = relay("retained");
        let removed = relay("removed");
        let mut service = Nip11Driver::default();
        service.relays.entry(retained.clone()).or_default();
        service.relays.entry(removed.clone()).or_default();

        service.retain_interests(&[active_nip11_interest(retained.clone())], now);

        assert!(service.relays.contains_key(&retained));
        assert!(!service.relays.contains_key(&removed));
    }

    #[test]
    fn nip11_driver_keeps_in_flight_state_when_candidate_temporarily_disappears() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_200);
        let relay = relay("candidate-churn");
        let mut service = Nip11Driver::default();
        service.relays.entry(relay.clone()).or_default();
        let request = service
            .start_fetch(relay.clone(), now)
            .expect("initial fetch starts");

        service.retain_interests(&[], now);

        assert!(service.is_current_result(&request));
        assert_eq!(
            service.available_fetch_capacity(),
            NIP11_FETCH_CONCURRENCY - 1
        );
        assert!(service
            .ready_interests([active_nip11_interest(relay)], now)
            .is_empty());
    }

    #[test]
    fn nip11_driver_keeps_future_fetch_state_when_candidate_temporarily_disappears() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_300);
        let relay = relay("future-fetch");
        let mut service = Nip11Driver::default();
        service
            .relays
            .entry(relay.clone())
            .or_default()
            .next_fetch_at = now.checked_add(Duration::from_secs(30));

        service.retain_interests(&[], now);

        assert!(service.relays.contains_key(&relay));
        assert!(service
            .ready_interests([active_nip11_interest(relay)], now)
            .is_empty());
    }

    #[test]
    fn nip11_driver_suspended_at_wakes_for_reprojection_without_fetching() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_350);
        let wake_at = now + Duration::from_secs(30);
        let relay = relay("suspended-at");
        let mut service = Nip11Driver::default();

        let ready = service.ready_interests(
            [nip11_interest(
                relay.clone(),
                Nip11InterestState::Suspended(Nip11InterestResume::At(wake_at)),
            )],
            now,
        );

        assert!(ready.is_empty());
        assert_eq!(service.next_deadline(), Some(wake_at));
        assert_eq!(
            service.relays.get(&relay).and_then(|state| state.suspended),
            Some(Nip11InterestResume::At(wake_at))
        );
    }

    #[test]
    fn nip11_driver_suspended_on_relay_input_has_no_timer_deadline() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_360);
        let relay = relay("suspended-input");
        let mut service = Nip11Driver::default();

        let ready = service.ready_interests(
            [nip11_interest(
                relay.clone(),
                Nip11InterestState::Suspended(Nip11InterestResume::OnRelayInput),
            )],
            now,
        );

        assert!(ready.is_empty());
        assert_eq!(service.next_deadline(), None);
        assert_eq!(
            service.relays.get(&relay).and_then(|state| state.suspended),
            Some(Nip11InterestResume::OnRelayInput)
        );
    }

    #[test]
    fn nip11_service_readiness_deadline_applies_suspended_interest() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_365);
        let wake_at = now + Duration::from_secs(30);
        let relay = relay("service-suspended-at");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = Nip11Service::new(CapturingNip11Capability { sender });

        let deadline = service.next_readiness_deadline(Nip11ReadinessInput {
            now,
            interests: vec![nip11_interest(
                relay,
                Nip11InterestState::Suspended(Nip11InterestResume::At(wake_at)),
            )],
        });

        assert_eq!(deadline, Some(wake_at));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn nip11_service_readiness_ranks_active_interests_and_enforces_concurrency() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_370);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = Nip11Service::new(CapturingNip11Capability { sender });
        let mut interests = (0..=NIP11_FETCH_CONCURRENCY)
            .map(|index| {
                let relay = relay(&format!("service-rank-{index}"));
                Nip11InterestRead {
                    relay,
                    rank: Nip11InterestRank {
                        priority: Some(RelayConnectionPriority {
                            strongest_demand: if index == 0 {
                                RelayDemandPriority::Critical
                            } else {
                                RelayDemandPriority::BestEffort
                            },
                            request_count: index,
                        }),
                        source_rank: 0,
                        connection_weight: index as u32,
                        health_rank: (false, 0),
                    },
                    state: Nip11InterestState::Active,
                }
            })
            .collect::<Vec<_>>();
        let lowest = interests
            .iter()
            .find(|interest| interest.rank.priority.unwrap().request_count == 1)
            .expect("lowest ranked interest")
            .relay
            .clone();
        let critical = interests[0].relay.clone();

        service.apply_readiness(Nip11ReadinessInput {
            now,
            interests: std::mem::take(&mut interests),
        });

        let requested = (0..NIP11_FETCH_CONCURRENCY)
            .map(|_| receiver.try_recv().expect("NIP-11 request").relay)
            .collect::<HashSet<_>>();
        assert!(requested.contains(&critical));
        assert!(!requested.contains(&lowest));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn nip11_service_recv_returns_limits_and_waits_for_apply_ack() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_380);
        let relay = relay("service-success");
        let raw = Nip11LimitationsRaw {
            max_subscriptions: Some(12),
            ..Default::default()
        };
        let mut service = Nip11Service::new(StaticNip11Capability {
            result: Ok(raw.clone()),
        });

        service.apply_readiness(Nip11ReadinessInput {
            now,
            interests: vec![active_nip11_interest(relay.clone())],
        });

        match service.recv().await.expect("NIP-11 output") {
            Nip11ServiceOutput::Limits {
                request,
                raw: output_raw,
            } => {
                assert_eq!(request.relay, relay);
                assert_eq!(request.attempt, 1);
                assert_eq!(*output_raw, raw);
                assert_eq!(
                    service.next_deadline(),
                    None,
                    "raw fetch success should stay in-flight until limits are consumed"
                );

                let consumed_at = now + Duration::from_secs(1);
                service.apply_limits_ack(request, Nip11ApplyAck::Consumed, consumed_at);
                assert_eq!(
                    service.next_deadline(),
                    consumed_at.checked_add(NIP11_REFRESH_AFTER_SUCCESS)
                );
            }
            other => panic!("unexpected NIP-11 output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn nip11_service_deferred_apply_ack_retries_with_short_backoff() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_385);
        let relay = relay("service-deferred");
        let raw = Nip11LimitationsRaw {
            max_subscriptions: Some(12),
            ..Default::default()
        };
        let mut service = Nip11Service::new(StaticNip11Capability {
            result: Ok(raw.clone()),
        });

        service.apply_readiness(Nip11ReadinessInput {
            now,
            interests: vec![active_nip11_interest(relay.clone())],
        });
        let request = match service.recv().await.expect("NIP-11 output") {
            Nip11ServiceOutput::Limits {
                request,
                raw: output_raw,
            } => {
                assert_eq!(*output_raw, raw);
                request
            }
            other => panic!("unexpected NIP-11 output: {other:?}"),
        };

        let deferred_at = now + Duration::from_secs(1);
        service.apply_limits_ack(request, Nip11ApplyAck::Deferred, deferred_at);
        let deadline = service
            .next_deadline()
            .expect("deferred apply should schedule retry");
        assert!(deadline >= deferred_at + NIP11_APPLY_DEFERRED_BACKOFF_BASE);
        assert!(deadline <= deferred_at + MAX_NIP11_APPLY_DEFERRED_BACKOFF);

        service.apply_readiness(Nip11ReadinessInput {
            now: deadline,
            interests: vec![active_nip11_interest(relay.clone())],
        });
        match service.recv().await.expect("retried NIP-11 output") {
            Nip11ServiceOutput::Limits { request, .. } => {
                assert_eq!(request.relay, relay);
                assert_eq!(request.attempt, 2);
            }
            other => panic!("unexpected NIP-11 retry output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn nip11_service_recv_failure_frees_capacity_for_refill() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_390);
        let first = relay("service-failure-first");
        let second = relay("service-failure-second");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = Nip11Service::new(CapturingNip11Capability { sender });

        service.apply_readiness(Nip11ReadinessInput {
            now,
            interests: vec![active_nip11_interest(first.clone())],
        });
        assert_eq!(
            receiver.try_recv().expect("first NIP-11 request").relay,
            first
        );

        assert!(matches!(
            service.recv().await.expect("NIP-11 output"),
            Nip11ServiceOutput::FetchFailed
        ));

        service.apply_readiness(Nip11ReadinessInput {
            now,
            interests: vec![active_nip11_interest(second.clone())],
        });
        assert_eq!(
            receiver.try_recv().expect("refill NIP-11 request").relay,
            second
        );
        assert!(service.next_deadline().is_some());
    }

    #[test]
    fn nip11_failure_uses_uniform_retry_base() {
        let retry = nip11_failure_retry_after(&relay("failure"), 1);

        assert!(retry >= NIP11_FAILURE_BACKOFF_BASE);
        assert!(retry <= MAX_NIP11_FAILURE_BACKOFF);
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

    #[test]
    fn derive_relay_limitations_clamps_huge_raw_values() {
        let fallback = RelayLimitations::default();
        let raw = Nip11LimitationsRaw {
            max_subscriptions: Some(i64::MAX),
            max_message_length: Some(i64::MAX),
            ..Default::default()
        };

        let derived = derive_relay_limitations_from_raw(&raw, fallback);
        let caps = RelayLimitCaps::default();
        assert_eq!(derived.maximum_subs, caps.maximum_subs);
        assert_eq!(derived.max_json_bytes, caps.max_json_bytes);

        let coordinator = crate::relay::RelayCoordinatorLimits::new(derived);
        assert_eq!(
            coordinator.sub_guardian.available_passes(),
            caps.maximum_subs
        );
    }

    #[tokio::test]
    async fn nip11_driver_skips_cap_blocked_low_value_remote_relay() {
        let relay = relay("nip11-low-value-cap");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = OutboxService::with_capabilities(
            CapturingNip11Capability { sender },
            TestFullHistoryCapability,
            TestEventIngestCapability,
        );
        let id = service.id_registry().next_sub_id();

        let _ = service.set_max_websocket_connections(Some(0));
        let _ = service.set_live(
            id,
            trivial_filter(),
            remote_relay_pkgs(
                relay.clone(),
                RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        poll_service_progress(&mut service).await;

        assert!(
            receiver.try_recv().is_err(),
            "cap-blocked low-value remote relay should not dispatch NIP-11"
        );
    }

    #[test]
    fn nip11_projection_suspends_cap_blocked_low_value_remote_relay() {
        let relay = relay("nip11-low-value-suspended");
        let mut service = service();
        let id = service.id_registry().next_sub_id();

        let _ = service.set_max_websocket_connections(Some(0));
        let _ = service.set_live(
            id,
            trivial_filter(),
            remote_relay_pkgs(
                relay.clone(),
                RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );

        let input = service
            .relay
            .nip11_readiness_input(Instant::now(), SystemTime::now());
        let interest = input
            .interests
            .iter()
            .find(|interest| interest.relay == relay)
            .expect("NIP-11 interest");
        assert_eq!(
            interest.state,
            Nip11InterestState::Suspended(Nip11InterestResume::OnRelayInput)
        );
    }

    #[test]
    fn nip11_projection_suspends_low_value_remote_transport_retry_until_deadline() {
        let relay = relay("nip11-low-value-health");
        let mut service = service();
        let id = service.id_registry().next_sub_id();
        let service_now = Instant::now();
        let fetch_now = SystemTime::now();

        let _ = service.set_live(
            id,
            trivial_filter(),
            remote_relay_pkgs(
                relay.clone(),
                RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        service.note_low_value_transport_retry(
            &relay,
            service_now,
            LowValueOpenBackoffReason::TransportFailure,
        );

        let input = service.relay.nip11_readiness_input(service_now, fetch_now);
        let interest = input
            .interests
            .iter()
            .find(|interest| interest.relay == relay)
            .expect("NIP-11 interest");
        assert!(matches!(
            interest.state,
            Nip11InterestState::Suspended(Nip11InterestResume::At(deadline))
                if deadline > fetch_now
        ));
    }

    #[tokio::test]
    async fn nip11_driver_does_not_reserve_low_value_remote_capacity() {
        let relay_a = relay("a-nip11-cap");
        let relay_b = relay("b-nip11-cap");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = OutboxService::with_capabilities(
            CapturingNip11Capability { sender },
            TestFullHistoryCapability,
            TestEventIngestCapability,
        );

        let _ = service.set_max_websocket_connections(Some(1));
        for relay in [relay_a.clone(), relay_b.clone()] {
            let id = service.id_registry().next_sub_id();
            let _ = service.set_live(
                id,
                trivial_filter(),
                remote_relay_pkgs(
                    relay,
                    RelayDemandPriority::Opportunistic,
                    crate::relay::RelayRoutingPreference::NoPreference,
                ),
            );
        }
        let _ = service.apply_nip11_readiness();

        let relays = [receiver.try_recv(), receiver.try_recv()]
            .into_iter()
            .map(|request| request.expect("NIP-11 request").relay)
            .collect::<HashSet<_>>();
        assert_eq!(relays, HashSet::from([relay_a, relay_b]));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn nip11_driver_keeps_explicit_and_important_demand_prompt() {
        let explicit = relay("nip11-explicit-cap");
        let important = relay("nip11-important-cap");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = OutboxService::with_capabilities(
            CapturingNip11Capability { sender },
            TestFullHistoryCapability,
            TestEventIngestCapability,
        );

        let _ = service.set_max_websocket_connections(Some(0));
        let explicit_id = service.id_registry().next_sub_id();
        let important_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            explicit_id,
            trivial_filter(),
            explicit_relay_pkgs(explicit.clone()),
        );
        let _ = service.set_live(
            important_id,
            trivial_filter(),
            remote_relay_pkgs(
                important.clone(),
                RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        let relays =
            wait_for_nip11_request_relays(&mut service, &receiver, 2, Duration::from_secs(5)).await;
        assert_eq!(relays, HashSet::from([explicit, important]));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn publish_only_demand_does_not_dispatch_nip11() {
        let relay = relay("publish-only-no-nip11");
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = OutboxService::with_capabilities(
            CapturingNip11Capability { sender },
            TestFullHistoryCapability,
            TestEventIngestCapability,
        );
        let _ = service.set_max_websocket_connections(Some(0));
        let msg = EventClientMessage {
            note_json: r#"{"id":"publish-only-no-nip11"}"#.to_owned(),
        };

        let _ = service.publish(msg, vec![RelayId::Websocket(relay.clone())]);
        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.has_pending_publish(&relay));
        assert!(
            receiver.try_recv().is_err(),
            "queued publish-only demand should not start a NIP-11 fetch"
        );
    }

    #[tokio::test]
    async fn nip11_relay_unknown_apply_defers_success_ack() {
        let now = SystemTime::now();
        let relay = relay("unknown-apply-defers");
        let mut service = OutboxService::with_capabilities(
            StaticNip11Capability {
                result: Ok(Nip11LimitationsRaw::default()),
            },
            TestFullHistoryCapability,
            TestEventIngestCapability,
        );

        service.nip11.apply_readiness(Nip11ReadinessInput {
            now,
            interests: vec![active_nip11_interest(relay)],
        });
        let output = service.nip11.recv().await.expect("NIP-11 output");
        let _ = service.apply_nip11_service_output(output);

        let deadline = service
            .nip11
            .next_deadline()
            .expect("unknown relay should defer NIP-11 apply");
        assert!(deadline <= SystemTime::now() + MAX_NIP11_APPLY_DEFERRED_BACKOFF);
    }

    #[tokio::test]
    async fn nip11_applied_and_unchanged_limits_consume_success_ack() {
        for (name, raw) in [
            ("unchanged", Nip11LimitationsRaw::default()),
            (
                "applied",
                Nip11LimitationsRaw {
                    max_subscriptions: Some(7),
                    ..Default::default()
                },
            ),
        ] {
            let relay = relay(&format!("{name}-apply-accepts"));
            let mut service = OutboxService::with_capabilities(
                StaticNip11Capability { result: Ok(raw) },
                TestFullHistoryCapability,
                TestEventIngestCapability,
            );
            let id = service.id_registry().next_sub_id();
            let _ = service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay.clone()));

            service.nip11.apply_readiness(Nip11ReadinessInput {
                now: SystemTime::now(),
                interests: vec![active_nip11_interest(relay)],
            });
            let output = service.nip11.recv().await.expect("NIP-11 output");
            let _ = service.apply_nip11_service_output(output);

            let deadline = service
                .nip11
                .next_deadline()
                .expect("consumed NIP-11 apply should schedule refresh");
            assert!(
                deadline > SystemTime::now() + Duration::from_secs(30 * 60),
                "consumed NIP-11 apply should use success refresh, not short deferred retry"
            );
        }
    }

    #[tokio::test]
    async fn next_yields_ready_service_output() {
        let relay =
            NormRelayUrl::new("wss://service-ready-output.example.com").expect("valid relay url");
        let expected = OutboxServiceOutput::Events(vec![OutboxEvent::RelayStatusChanged {
            relay,
            status: Some(RelayStatus::Connected),
        }]);
        let mut service = service();
        service.outputs.ready_outputs.push_back(expected.clone());

        assert_eq!(service.next().await, expected);
    }

    #[tokio::test]
    async fn service_websocket_req_eose_updates_relay_eose_fact() {
        let (_relay_task, relay, sent) = create_eose_relay().await;
        let mut service = service();
        let id = service.id_registry().next_sub_id();

        service.begin_effect_turn();
        assert_eq!(
            service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay)),
            OutboxServiceOutput::NoEvents,
        );
        let _ = service.end_effect_turn();

        let event = wait_for_service_event(&mut service, Duration::from_secs(5), |event| {
            matches!(
                event,
                OutboxEvent::OutboxSubRelayEoseChanged {
                    id: event_id,
                    relay_eose: Some(relay_eose),
                } if *event_id == id && relay_eose.any_eose
            )
        })
        .await;
        sent.notified().await;

        assert!(matches!(
            event,
            OutboxEvent::OutboxSubRelayEoseChanged {
                relay_eose: Some(relay_eose),
                ..
            } if relay_eose.all_eosed
        ));
    }

    #[tokio::test]
    async fn service_live_eose_updates_when_full_history_is_active() {
        let (_relay_task, relay) = create_negentropy_eose_relay().await;
        let mut service = OutboxService::with_capabilities(
            TestNip11Capability,
            EmptyFullHistoryCapability,
            TestEventIngestCapability,
        );
        let live_id = service.id_registry().next_sub_id();
        let history_id = service.id_registry().next_full_history_id();

        service.begin_effect_turn();
        assert_eq!(
            service.set_live(
                live_id,
                trivial_filter(),
                explicit_relay_pkgs(relay.clone()),
            ),
            OutboxServiceOutput::NoEvents,
        );
        assert_eq!(
            service.set_full_history_targets(
                history_id,
                vec![FullHistoryTarget::new(
                    trivial_filter(),
                    vec![explicit_relay_pkgs(relay)],
                )],
            ),
            OutboxServiceOutput::NoEvents,
        );
        let _ = service.end_effect_turn();

        let event = wait_for_service_event(&mut service, Duration::from_secs(5), |event| {
            matches!(
                event,
                OutboxEvent::OutboxSubRelayEoseChanged {
                    id,
                    relay_eose: Some(relay_eose),
                } if *id == live_id && relay_eose.any_eose
            )
        })
        .await;

        assert!(matches!(
            event,
            OutboxEvent::OutboxSubRelayEoseChanged {
                relay_eose: Some(relay_eose),
                ..
            } if relay_eose.all_eosed
        ));
    }

    #[tokio::test]
    async fn full_history_only_sends_neg_open_through_service() {
        let (_relay_task, relay, captured, notify) =
            crate::relay::test_utils::create_text_capture_relay().await;
        let mut service = OutboxService::with_capabilities(
            TestNip11Capability,
            EmptyFullHistoryCapability,
            TestEventIngestCapability,
        );
        let history_id = service.id_registry().next_full_history_id();

        service.begin_effect_turn();
        assert_eq!(
            service.set_full_history_targets(
                history_id,
                vec![FullHistoryTarget::new(
                    trivial_filter(),
                    vec![explicit_relay_pkgs(relay)],
                )],
            ),
            OutboxServiceOutput::NoEvents,
        );
        let _ = service.end_effect_turn();

        let neg_open = wait_for_service_capture(
            &mut service,
            &captured,
            &notify,
            Duration::from_secs(5),
            "full-history NEG-OPEN",
            |text| text.starts_with("[\"NEG-OPEN\","),
        )
        .await;

        assert!(neg_open.starts_with("[\"NEG-OPEN\","));
    }

    #[tokio::test]
    async fn full_history_relay_open_waits_for_ready_local_set() {
        let relay = relay("full-history-open-before-local-set");
        let mut service = service();
        let history_id = service.id_registry().next_full_history_id();

        assert_eq!(
            service.set_full_history_targets(
                history_id,
                vec![FullHistoryTarget::new(
                    trivial_filter(),
                    vec![explicit_relay_pkgs(relay.clone())],
                )],
            ),
            OutboxServiceOutput::NoEvents
        );

        assert_eq!(
            service.apply_relay_transport_opened(relay, 0),
            OutboxServiceOutput::NoEvents
        );
    }

    #[tokio::test]
    async fn clear_full_history_removes_service_transport_demand() {
        let relay = relay("full-history-demand-clear");
        let mut service = service();
        let history_id = service.id_registry().next_full_history_id();

        let _ = service.set_full_history_targets(
            history_id,
            vec![FullHistoryTarget::new(
                trivial_filter(),
                vec![explicit_relay_pkgs(relay.clone())],
            )],
        );

        assert!(
            service.relay.transport.demand_for(&relay).is_some(),
            "full-history target should contribute service-owned transport demand"
        );

        let _ = service.clear_full_history(history_id);

        assert_eq!(
            service.relay.transport.demand_for(&relay),
            None,
            "clearing full-history should remove service-owned transport demand"
        );
    }

    #[tokio::test]
    async fn publish_sends_event_to_websocket_relay() {
        let (_relay_task, relay, captured, notify) =
            crate::relay::test_utils::create_text_capture_relay().await;
        let mut service = service();
        let signer = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("service websocket publish test")
            .sign(&signer.secret_key.secret_bytes())
            .build()
            .expect("build websocket publish note");
        let note_id_hex = hex::encode(note.id());
        let msg = EventClientMessage::try_from(&note).expect("note converts to EVENT message");

        let _ = service.publish(msg, vec![RelayId::Websocket(relay)]);

        let frame = wait_for_service_capture(
            &mut service,
            &captured,
            &notify,
            Duration::from_secs(2),
            "published EVENT frame",
            |text| text.starts_with("[\"EVENT\",") && text.contains(&note_id_hex),
        )
        .await;

        assert!(frame.contains(&note_id_hex));
    }

    #[tokio::test]
    async fn publish_queues_payload_in_service_when_socket_open_is_deferred() {
        let relay = relay("service-publish-deferred");
        let mut service = service();
        let _ = service.set_max_websocket_connections(Some(0));
        let msg = EventClientMessage {
            note_json: r#"{"id":"service-owned-publish-queue"}"#.to_owned(),
        };

        let _ = service.publish(msg, vec![RelayId::Websocket(relay.clone())]);

        assert!(
            service.relay.transport.has_pending_publish(&relay),
            "publish payload should stay in service-owned relay transport state"
        );
        assert!(
            !service.pool.relays.contains_key(&relay),
            "publish should not call into the pool before relay maintenance"
        );

        assert!(
            tokio::time::timeout(Duration::from_millis(20), service.next())
                .await
                .is_err(),
            "zero websocket cap should leave the service waiting after async maintenance runs"
        );

        assert!(
            !service.relay.transport.websockets.contains_key(&relay),
            "zero websocket cap should defer the socket open"
        );
    }

    #[tokio::test]
    async fn live_demand_open_is_deferred_by_zero_cap_via_service_next() {
        let relay = relay("service-live-zero-cap");
        let mut service = service();
        let _ = service.set_max_websocket_connections(Some(0));
        let id = service.id_registry().next_sub_id();

        let _ = service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay.clone()));

        assert!(
            tokio::time::timeout(Duration::from_millis(20), service.next())
                .await
                .is_err(),
            "zero websocket cap should leave the service waiting after async maintenance runs"
        );
        assert!(service.pool.relay_transport_demand(&relay).is_some());
        assert!(!service.relay.transport.websockets.contains_key(&relay));
    }

    #[tokio::test]
    async fn service_open_work_sorts_by_priority_count_then_relay_url() {
        let relay_low_url_first = relay("a-admission");
        let relay_critical_one_b = relay("b-admission");
        let relay_critical_one_c = relay("c-admission");
        let relay_critical_many = relay("z-admission");
        let mut service = service();
        let _ = service.set_max_websocket_connections(Some(2));

        for (relay, priority) in [
            (
                relay_low_url_first.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
            ),
            (
                relay_critical_one_b.clone(),
                crate::relay::RelayDemandPriority::Critical,
            ),
            (
                relay_critical_one_c.clone(),
                crate::relay::RelayDemandPriority::Critical,
            ),
            (
                relay_critical_many.clone(),
                crate::relay::RelayDemandPriority::Critical,
            ),
            (
                relay_critical_many.clone(),
                crate::relay::RelayDemandPriority::Critical,
            ),
        ] {
            let id = service.id_registry().next_sub_id();
            let _ = service.set_live(
                id,
                trivial_filter(),
                remote_relay_pkgs(
                    relay,
                    priority,
                    crate::relay::RelayRoutingPreference::NoPreference,
                ),
            );
        }

        poll_service_progress(&mut service).await;

        assert!(service
            .relay
            .transport
            .websockets
            .contains_key(&relay_critical_many));
        assert!(service
            .relay
            .transport
            .websockets
            .contains_key(&relay_critical_one_b));
        assert!(!service
            .relay
            .transport
            .websockets
            .contains_key(&relay_critical_one_c));
        assert!(!service
            .relay
            .transport
            .websockets
            .contains_key(&relay_low_url_first));
    }

    #[tokio::test]
    async fn service_connecting_cap_defers_low_value_demand_without_dropping_it() {
        let blocker = relay("connecting-blocker");
        let target = relay("connecting-deferred");
        let mut service =
            service_with_config(OutboxServiceConfig::default().with_max_connecting_websockets(1));

        let blocker_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            blocker_id,
            trivial_filter(),
            remote_relay_pkgs(
                blocker.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        install_connecting_service_websocket(&mut service, blocker.clone(), 1);

        let target_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            target_id,
            trivial_filter(),
            remote_relay_pkgs(
                target.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );

        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.websockets.contains_key(&blocker));
        assert!(!service.relay.transport.websockets.contains_key(&target));
        assert!(service.pool.relay_transport_demand(&target).is_some());
        assert!(service
            .relay
            .admission
            .state
            .deferrals
            .contains_key(&target));
    }

    #[tokio::test]
    async fn service_connecting_cap_preempts_lower_value_connecting_for_stronger_demand() {
        let low_value = relay("connecting-low-value");
        let critical = relay("connecting-critical");
        let mut service =
            service_with_config(OutboxServiceConfig::default().with_max_connecting_websockets(1));

        let low_value_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            low_value_id,
            trivial_filter(),
            remote_relay_pkgs(
                low_value.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        install_connecting_service_websocket(&mut service, low_value.clone(), 1);

        let critical_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            critical_id,
            trivial_filter(),
            explicit_relay_pkgs_with_priority(
                critical.clone(),
                crate::relay::RelayDemandPriority::Critical,
            ),
        );

        let output = tokio::time::timeout(Duration::from_millis(20), service.next())
            .await
            .expect("stronger demand should produce admission replacement output");
        let OutboxServiceOutput::Events(events) = output else {
            panic!("stronger demand should emit relay status events");
        };

        assert!(!service.relay.transport.websockets.contains_key(&low_value));
        assert!(service.relay.transport.websockets.contains_key(&critical));
        assert!(
            events.contains(&OutboxEvent::RelayStatusChanged {
                relay: low_value.clone(),
                status: Some(RelayStatus::Disconnected),
            }),
            "evicted relay status must be forwarded to the read model"
        );
        assert!(
            events.contains(&OutboxEvent::RelayStatusChanged {
                relay: critical.clone(),
                status: Some(RelayStatus::Connecting),
            }),
            "incoming relay status must be forwarded to the read model"
        );
        assert_eq!(
            service
                .relay
                .transport
                .websockets
                .get(&critical)
                .map(|leg| leg.conn.status),
            Some(RelayStatus::Connecting)
        );
        assert!(service.pool.relay_transport_demand(&low_value).is_some());
        assert!(!service
            .relay
            .admission
            .state
            .deferrals
            .contains_key(&critical));
    }

    #[tokio::test]
    async fn service_admission_defers_equal_priority_and_stronger_demand_bypasses() {
        let connected = relay("connected-peer");
        let target = relay("no-victim-target");
        let mut service = service();
        let _ = service.set_max_websocket_connections(Some(1));

        let connected_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            connected_id,
            trivial_filter(),
            remote_relay_pkgs(
                connected.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        install_connected_service_websocket(&mut service, connected.clone(), 1);

        let target_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            target_id,
            trivial_filter(),
            remote_relay_pkgs(
                target.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );

        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.websockets.contains_key(&connected));
        assert!(!service.relay.transport.websockets.contains_key(&target));
        assert!(service
            .relay
            .admission
            .state
            .deferrals
            .contains_key(&target));
        assert!(!service
            .relay
            .admission
            .state
            .transport_health
            .contains_key(&target));

        let upgraded_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            upgraded_id,
            trivial_filter(),
            remote_relay_pkgs(
                target.clone(),
                crate::relay::RelayDemandPriority::Important,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );

        poll_service_progress(&mut service).await;

        assert!(!service.relay.transport.websockets.contains_key(&connected));
        assert!(service.relay.transport.websockets.contains_key(&target));
        assert!(!service
            .relay
            .admission
            .state
            .deferrals
            .contains_key(&target));
    }

    #[tokio::test]
    async fn service_admission_deferral_is_bypassed_after_policy_change() {
        let connected = relay("policy-connected-peer");
        let target = relay("policy-target");
        let mut service = service();
        let _ = service.set_max_websocket_connections(Some(1));

        let connected_id = service.id_registry().next_sub_id();
        let target_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            connected_id,
            trivial_filter(),
            remote_relay_pkgs(
                connected.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        install_connected_service_websocket(&mut service, connected.clone(), 1);
        let _ = service.set_live(
            target_id,
            trivial_filter(),
            remote_relay_pkgs(
                target.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );

        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.websockets.contains_key(&connected));
        assert!(!service.relay.transport.websockets.contains_key(&target));
        assert!(service
            .relay
            .admission
            .state
            .deferrals
            .contains_key(&target));

        let _ = service.set_max_websocket_connections(Some(2));
        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.websockets.contains_key(&connected));
        assert!(service.relay.transport.websockets.contains_key(&target));
        assert!(!service
            .relay
            .admission
            .state
            .deferrals
            .contains_key(&target));
    }

    #[tokio::test]
    async fn service_reconnect_deadline_blocks_open_until_due() {
        let relay = relay("transport-retry");
        let mut service = service();
        let id = service.id_registry().next_sub_id();
        let _ = service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay.clone()));
        service.relay.transport.reconnects.insert(
            relay.clone(),
            RelayReconnectState {
                attempt: 0,
                retry_at: Instant::now() + Duration::from_secs(30),
            },
        );

        poll_service_progress(&mut service).await;

        assert!(!service.relay.transport.websockets.contains_key(&relay));

        service.relay.transport.reconnects.insert(
            relay.clone(),
            RelayReconnectState {
                attempt: 0,
                retry_at: Instant::now() - Duration::from_millis(1),
            },
        );

        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.websockets.contains_key(&relay));
        assert!(service
            .pool
            .relays
            .get(&relay)
            .and_then(|relay| relay.current_generation())
            .is_none());
    }

    #[tokio::test]
    async fn service_websocket_cap_sheds_remote_advertised_before_explicit_peer() {
        let remote = relay("a-remote-advertised");
        let explicit = relay("z-explicit");
        let mut service = service();

        let remote_id = service.id_registry().next_sub_id();
        let explicit_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            remote_id,
            trivial_filter(),
            remote_relay_pkgs(
                remote.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        let _ = service.set_live(
            explicit_id,
            trivial_filter(),
            explicit_relay_pkgs_with_priority(
                explicit.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
            ),
        );
        install_connected_service_websocket(&mut service, remote.clone(), 1);
        install_connected_service_websocket(&mut service, explicit.clone(), 2);

        let _ = service.set_max_websocket_connections(Some(1));

        assert!(!service.relay.transport.websockets.contains_key(&remote));
        assert!(service.relay.transport.websockets.contains_key(&explicit));
    }

    #[tokio::test]
    async fn service_websocket_cap_uses_current_demand_when_shedding() {
        let removed = relay("removed-important");
        let retained = relay("retained-remote");
        let mut service = service();

        let removed_id = service.id_registry().next_sub_id();
        let retained_id = service.id_registry().next_sub_id();
        let _ = service.set_live(
            removed_id,
            trivial_filter(),
            explicit_relay_pkgs(removed.clone()),
        );
        let _ = service.set_live(
            retained_id,
            trivial_filter(),
            remote_relay_pkgs(
                retained.clone(),
                crate::relay::RelayDemandPriority::Opportunistic,
                crate::relay::RelayRoutingPreference::NoPreference,
            ),
        );
        install_connected_service_websocket(&mut service, removed.clone(), 1);
        install_connected_service_websocket(&mut service, retained.clone(), 2);

        let _ = service.clear_live(removed_id);
        let _ = service.set_max_websocket_connections(Some(1));

        assert!(!service.relay.transport.websockets.contains_key(&removed));
        assert!(service.relay.transport.websockets.contains_key(&retained));
    }

    #[tokio::test]
    async fn service_demand_loss_closes_idle_websocket_on_next_turn() {
        let relay = relay("demand-boundary");
        let mut service = service();
        let id = service.id_registry().next_sub_id();
        let _ = service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay.clone()));
        install_connected_service_websocket(&mut service, relay.clone(), 1);

        let _ = service.clear_live(id);
        poll_service_progress(&mut service).await;

        assert!(!service.relay.transport.websockets.contains_key(&relay));
    }

    #[tokio::test]
    async fn service_transport_open_does_not_install_pool_generation_until_ready() {
        let relay = relay("reconnect-deadline");
        let mut service = service();
        let id = service.id_registry().next_sub_id();
        let _ = service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay.clone()));

        poll_service_progress(&mut service).await;

        assert!(service.relay.transport.websockets.contains_key(&relay));
        assert!(service
            .pool
            .relays
            .get(&relay)
            .and_then(|relay| relay.current_generation())
            .is_none());
    }

    #[tokio::test]
    async fn websocket_event_frame_ingests_without_pool_relay_state() {
        let relay = relay("event-ingest-bypass");
        let generation = 17;
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut service = OutboxService::with_capabilities(
            TestNip11Capability,
            TestFullHistoryCapability,
            CapturingEventIngestCapability { sender },
        );
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), websocket_leg(&relay, generation));
        let signer = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("service websocket ingest test")
            .sign(&signer.secret_key.secret_bytes())
            .build()
            .expect("build websocket ingest note");
        let note_json = note.json().expect("serialize note");
        let note_value: serde_json::Value =
            serde_json::from_str(&note_json).expect("note json value");
        let frame = serde_json::json!(["EVENT", "subid", note_value]).to_string();

        let output = service.apply_relay_ws_message(
            RelayTransportReady {
                relay: relay.clone(),
                generation,
            },
            WsMessage::Text(frame.clone()),
        );

        assert_eq!(output, OutboxServiceOutput::NoEvents);
        assert!(!service.pool.relays.contains_key(&relay));
        let request = receiver.try_recv().expect("captured event ingest");
        assert_eq!(request.relay_url, relay.to_string());
        assert_eq!(request.relay_type, RelayImplType::Websocket);
        assert_eq!(request.ingest_json, frame);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn service_config_applies_reconnect_settings_to_service_state() {
        let reconnect_delay = Duration::from_millis(17);
        let reconnect_backoff_base = Duration::from_millis(29);
        let service = service_with_config(
            OutboxServiceConfig::default()
                .with_keepalive_reconnect_delay(reconnect_delay)
                .with_keepalive_reconnect_backoff_base(reconnect_backoff_base),
        );

        assert_eq!(
            service.relay.config.keepalive_reconnect_delay,
            reconnect_delay
        );
        assert_eq!(
            service.relay.config.keepalive_reconnect_backoff_base,
            reconnect_backoff_base
        );
    }

    #[tokio::test]
    async fn websocket_liveness_ping_updates_service_last_ping() {
        let relay = relay("liveness-ping");
        let generation = 8;
        let mut service = service_with_config(
            OutboxServiceConfig::default()
                .with_keepalive_ping_rate(Duration::from_millis(1))
                .with_pong_timeout(Duration::from_secs(60)),
        );
        let mut leg = websocket_leg(&relay, generation);
        let old_ping = Instant::now() - Duration::from_secs(30);
        leg.last_ping = old_ping;
        leg.last_pong = Instant::now();
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), leg);

        let output = service.apply_websocket_liveness_timer();

        assert_eq!(output, OutboxServiceOutput::NoEvents);
        assert!(
            service
                .relay
                .transport
                .websockets
                .get(&relay)
                .expect("service websocket")
                .last_ping
                > old_ping
        );
    }

    #[tokio::test]
    async fn websocket_pong_updates_service_last_pong_for_current_generation() {
        let relay = relay("liveness-pong");
        let generation = 11;
        let mut service = service();
        let mut leg = websocket_leg(&relay, generation);
        let old_pong = Instant::now() - Duration::from_secs(30);
        leg.last_pong = old_pong;
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), leg);

        let output = service.apply_relay_ws_message(
            RelayTransportReady {
                relay: relay.clone(),
                generation,
            },
            WsMessage::Pong(vec![]),
        );

        assert_eq!(output, OutboxServiceOutput::NoEvents);
        assert!(
            service
                .relay
                .transport
                .websockets
                .get(&relay)
                .expect("service websocket")
                .last_pong
                > old_pong
        );
    }

    #[tokio::test]
    async fn websocket_liveness_removes_stale_pong_leg() {
        let relay = relay("liveness-timeout");
        let generation = 12;
        let mut service = service_with_config(
            OutboxServiceConfig::default().with_pong_timeout(Duration::from_millis(1)),
        );
        let mut leg = websocket_leg(&relay, generation);
        let now = Instant::now();
        leg.last_ping = now;
        leg.last_pong = now - Duration::from_secs(30);
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), leg);

        let output = service.apply_websocket_liveness_timer();

        assert_eq!(
            output,
            OutboxServiceOutput::Events(vec![OutboxEvent::RelayStatusChanged {
                relay: relay.clone(),
                status: Some(RelayStatus::Disconnected),
            }])
        );
        assert!(!service.relay.transport.websockets.contains_key(&relay));
    }

    #[tokio::test]
    async fn websocket_liveness_timeout_preserves_connect_backoff_for_connecting_leg() {
        let relay = relay("liveness-connecting-timeout");
        let generation = 13;
        let reconnect_delay = Duration::from_millis(10);
        let reconnect_backoff_base = Duration::from_millis(100);
        let mut service = service_with_config(
            OutboxServiceConfig::default()
                .with_pong_timeout(Duration::from_millis(1))
                .with_keepalive_reconnect_delay(reconnect_delay)
                .with_keepalive_reconnect_backoff_base(reconnect_backoff_base),
        );
        let mut conn =
            WebsocketConn::new(relay.clone().into(), || {}).expect("test websocket conn");
        conn.set_send_generation(generation);
        let mut leg = ServiceWebsocketLeg::new(conn, generation);
        let now = Instant::now();
        leg.last_ping = now;
        leg.last_pong = now - Duration::from_secs(30);
        service
            .relay
            .transport
            .websockets
            .insert(relay.clone(), leg);
        service.relay.transport.reconnects.insert(
            relay.clone(),
            RelayReconnectState {
                attempt: 2,
                retry_at: now,
            },
        );

        let before_timeout = Instant::now();
        let output = service.apply_websocket_liveness_timer();

        assert_eq!(
            output,
            OutboxServiceOutput::Events(vec![OutboxEvent::RelayStatusChanged {
                relay: relay.clone(),
                status: Some(RelayStatus::Disconnected),
            }])
        );
        assert!(!service.relay.transport.websockets.contains_key(&relay));
        let reconnect = service
            .relay
            .transport
            .reconnects
            .get(&relay)
            .expect("reconnect state");
        assert_eq!(reconnect.attempt, 2);
        assert!(
            reconnect.retry_at >= before_timeout + reconnect_backoff_base * 4,
            "connecting timeout should use exponential reconnect backoff, got {:?}",
            reconnect.retry_at.saturating_duration_since(before_timeout)
        );
    }

    #[tokio::test]
    async fn missing_websocket_send_reports_closed_to_pool() {
        let relay = relay("missing-send");
        let mut service = service();

        let output = service.activate_transport_effect(OutboxTransportEffect::SendRelayFrame {
            relay: relay.clone(),
            generation: 0,
            message: close_message("missing"),
        });

        assert_eq!(
            output,
            OutboxServiceOutput::Events(vec![OutboxEvent::RelayStatusChanged {
                relay,
                status: Some(RelayStatus::Disconnected),
            }])
        );
    }

    #[tokio::test]
    async fn set_live_updates_retained_relay_policy() {
        let relay = relay("set-live-policy");
        let mut service = service();
        let id = service.id_registry().next_sub_id();

        service.begin_effect_turn();
        assert_eq!(
            service.set_live(
                id,
                trivial_filter(),
                remote_relay_pkgs(
                    relay.clone(),
                    crate::relay::RelayDemandPriority::BestEffort,
                    crate::relay::RelayRoutingPreference::NoPreference,
                ),
            ),
            OutboxServiceOutput::NoEvents
        );
        assert_eq!(
            service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay.clone())),
            OutboxServiceOutput::NoEvents
        );

        let demand = service
            .pool
            .relay_transport_demand(&relay)
            .expect("retained relay demand");
        assert_eq!(
            demand.priority,
            crate::relay::RelayConnectionPriority::from_demand(
                crate::relay::RelayDemandPriority::Important,
                1,
            )
            .expect("important relay demand")
        );
        assert_eq!(demand.source, crate::relay::RelayUrlSource::Explicit);
    }

    #[test]
    fn effect_turn_clear_live_mutates_pool_immediately() {
        let relay = relay("effect-turn-clear-live");
        let mut service = service();
        let id = service.id_registry().next_sub_id();

        let _ = service.set_live(id, trivial_filter(), explicit_relay_pkgs(relay));
        assert!(service.pool.subs.get(&id).is_some());
        service.begin_effect_turn();
        assert_eq!(service.clear_live(id), OutboxServiceOutput::NoEvents);

        assert!(service.pool.subs.get(&id).is_none());
    }
}
