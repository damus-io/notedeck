use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering as AtomicOrdering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    relay::subscription::OutboxSubscription,
    relay::{
        backoff,
        coordinator::{CoordinationData, CoordinationOutput, RecvResponse, RelayEoseDelta},
        frame::{QueuedRelayFrame, RelayFrameSink},
        negentropy::{NegentropyData, NegentropyRelay},
        same_canonical_filter_set, FullHistoryRelayFilter, FullHistorySubId,
        FullRelayPkgsModificationTask, ModifyTask, Nip11ApplyOutcome, NormRelayUrl, OutboxSubId,
        OutboxSubscriptions, RelayConnectionPriority, RelayDemandPriority, RelayLegReadiness,
        RelayLimitations, RelayReqId, RelayReqStatus, RelayType, RelayUrlPkgs, RelayUrlSource,
        SubPass, SubscribeTask,
    },
};

fn run_negentropy_relay_with_frames<T>(
    generation: Option<u64>,
    data: &mut NegentropyData,
    f: impl FnOnce(&mut NegentropyRelay<'_>) -> T,
) -> (T, Vec<QueuedRelayFrame>) {
    let mut relay = NegentropyRelay::new(RelayFrameSink::transport(generation), data);
    let result = f(&mut relay);
    let frames = relay.take_frames();
    (result, frames)
}
mod admission;
mod eose;
mod fd_pressure;
mod full_history;
mod output;
mod service;

#[cfg(test)]
#[path = "full_history/tests.rs"]
mod full_history_tests;

use eose::{ChangedRelayLeg, EoseTracker, FullyEosedEffectsPlan};
use fd_pressure::{FdPressureGate, RelayAdmissionPolicy};
use full_history::{FullHistoryFetchRequest, FullHistoryOutput, FullHistoryRuntime};
pub use full_history::{
    FullHistoryLocalPresenceRequest, FullHistoryLocalPresenceResult, FullHistoryLocalSetRequest,
    FullHistoryPendingIngestionPresenceRequest, FullHistoryPendingIngestionPresenceResult,
};
use output::{
    OutboxFullHistoryEffect, OutboxPoolFact, OutboxPoolOutput, OutboxTransportEffect,
    RelayDemandChanged,
};
pub use service::{
    EventIngestCapability, EventIngestRequest, FullHistoryCapability, FullHistoryLocalSetResult,
    Nip11Capability, Nip11FetchRequest, OutboxEvent, OutboxService, OutboxServiceConfig,
    OutboxServiceOutput,
};

const DEFAULT_KEEPALIVE_PING_RATE: Duration = Duration::from_secs(45);
const PONG_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_RECONNECT_BACKOFF_BASE: Duration = Duration::from_secs(5);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30 * 60); // 30 minutes
const REMOTE_TRANSPORT_FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(30);
const MAX_REMOTE_TRANSPORT_FAILURE_BACKOFF: Duration = Duration::from_secs(30 * 60);

fn aggregate_outbox_sub_relay_eose(
    readiness: impl IntoIterator<Item = RelayLegReadiness>,
) -> OutboxSubRelayEose {
    let mut tracked_relays = 0usize;
    let mut unsupported_relays = 0usize;
    let mut any_eose = false;
    let mut all_eosed = true;

    for readiness in readiness {
        match readiness {
            RelayLegReadiness::Placed(RelayReqStatus::Eose) => {
                tracked_relays += 1;
                any_eose = true;
            }
            RelayLegReadiness::Placed(_) | RelayLegReadiness::PendingPlacement => {
                tracked_relays += 1;
                all_eosed = false;
            }
            RelayLegReadiness::Unsupported => {
                unsupported_relays += 1;
            }
        }
    }

    if tracked_relays == 0 {
        all_eosed = false;
    }

    OutboxSubRelayEose {
        tracked_relays,
        unsupported_relays,
        any_eose,
        all_eosed,
    }
}

/// OutboxPool owns active relay coordinators and applies exact subscription
/// transitions to retained protocol state.
pub struct OutboxPool {
    id_registry: OutboxIdRegistry,
    relays: HashMap<NormRelayUrl, CoordinationData>,
    subs: OutboxSubscriptions,
    eose_tracker: EoseTracker,
    demand: RelayDemandSnapshot,
}

/// Cloneable allocator for concrete outbox ids owned by one outbox service.
#[derive(Clone, Default)]
pub struct OutboxIdRegistry {
    next_sub_id: Arc<AtomicU64>,
    next_full_history_id: Arc<AtomicU64>,
}

impl OutboxIdRegistry {
    /// Create one fresh outbox id namespace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate one live subscription id from this namespace.
    pub fn next_sub_id(&self) -> OutboxSubId {
        OutboxSubId(self.next_sub_id.fetch_add(1, AtomicOrdering::Relaxed))
    }

    /// Allocate one full-history subscription id from this namespace.
    pub fn next_full_history_id(&self) -> FullHistorySubId {
        FullHistorySubId(
            self.next_full_history_id
                .fetch_add(1, AtomicOrdering::Relaxed),
        )
    }

    #[cfg(test)]
    fn next_sub_id_value_for_test(&self) -> u64 {
        self.next_sub_id.load(AtomicOrdering::Relaxed)
    }
}

/// Aggregate relay EOSE readiness for every desired relay leg of one
/// [`OutboxSubId`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutboxSubRelayEose {
    /// Number of serviceable relay legs considered for readiness.
    pub tracked_relays: usize,
    /// Number of desired relay legs the outbox cannot service.
    pub unsupported_relays: usize,
    /// Whether any tracked relay has reached EOSE.
    pub any_eose: bool,
    /// Whether all tracked relay legs have reached EOSE.
    pub all_eosed: bool,
}

pub(super) struct RelayAdmissionState {
    fd_pressure: FdPressureGate,
    max_websocket_connections: Option<usize>,
    deferrals: HashMap<NormRelayUrl, RelayAdmissionDeferral>,
    generation: u64,
    transport_health: HashMap<NormRelayUrl, RelayTransportHealth>,
}

impl Default for RelayAdmissionState {
    fn default() -> Self {
        Self {
            fd_pressure: FdPressureGate::default(),
            max_websocket_connections: None,
            deferrals: HashMap::new(),
            generation: 0,
            transport_health: HashMap::new(),
        }
    }
}

impl RelayAdmissionState {
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn set_max_websocket_connections(&mut self, max: Option<usize>) {
        let previous = self.max_websocket_connections;
        if previous != max {
            self.bump_generation();
        }
        self.max_websocket_connections = max;
    }

    fn enter_hard_failure_from_websocket_error(&mut self, error: &crate::WebSocketError) -> bool {
        let entered = self
            .fd_pressure
            .enter_hard_failure_from_websocket_error(error);
        if entered {
            self.bump_generation();
        }
        entered
    }
}

#[derive(Default)]
struct RelayDemandSnapshot {
    /// Last aggregate relay demand emitted to the service.
    current: HashMap<NormRelayUrl, RelayTransportDemand>,
}

impl Default for OutboxPool {
    fn default() -> Self {
        Self::with_id_registry(OutboxIdRegistry::new())
    }
}

impl OutboxPool {
    /// Build a pool using the provided concrete outbox id namespace.
    fn with_id_registry(id_registry: OutboxIdRegistry) -> Self {
        Self {
            id_registry,
            relays: HashMap::new(),
            eose_tracker: EoseTracker::default(),
            subs: Default::default(),
            demand: RelayDemandSnapshot::default(),
        }
    }

    /// Allocate one live subscription id from this pool's internal registry.
    pub(in crate::relay::outbox) fn next_sub_id(&mut self) -> OutboxSubId {
        self.id_registry.next_sub_id()
    }

    fn apply_relay_demand_change(
        &mut self,
        relay: &NormRelayUrl,
        next: Option<RelayTransportDemand>,
    ) -> Option<RelayDemandChanged> {
        let previous = self.demand.current.get(relay).copied();
        if previous == next {
            return None;
        }

        if let Some(next) = next {
            self.demand.current.insert(relay.clone(), next);
        } else {
            self.demand.current.remove(relay);
        }
        Some(RelayDemandChanged {
            relay: relay.clone(),
            demand: next,
        })
    }

    /// Applies an already planned set of post-EOSE subscription effects.
    fn apply_fully_eosed_effects(&mut self, plan: FullyEosedEffectsPlan) -> OutboxPoolOutput {
        let mut output = OutboxPoolOutput::default();
        let remove_oneshots = plan.remove_oneshots;
        for id in remove_oneshots {
            output.extend(self.clear_fetch(id));
        }

        let Some(now) = plan.optimize_since_at else {
            return output;
        };
        for id in plan.optimize_since {
            self.subs.see_all(&id, now);
        }
        output
    }

    /// Returns true when every currently requested relay leg for this
    /// subscription is owned by compaction, making `since` advancement safe.
    fn is_fully_compaction_routed(&self, id: OutboxSubId) -> bool {
        let Some(sub) = self.subs.get(&id) else {
            return false;
        };
        if sub.relays.is_empty() {
            return false;
        }

        sub.relays.iter().all(|relay_id| {
            self.relays
                .get(relay_id)
                .and_then(|relay| relay.route_type(&id))
                == Some(RelayType::Compaction)
        })
    }

    /// Classifies fully-EOSE subscriptions into concrete lifecycle effects.
    ///
    /// Fully completed oneshots are removed immediately. `since` advancement is
    /// only safe for subscriptions whose entire current relay set is routed
    /// through compaction.
    fn plan_fully_eosed_effects(&self, ids: HashSet<OutboxSubId>) -> FullyEosedEffectsPlan {
        let mut remove_oneshots = HashSet::new();
        let mut optimize_since = HashSet::new();

        for id in ids {
            if self.subs.is_oneshot(&id) {
                remove_oneshots.insert(id);
                continue;
            }

            if self.is_fully_compaction_routed(id) {
                optimize_since.insert(id);
            }
        }

        let optimize_since_at = if optimize_since.is_empty() {
            None
        } else {
            unix_now_secs()
        };

        FullyEosedEffectsPlan {
            remove_oneshots,
            optimize_since,
            optimize_since_at,
        }
    }

    /// Applies derived side effects for subscriptions that reached full EOSE in
    /// the current transition.
    fn apply_fully_eosed_effects_for_ids(
        &mut self,
        fully_eosed: HashSet<OutboxSubId>,
    ) -> OutboxPoolOutput {
        let effects = self.plan_fully_eosed_effects(fully_eosed);
        if effects.is_empty() {
            return OutboxPoolOutput::default();
        }

        self.apply_fully_eosed_effects(effects)
    }

    fn finish_exact_relay_transition(
        &mut self,
        changed_legs: Vec<ChangedRelayLeg>,
        removed_subs: HashSet<OutboxSubId>,
        mut output: OutboxPoolOutput,
    ) -> OutboxPoolOutput {
        let fully_eosed = self.apply_exact_relay_transition_readiness(&changed_legs, &removed_subs);
        output
            .facts
            .extend(self.exact_relay_transition_facts(&changed_legs, &removed_subs));
        output.extend(self.apply_fully_eosed_effects_for_ids(fully_eosed));
        output
    }

    fn apply_exact_relay_subscribe(
        &mut self,
        relay_id: &NormRelayUrl,
        id: OutboxSubId,
    ) -> OutboxPoolOutput {
        self.ensure_relay(relay_id);
        if self.relay_subscription_ids_unsupported(relay_id) {
            tracing::debug!(
                "relay {relay_id} skipped subscription work after NIP-11 max_subid_length rejection"
            );
            return OutboxPoolOutput::default();
        }

        self.apply_relay_subscribe(relay_id, id)
    }

    fn apply_exact_relay_replace_subscribe(
        &mut self,
        relay_id: &NormRelayUrl,
        id: OutboxSubId,
    ) -> OutboxPoolOutput {
        self.ensure_relay(relay_id);
        if self.relay_subscription_ids_unsupported(relay_id) {
            tracing::debug!(
                "relay {relay_id} skipped subscription work after NIP-11 max_subid_length rejection"
            );
            return OutboxPoolOutput::default();
        }

        self.apply_relay_replace_subscribe(relay_id, id)
    }

    fn apply_exact_relay_unsubscribe(
        &mut self,
        relay_id: &NormRelayUrl,
        id: OutboxSubId,
    ) -> OutboxPoolOutput {
        self.ensure_relay(relay_id);
        if self.relay_subscription_ids_unsupported(relay_id) {
            tracing::debug!(
                "relay {relay_id} skipped subscription work after NIP-11 max_subid_length rejection"
            );
            return OutboxPoolOutput::default();
        }

        self.apply_relay_unsubscribe(relay_id, id)
    }

    /// Create or replace one retained live subscription and return transition output.
    pub(super) fn set_live(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    ) -> OutboxPoolOutput {
        let Some(filters) = prune_empty_filters(filters) else {
            return self.clear_live(id);
        };

        let mut replacement = FullRelayPkgsModificationTask {
            filters,
            relays: relay_pkgs,
        };
        retain_allowed_relay_pkgs(&mut replacement.relays);

        let Some(previous) = self.subs.get(&id) else {
            return self.set_new_live(id, replacement);
        };
        if full_relay_pkgs_modification_is_noop(previous, &replacement) {
            return OutboxPoolOutput::default();
        }
        if replacement.relays.urls.is_empty() {
            return self.clear_live(id);
        }

        let previous_relays = previous.relays.clone();
        let next_relays = replacement.relays.urls.clone();
        let filters_changed = !same_canonical_filter_set(
            previous.filters.get_filters(),
            replacement.filters.as_slice(),
        );
        let routing_changed =
            previous.routing_preference != replacement.relays.routing_preference();

        let removed_relays = sorted_relay_vec(previous_relays.difference(&next_relays).cloned());
        let replaced_relays = if filters_changed || routing_changed {
            sorted_relay_vec(next_relays.iter().cloned())
        } else {
            Vec::new()
        };
        let added_relays = if replaced_relays.is_empty() {
            sorted_relay_vec(next_relays.difference(&previous_relays).cloned())
        } else {
            Vec::new()
        };

        let changed_legs = changed_legs_for_relay_sets(
            id,
            [&removed_relays, &replaced_relays, &added_relays]
                .into_iter()
                .flat_map(|relays| relays.iter().cloned()),
        );
        let removed_subs = HashSet::new();
        let _ = self
            .subs
            .ingest_task(&id, ModifyTask::FullRelayPkgs(replacement));

        let mut output = OutboxPoolOutput::default();
        for relay in removed_relays {
            output.extend(self.apply_exact_relay_unsubscribe(&relay, id));
        }
        for relay in replaced_relays {
            output.extend(self.apply_exact_relay_replace_subscribe(&relay, id));
        }
        for relay in added_relays {
            output.extend(self.apply_exact_relay_subscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    fn set_new_live(
        &mut self,
        id: OutboxSubId,
        replacement: FullRelayPkgsModificationTask,
    ) -> OutboxPoolOutput {
        if replacement.relays.urls.is_empty() {
            return OutboxPoolOutput::default();
        }

        let relays = sorted_relay_vec(replacement.relays.urls.iter().cloned());
        let changed_legs = changed_legs_for_relay_sets(id, relays.iter().cloned());
        let removed_subs = HashSet::from([id]);
        self.subs.new_subscription(
            id,
            SubscribeTask {
                filters: replacement.filters,
                relays: replacement.relays,
            },
            false,
        );

        let mut output = OutboxPoolOutput::default();
        for relay in relays {
            output.extend(self.apply_exact_relay_subscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    /// Remove one retained live subscription and return transition output.
    pub(super) fn clear_live(&mut self, id: OutboxSubId) -> OutboxPoolOutput {
        self.clear_subscription_where(id, |_| true)
    }

    fn clear_subscription_where(
        &mut self,
        id: OutboxSubId,
        should_clear: impl FnOnce(&OutboxSubscription) -> bool,
    ) -> OutboxPoolOutput {
        let Some(sub) = self.subs.get(&id) else {
            return OutboxPoolOutput::default();
        };
        if !should_clear(sub) {
            return OutboxPoolOutput::default();
        }

        let relays = sorted_relay_vec(sub.relays.iter().cloned());
        let changed_legs = changed_legs_for_relay_sets(id, relays.iter().cloned());
        let removed_subs = HashSet::from([id]);
        self.subs.remove(&id);

        let mut output = OutboxPoolOutput::default();
        for relay in relays {
            output.extend(self.apply_exact_relay_unsubscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    /// Remove one retained transient fetch and return transition output.
    pub(super) fn clear_fetch(&mut self, id: OutboxSubId) -> OutboxPoolOutput {
        self.clear_subscription_where(id, |sub| sub.is_oneshot)
    }

    /// Remove matching relay legs from retained full-history fetches and return transition output.
    pub(super) fn clear_full_history_fetch_relays_matching(
        &mut self,
        owner: FullHistorySubId,
        mut matches: impl FnMut(&NormRelayUrl, &Filter) -> bool,
    ) -> OutboxPoolOutput {
        let cancellations = self
            .subs
            .remove_full_history_fetch_relays_matching(owner, |relay, filter| {
                matches(relay, filter)
            });
        if cancellations.is_empty() {
            return OutboxPoolOutput::default();
        }

        let mut unsubscribes = Vec::new();
        let mut changed_legs = Vec::new();
        let mut removed_subs = HashSet::new();
        for cancellation in cancellations {
            let id = cancellation.id;
            for relay in cancellation.relays {
                changed_legs.push(ChangedRelayLeg {
                    relay: relay.clone(),
                    sub_id: id,
                });
                unsubscribes.push((relay, id));
            }
            if cancellation.removed_sub {
                removed_subs.insert(id);
            }
        }

        let mut output = OutboxPoolOutput::default();
        for (relay, id) in unsubscribes {
            output.extend(self.apply_exact_relay_unsubscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    /// Refresh retained full-history fetch relay policy and return exact relay output.
    pub(super) fn refresh_full_history_fetch_policies(
        &mut self,
        owner: FullHistorySubId,
        mut relay_pkgs_for: impl FnMut(&NormRelayUrl, &Filter) -> Option<RelayUrlPkgs>,
    ) -> OutboxPoolOutput {
        let refreshes = self
            .subs
            .refresh_full_history_fetch_policies(owner, |relay, filter| {
                relay_pkgs_for(relay, filter)
            });
        if refreshes.is_empty() {
            return OutboxPoolOutput::default();
        }

        let mut replacements = Vec::new();
        for refresh in refreshes {
            replacements.extend(refresh.relays.into_iter().map(|relay| (relay, refresh.id)));
        }

        replacements.sort_by(|(left_relay, left_id), (right_relay, right_id)| {
            left_relay
                .cmp(right_relay)
                .then_with(|| left_id.cmp(right_id))
        });
        let changed_legs = replacements
            .iter()
            .map(|(relay, id)| ChangedRelayLeg {
                relay: relay.clone(),
                sub_id: *id,
            })
            .collect::<Vec<_>>();
        let removed_subs = HashSet::new();
        let mut output = OutboxPoolOutput::default();
        for (relay, id) in replacements {
            output.extend(self.apply_exact_relay_replace_subscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    /// Start one transient fetch and return transition output.
    pub(super) fn start_fetch(
        &mut self,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relay_pkgs: RelayUrlPkgs,
    ) -> OutboxPoolOutput {
        let Some(filters) = prune_empty_filters(filters) else {
            return OutboxPoolOutput::default();
        };
        let mut subscribe = SubscribeTask {
            filters,
            relays: relay_pkgs,
        };
        retain_allowed_relay_pkgs(&mut subscribe.relays);

        if subscribe.relays.urls.is_empty() {
            return OutboxPoolOutput::default();
        }

        let new_relays = sorted_relay_vec(subscribe.relays.urls.iter().cloned());
        let changed_legs = changed_legs_for_relay_sets(id, new_relays.iter().cloned());
        let removed_subs = HashSet::from([id]);
        self.subs.new_subscription(id, subscribe, true);

        let mut output = OutboxPoolOutput::default();
        for relay in new_relays {
            output.extend(self.apply_exact_relay_subscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    /// Start full-history fetch requests lowered by the service runtime.
    pub(in crate::relay::outbox) fn start_full_history_fetches(
        &mut self,
        requests: Vec<(OutboxSubId, FullHistoryFetchRequest)>,
    ) -> OutboxPoolOutput {
        if requests.is_empty() {
            return OutboxPoolOutput::default();
        }

        let mut subscribes = Vec::new();
        let mut removed_subs = HashSet::new();
        for (id, mut fetch) in requests {
            retain_allowed_relay_pkgs(&mut fetch.subscribe.relays);
            if fetch.subscribe.relays.urls.is_empty() {
                continue;
            }

            subscribes.extend(
                sorted_relay_vec(fetch.subscribe.relays.urls.iter().cloned())
                    .into_iter()
                    .map(|relay| (relay, id)),
            );
            removed_subs.insert(id);
            self.subs.new_full_history_fetch_subscription(
                id,
                fetch.subscribe,
                fetch.owner,
                fetch.filter,
            );
        }

        if subscribes.is_empty() {
            return OutboxPoolOutput::default();
        }

        subscribes.sort_by(|(left_relay, left_id), (right_relay, right_id)| {
            left_relay
                .cmp(right_relay)
                .then_with(|| left_id.cmp(right_id))
        });
        let changed_legs = subscribes
            .iter()
            .map(|(relay, id)| ChangedRelayLeg {
                relay: relay.clone(),
                sub_id: *id,
            })
            .collect::<Vec<_>>();
        let mut output = OutboxPoolOutput::default();
        for (relay, id) in subscribes {
            output.extend(self.apply_exact_relay_subscribe(&relay, id));
        }
        self.finish_exact_relay_transition(changed_legs, removed_subs, output)
    }

    fn request_full_history_negentropy_capacity(
        &mut self,
        relay_id: &NormRelayUrl,
    ) -> Result<OutboxPoolOutput, crate::relay::coordinator::NegentropyCapacityError> {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return Err(crate::relay::coordinator::NegentropyCapacityError::Retry);
        };
        let output = relay.request_full_history_negentropy_capacity()?;
        Ok(self.apply_coordination_output(relay_id, output))
    }

    fn return_full_history_negentropy_capacity(
        &mut self,
        relay_id: &NormRelayUrl,
        pass: SubPass,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        let output = relay.return_full_history_negentropy_capacity(&self.subs, pass);
        self.apply_coordination_output(relay_id, output)
    }

    fn cancel_full_history_negentropy_owner(&mut self, id: FullHistorySubId) -> OutboxPoolOutput {
        let mut output = OutboxPoolOutput::default();
        let relay_ids = self.relays.keys().cloned().collect::<Vec<_>>();
        for relay_id in relay_ids {
            let Some(relay) = self.relays.get_mut(&relay_id) else {
                continue;
            };
            let coordination_output = relay.cancel_negentropy_owner(id);
            output.extend(self.apply_coordination_output(&relay_id, coordination_output));
        }
        output
    }

    fn cancel_full_history_negentropy_relay_filters(
        &mut self,
        id: FullHistorySubId,
        relay_filters: &[FullHistoryRelayFilter],
    ) -> OutboxPoolOutput {
        let mut by_relay: HashMap<NormRelayUrl, Vec<Filter>> = HashMap::new();
        for relay_filter in relay_filters {
            by_relay
                .entry(relay_filter.relay.clone())
                .or_default()
                .push(relay_filter.filter.clone());
        }

        let mut output = OutboxPoolOutput::default();
        for (relay_url, filters) in by_relay {
            let Some(relay) = self.relays.get_mut(&relay_url) else {
                continue;
            };
            let coordination_output = relay.cancel_negentropy_owner_filters(id, &filters);
            output.extend(self.apply_coordination_output(&relay_url, coordination_output));
        }
        output
    }

    fn apply_negentropy_timeout(
        &mut self,
        relay_id: &NormRelayUrl,
        now: Instant,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        let output = relay.apply_negentropy_timeout(now);
        self.apply_coordination_output(relay_id, output)
    }

    fn has_relay(&self, relay_id: &NormRelayUrl) -> bool {
        self.relays.contains_key(relay_id)
    }

    fn relay_subscription_ids_unsupported(&self, relay_id: &NormRelayUrl) -> bool {
        self.relays
            .get(relay_id)
            .is_some_and(|relay| !relay.supports_relay_subscription_ids())
    }

    fn apply_relay_subscribe(
        &mut self,
        relay_id: &NormRelayUrl,
        id: OutboxSubId,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        let output = relay.subscribe(&self.subs, id);
        self.apply_coordination_output(relay_id, output)
    }

    fn apply_relay_replace_subscribe(
        &mut self,
        relay_id: &NormRelayUrl,
        id: OutboxSubId,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        let output = relay.replace_subscribe(&self.subs, id);
        self.apply_coordination_output(relay_id, output)
    }

    fn apply_relay_unsubscribe(
        &mut self,
        relay_id: &NormRelayUrl,
        id: OutboxSubId,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        let output = relay.unsubscribe(&self.subs, id);
        self.apply_coordination_output(relay_id, output)
    }

    /// Applies one relay's coordination facts and any follow-up oneshot unsubs
    /// caused by newly completed EOSE state.
    fn apply_coordination_facts(
        &mut self,
        relay_id: &NormRelayUrl,
        facts: crate::relay::coordinator::CoordinationFacts,
    ) -> OutboxPoolOutput {
        let status_changed_sub_ids = facts.status_changed_sub_ids;
        let mut eose_delta = facts.eose_delta;
        eose_delta
            .invalidated_sub_ids
            .extend(facts.invalidated_sub_ids);
        for id in &eose_delta.invalidated_sub_ids {
            eose_delta.sub_ids.remove(id);
        }
        let mut output = self.apply_relay_eose_delta(relay_id, eose_delta);
        for id in status_changed_sub_ids {
            self.apply_relay_leg_readiness(relay_id, id);
            output.facts.extend(self.relay_leg_facts(relay_id, id));
        }
        output
    }

    fn apply_coordination_output(
        &mut self,
        relay_id: &NormRelayUrl,
        output: CoordinationOutput,
    ) -> OutboxPoolOutput {
        let relay_demand = output.relay_demand;
        let mut pool_output = self.apply_coordination_facts(relay_id, output.facts);
        pool_output
            .transport_effects
            .extend(Self::relay_frame_effects(relay_id, output.frames));
        pool_output.full_history_effects.extend(
            output
                .full_history_capacity_grants
                .into_iter()
                .map(|grant| OutboxFullHistoryEffect::NegentropyCapacityGranted {
                    relay: relay_id.clone(),
                    grant,
                }),
        );
        pool_output
            .full_history_effects
            .extend(output.negentropy_effects.into_iter().map(|effect| {
                OutboxFullHistoryEffect::NegentropyEffect {
                    relay: relay_id.clone(),
                    effect,
                }
            }));
        if let Some(relay_demand) = relay_demand {
            if let Some(change) = self.apply_relay_demand_change(relay_id, relay_demand) {
                pool_output.relay_demand_changes.push(change);
            }
        }
        pool_output
    }

    fn apply_negentropy_effects_after_release(
        &mut self,
        relay_id: &NormRelayUrl,
        effects: crate::relay::negentropy::NegentropyRelayEffects,
    ) -> OutboxPoolOutput {
        let followup = {
            let Some(coord) = self.relays.get_mut(relay_id) else {
                return OutboxPoolOutput::default();
            };
            coord.apply_negentropy_effects_after_release(&self.subs, effects)
        };
        self.apply_coordination_output(relay_id, followup)
    }

    fn relay_frame_effects(
        relay_id: &NormRelayUrl,
        frames: Vec<crate::relay::frame::QueuedRelayFrame>,
    ) -> Vec<OutboxTransportEffect> {
        frames
            .into_iter()
            .map(
                |(generation, message)| OutboxTransportEffect::SendRelayFrame {
                    relay: relay_id.clone(),
                    generation,
                    message,
                },
            )
            .collect()
    }

    /// Applies one relay's EOSE delta to the durable tracker and direct completion effects.
    fn apply_relay_eose_delta(
        &mut self,
        relay_id: &NormRelayUrl,
        delta: RelayEoseDelta,
    ) -> OutboxPoolOutput {
        let mut fully_eosed = HashSet::new();
        let mut output = OutboxPoolOutput::default();
        output
            .facts
            .extend(self.apply_relay_tracker_invalidations(relay_id, delta.invalidated_sub_ids));
        for id in delta.sub_ids {
            if self.subs.get(&id).is_none() {
                continue;
            }
            if self.eose_tracker.mark_relay_eose(relay_id, id, &self.subs) {
                fully_eosed.insert(id);
            }
            output.facts.extend(self.relay_leg_facts(relay_id, id));
        }

        output.extend(self.apply_fully_eosed_effects_for_ids(fully_eosed));
        output
    }

    /// Clears durable EOSE state for relay legs coordinator reset internally.
    fn apply_relay_tracker_invalidations(
        &mut self,
        relay_id: &NormRelayUrl,
        invalidated_sub_ids: HashSet<OutboxSubId>,
    ) -> Vec<OutboxPoolFact> {
        let mut facts = Vec::new();
        for id in invalidated_sub_ids {
            self.apply_relay_leg_readiness(relay_id, id);
            facts.extend(self.relay_leg_facts(relay_id, id));
        }
        facts
    }

    /// Return the effective relay limitations currently applied to one relay.
    #[must_use]
    pub(super) fn relay_limitations(&self, relay: &NormRelayUrl) -> Option<RelayLimitations> {
        self.relays.get(relay).map(CoordinationData::current_limits)
    }

    /// Mark a relay unsupported for subscription-id-bearing protocols.
    fn apply_unsupported_subid_length_inner(
        &mut self,
        relay: &NormRelayUrl,
        max_subid_length: usize,
    ) -> (Nip11ApplyOutcome, OutboxPoolOutput) {
        let (unsupported_output, evict_output) = {
            let Some(coord) = self.relays.get_mut(relay) else {
                return (Nip11ApplyOutcome::RelayUnknown, OutboxPoolOutput::default());
            };
            // NOTE: Relays whose advertised max_subid_length cannot carry our
            // RelayReqId are intentionally unsupported for subscription-id
            // protocols. We do not sweep every retained outbox/full-history
            // state bucket here; that work is abandoned with the unsupported
            // relay. TODO: add explicit unsupported-relay cleanup if retained
            // state needs to be reclaimed instead of ignored.
            let unsupported_output =
                coord.mark_subscription_id_length_unsupported(max_subid_length);
            if coord.current_generation().is_some() {
                tracing::debug!(
                    relay = %relay,
                    reason = ?RelayConnectionDropReason::UnsupportedSubIdLength,
                    "evicting relay websocket"
                );
            }
            let evict_output = coord.evict_websocket_leg_at();
            (unsupported_output, evict_output)
        };
        tracing::warn!(
            "nip11: {relay} rejected for subscription-id-bearing protocols because max_subid_length {} is below required {}",
            max_subid_length,
            RelayReqId::byte_len()
        );
        let mut output = OutboxPoolOutput::default();
        output.extend(self.apply_coordination_output(relay, unsupported_output));
        output.extend(self.apply_coordination_output(relay, evict_output));
        (
            Nip11ApplyOutcome::UnsupportedSubIdLength { max_subid_length },
            output,
        )
    }

    fn apply_relay_limitations_inner(
        &mut self,
        relay: &NormRelayUrl,
        limitations: RelayLimitations,
        active_negentropy_session_count: usize,
    ) -> (Nip11ApplyOutcome, OutboxPoolOutput) {
        let (current, derived, output) = {
            let Some(coord) = self.relays.get_mut(relay) else {
                return (Nip11ApplyOutcome::RelayUnknown, OutboxPoolOutput::default());
            };

            let current = coord.current_limits();
            let derived = limitations;

            // NOTE: Subscription-id support is intentionally not re-enabled by
            // later compatible NIP-11 data after a relay has advertised an
            // incompatible max_subid_length. TODO: support recovery by
            // clearing the unsupported latch and replaying retained demand.
            if derived == current {
                (current, derived, None)
            } else {
                let output = coord.set_limits(&self.subs, active_negentropy_session_count, derived);
                (current, derived, Some(output))
            }
        };

        let Some(coordination_output) = output else {
            tracing::debug!("nip11: {relay} limits unchanged");
            return (Nip11ApplyOutcome::Unchanged, OutboxPoolOutput::default());
        };

        tracing::info!(
            "nip11: {relay} limits updated — max_subs: {} -> {}, max_json_bytes: {} -> {}",
            current.maximum_subs,
            derived.maximum_subs,
            current.max_json_bytes,
            derived.max_json_bytes,
        );
        (
            Nip11ApplyOutcome::Applied,
            self.apply_coordination_output(relay, coordination_output),
        )
    }

    /// Apply unsupported sub-id length and return exact transition output.
    pub(super) fn apply_unsupported_subid_length(
        &mut self,
        relay: &NormRelayUrl,
        max_subid_length: usize,
    ) -> (Nip11ApplyOutcome, OutboxPoolOutput) {
        self.apply_unsupported_subid_length_inner(relay, max_subid_length)
    }

    /// Apply effective relay limitations and return exact transition output.
    pub(super) fn apply_relay_limit_update(
        &mut self,
        relay: &NormRelayUrl,
        limitations: RelayLimitations,
        active_negentropy_session_count: usize,
    ) -> (Nip11ApplyOutcome, OutboxPoolOutput) {
        self.apply_relay_limitations_inner(relay, limitations, active_negentropy_session_count)
    }

    fn ensure_relay(&mut self, relay_id: &NormRelayUrl) -> &mut CoordinationData {
        if !self.relays.contains_key(relay_id) {
            self.relays.insert(relay_id.clone(), build_relay());
        }

        self.relays
            .get_mut(relay_id)
            .expect("relay should exist after ensure")
    }

    /// Returns aggregate websocket demand and relay URL source for one relay.
    #[cfg(test)]
    fn relay_transport_demand(&self, relay_id: &NormRelayUrl) -> Option<RelayTransportDemand> {
        self.demand.current.get(relay_id).copied()
    }

    fn evict_relay_connection_for_reason(
        &mut self,
        relay_id: &NormRelayUrl,
        reason: RelayConnectionDropReason,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        if relay.current_generation().is_some() {
            tracing::debug!(
                relay = %relay_id,
                ?reason,
                "evicting relay websocket"
            );
        }
        let output = relay.evict_websocket_leg_at();
        self.apply_coordination_output(relay_id, output)
    }

    #[cfg(test)]
    fn status(&self, id: &OutboxSubId) -> HashMap<&NormRelayUrl, RelayReqStatus> {
        let mut status = HashMap::new();
        for (url, relay) in &self.relays {
            let Some(res) = relay.req_status(id) else {
                continue;
            };
            status.insert(url, res);
        }

        status
    }

    /// Return committed aggregate relay EOSE readiness for one subscription.
    fn outbox_sub_relay_eose(&self, id: &OutboxSubId) -> Option<OutboxSubRelayEose> {
        self.eose_tracker.sub_relay_eose(id)
    }

    fn current_relay_req_status(
        &self,
        id: OutboxSubId,
        relay: &NormRelayUrl,
    ) -> Option<RelayReqStatus> {
        self.relays.get(relay)?.req_status(&id)
    }

    /// Emit the current relay-local request status for one touched relay leg.
    fn relay_req_status_fact(&self, id: OutboxSubId, relay: NormRelayUrl) -> OutboxPoolFact {
        let status = self.current_relay_req_status(id, &relay);
        OutboxPoolFact::RelayReqStatus { id, relay, status }
    }

    fn current_relay_leg_readiness(
        &self,
        relay: &NormRelayUrl,
        id: OutboxSubId,
    ) -> Option<RelayLegReadiness> {
        let sub = self.subs.get(&id)?;
        if !sub.relays.contains(relay) {
            return None;
        }

        let Some(coord) = self.relays.get(relay) else {
            return Some(RelayLegReadiness::PendingPlacement);
        };
        if !coord.supports_relay_subscription_ids() {
            return Some(RelayLegReadiness::Unsupported);
        }
        Some(
            coord
                .req_status(&id)
                .map(RelayLegReadiness::Placed)
                .unwrap_or(RelayLegReadiness::PendingPlacement),
        )
    }

    fn apply_relay_leg_readiness(&mut self, relay: &NormRelayUrl, id: OutboxSubId) -> bool {
        match self.current_relay_leg_readiness(relay, id) {
            Some(readiness) => {
                self.eose_tracker
                    .set_relay_leg_readiness(relay.clone(), id, readiness)
            }
            None => self.eose_tracker.remove_relay_leg(relay, id),
        }
    }

    fn apply_exact_relay_transition_readiness(
        &mut self,
        changed_legs: &[ChangedRelayLeg],
        removed_subs: &HashSet<OutboxSubId>,
    ) -> HashSet<OutboxSubId> {
        let mut fully_eosed = HashSet::new();
        for leg in changed_legs {
            if self.apply_relay_leg_readiness(&leg.relay, leg.sub_id) {
                fully_eosed.insert(leg.sub_id);
            }
        }

        for id in removed_subs {
            if self.subs.get(id).is_none() {
                self.eose_tracker.remove_sub(id);
            }
        }
        fully_eosed
    }

    /// Emit aggregate readiness for one touched retained subscription id.
    fn sub_relay_eose_fact(&self, id: OutboxSubId) -> OutboxPoolFact {
        let relay_eose = self.outbox_sub_relay_eose(&id);
        OutboxPoolFact::OutboxSubRelayEose { id, relay_eose }
    }

    /// Emit both relay-local and aggregate facts for one touched relay leg.
    fn relay_leg_facts(&self, relay: &NormRelayUrl, id: OutboxSubId) -> Vec<OutboxPoolFact> {
        vec![
            self.relay_req_status_fact(id, relay.clone()),
            self.sub_relay_eose_fact(id),
        ]
    }

    fn exact_relay_transition_facts(
        &self,
        changed_legs: &[ChangedRelayLeg],
        removed_subs: &HashSet<OutboxSubId>,
    ) -> Vec<OutboxPoolFact> {
        let mut facts = Vec::new();
        let mut emitted_legs = HashSet::new();
        let mut sub_eose_ids = Vec::new();
        let mut emitted_sub_eose = HashSet::new();

        for leg in changed_legs {
            if emitted_legs.insert((leg.sub_id, leg.relay.clone())) {
                facts.push(self.relay_req_status_fact(leg.sub_id, leg.relay.clone()));
            }
            if emitted_sub_eose.insert(leg.sub_id) {
                sub_eose_ids.push(leg.sub_id);
            }
        }

        for id in removed_subs {
            if emitted_sub_eose.insert(*id) {
                sub_eose_ids.push(*id);
            }
        }

        for id in sub_eose_ids {
            facts.push(self.sub_relay_eose_fact(id));
        }
        facts
    }

    /// Returns true after any routed relay leg has observed EOSE for `id`.
    ///
    /// This checks both durable tracker state and relay-local EOSE status that
    /// may not have flushed into the tracker yet. Use `all_have_eose` for the
    /// tracker-based "all current relay legs reached EOSE" authority.
    #[cfg(test)]
    fn has_observed_eose(&self, id: &OutboxSubId) -> bool {
        if self.eose_tracker.has_any_eose(&self.subs, id) {
            return true;
        }

        for relay in self.relays.values() {
            if relay.req_status(id) == Some(RelayReqStatus::Eose) {
                return true;
            }
        }

        false
    }

    /// Returns true when every currently routed relay leg has reached EOSE.
    ///
    /// Unlike `has_observed_eose`, this is derived only from `EoseTracker`,
    /// after coordinator-local EOSE deltas have been flushed.
    #[cfg(test)]
    fn all_have_eose(&self, id: &OutboxSubId) -> bool {
        self.eose_tracker.is_fully_eosed(&self.subs, id)
    }

    /// Returns a clone of the filters for the given subscription ID.
    #[cfg(test)]
    fn filters(&self, id: &OutboxSubId) -> Option<&Vec<Filter>> {
        self.subs.stored_ref(id).map(|v| v.filters.get_filters())
    }

    /// Returns the retained relay URL set for the given subscription ID.
    #[cfg(test)]
    fn relays(&self, id: &OutboxSubId) -> Option<&HashSet<NormRelayUrl>> {
        self.subs.get(id).map(|sub| &sub.relays)
    }

    /// Returns the compaction-projected filters for the given subscription ID,
    /// applying any stored synthetic `since` cursor without mutating the base
    /// subscription filters.
    #[cfg(test)]
    fn compaction_filters(&self, id: &OutboxSubId) -> Option<Vec<Filter>> {
        self.subs.filters_for_compaction(id)
    }

    pub(super) fn apply_relay_transport_opened(
        &mut self,
        relay_id: NormRelayUrl,
        generation: u64,
    ) -> OutboxPoolOutput {
        let outcome = {
            let Some(relay) = self.relays.get_mut(&relay_id) else {
                return OutboxPoolOutput::default();
            };
            relay.apply_websocket_opened(&self.subs, DEFAULT_RECONNECT_DELAY, generation)
        };
        self.finish_relay_transport_opened_input(&relay_id, outcome)
    }

    pub(super) fn apply_relay_transport_closed(
        &mut self,
        relay_id: &NormRelayUrl,
        generation: u64,
        now: Instant,
    ) -> OutboxPoolOutput {
        let _ = now;
        let outcome = {
            let Some(relay) = self.relays.get_mut(relay_id) else {
                return OutboxPoolOutput::default();
            };
            relay.apply_websocket_closed(generation)
        };
        self.finish_relay_transport_opened_input(relay_id, outcome)
    }

    pub(super) fn apply_relay_transport_error(
        &mut self,
        relay_id: &NormRelayUrl,
        generation: u64,
        error: String,
        now: Instant,
    ) -> OutboxPoolOutput {
        let _ = now;
        let outcome = {
            let Some(relay) = self.relays.get_mut(relay_id) else {
                return OutboxPoolOutput::default();
            };
            relay.apply_websocket_error(generation, error)
        };
        self.finish_relay_transport_opened_input(relay_id, outcome)
    }

    pub(super) fn apply_relay_eose(
        &mut self,
        relay_id: &NormRelayUrl,
        generation: u64,
        sid: &str,
    ) -> OutboxPoolOutput {
        let outcome = {
            let Some(relay) = self.relays.get_mut(relay_id) else {
                return OutboxPoolOutput::default();
            };
            relay.apply_relay_eose(generation, sid)
        };
        self.finish_relay_transport_opened_input(relay_id, outcome)
    }

    pub(super) fn apply_relay_closed(
        &mut self,
        relay_id: &NormRelayUrl,
        generation: u64,
        sid: &str,
    ) -> OutboxPoolOutput {
        let outcome = {
            let Some(relay) = self.relays.get_mut(relay_id) else {
                return OutboxPoolOutput::default();
            };
            relay.apply_relay_closed(generation, sid)
        };
        self.finish_relay_transport_opened_input(relay_id, outcome)
    }

    pub(super) fn apply_relay_transport_pong(
        &mut self,
        relay_id: &NormRelayUrl,
        generation: u64,
    ) -> OutboxPoolOutput {
        let Some(relay) = self.relays.get_mut(relay_id) else {
            return OutboxPoolOutput::default();
        };
        relay.apply_websocket_pong(generation);
        OutboxPoolOutput::default()
    }

    fn finish_relay_transport_opened_input(
        &mut self,
        relay_id: &NormRelayUrl,
        outcome: RecvResponse,
    ) -> OutboxPoolOutput {
        self.apply_coordination_output(relay_id, outcome.output)
    }
}

fn sorted_relay_vec(relays: impl IntoIterator<Item = NormRelayUrl>) -> Vec<NormRelayUrl> {
    let mut relays = relays.into_iter().collect::<Vec<_>>();
    relays.sort_unstable();
    relays
}

fn changed_legs_for_relay_sets(
    sub_id: OutboxSubId,
    relays: impl IntoIterator<Item = NormRelayUrl>,
) -> Vec<ChangedRelayLeg> {
    relays
        .into_iter()
        .map(|relay| ChangedRelayLeg { relay, sub_id })
        .collect()
}

fn prune_empty_filters(mut filters: Vec<Filter>) -> Option<Vec<Filter>> {
    filters.retain(|filter| filter.num_elements() != 0);
    (!filters.is_empty()).then_some(filters)
}

fn retain_allowed_relay_pkgs(relays: &mut RelayUrlPkgs) {
    let source = relays.source();
    retain_allowed_relay_set(&mut relays.urls, source);
}

fn retain_allowed_relay_set(relays: &mut HashSet<NormRelayUrl>, source: RelayUrlSource) {
    relays.retain(|relay| relay.allowed_for_source(source));
}

fn full_relay_pkgs_modification_is_noop(
    sub: &OutboxSubscription,
    full: &FullRelayPkgsModificationTask,
) -> bool {
    sub.relays == full.relays.urls
        && sub.relay_policy_matches(&full.relays)
        && same_canonical_filter_set(sub.filters.get_filters(), full.filters.as_slice())
}

fn unix_now_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Admission deferral state for one relay whose websocket demand remains declared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelayAdmissionDeferral {
    retry_at: Instant,
    attempt: u32,
    demand: RelayTransportDemand,
    policy: RelayAdmissionPolicy,
    generation: u64,
}

/// Aggregate websocket demand for one relay URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RelayTransportDemand {
    priority: RelayConnectionPriority,
    source: RelayUrlSource,
    connection_weight: u32,
}

impl RelayTransportDemand {
    pub(in crate::relay) fn new(
        priority: RelayConnectionPriority,
        source: RelayUrlSource,
        connection_weight: u32,
    ) -> Self {
        Self {
            priority,
            source,
            connection_weight,
        }
    }

    pub(in crate::relay) fn merge_optional(
        left: Option<Self>,
        right: Option<Self>,
    ) -> Option<Self> {
        match (left, right) {
            (Some(left), Some(right)) => Some(Self {
                priority: left.priority.merge(right.priority),
                source: left.source.strongest(right.source),
                connection_weight: left.connection_weight.max(right.connection_weight),
            }),
            (Some(demand), None) | (None, Some(demand)) => Some(demand),
            (None, None) => None,
        }
    }

    fn low_value_remote_advertised(self) -> bool {
        self.source == RelayUrlSource::RemoteAdvertised
            && self.priority.strongest_demand < RelayDemandPriority::Important
    }
}

/// Low-value retry state used to avoid repeatedly spending remote-advertised
/// admission on relays with relay-specific auth or transport failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LowValueOpenBackoffReason {
    AuthRequired,
    TransportFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelayConnectionDropReason {
    IdleAfterUnsubscribe,
    UnsupportedSubIdLength,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RelayTransportHealth {
    low_value_retry_attempts: u32,
    low_value_retry_at: Option<Instant>,
    low_value_retry_reason: Option<LowValueOpenBackoffReason>,
}

impl RelayTransportHealth {
    fn note_low_value_retry(
        &mut self,
        relay_id: &NormRelayUrl,
        now: Instant,
        reason: LowValueOpenBackoffReason,
    ) {
        let attempt = self.low_value_retry_attempts;
        self.low_value_retry_attempts = self.low_value_retry_attempts.saturating_add(1);
        let retry_after = backoff::next_duration_from_base(
            attempt,
            REMOTE_TRANSPORT_FAILURE_BACKOFF_BASE,
            backoff::jitter_seed(relay_id, attempt),
            MAX_REMOTE_TRANSPORT_FAILURE_BACKOFF,
        );
        self.low_value_retry_at = Some(now + retry_after);
        self.low_value_retry_reason = Some(reason);
    }

    fn note_success(&mut self) {
        self.low_value_retry_attempts = 0;
        self.low_value_retry_at = None;
        self.low_value_retry_reason = None;
    }

    fn blocks_low_value_open(&self, now: Instant) -> bool {
        self.low_value_retry_at
            .is_some_and(|retry_at| now < retry_at)
    }
}

fn build_relay() -> CoordinationData {
    CoordinationData::new(RelayLimitations::default()) // TODO(kernelkind): add actual limitations
}

#[cfg(test)]
mod tests {
    use hashbrown::HashSet;
    use nostrdb::Filter;

    use super::*;
    use crate::relay::{
        test_utils::{create_req_capture_relay, filters_json, trivial_filter, MockWakeup},
        FullHistoryTarget, RelayDemandPriority, RelayLimitations, RelayRoutingPreference,
        RelayType, RelayUrlPkgs, RelayUrlPolicy,
    };
    use crate::test_support::outbox::{test_outbox_service, TestOutboxService};

    fn service() -> TestOutboxService {
        test_outbox_service()
    }

    fn open_relay_transport_for_test(
        pool: &mut OutboxPool,
        relay: &NormRelayUrl,
    ) -> OutboxPoolOutput {
        pool.apply_relay_transport_opened(relay.clone(), 1)
    }

    fn ensure_relay<'a>(
        pool: &'a mut OutboxPool,
        relay: &NormRelayUrl,
    ) -> &'a mut CoordinationData {
        pool.ensure_relay(relay)
    }

    fn apply_relay_limit_update_for_test(
        pool: &mut OutboxPool,
        relay: &NormRelayUrl,
        limitations: RelayLimitations,
    ) -> Nip11ApplyOutcome {
        let (outcome, output) = pool.apply_relay_limit_update(relay, limitations, 0);
        let _ = output;
        outcome
    }

    fn output_touches_relay(output: &OutboxPoolOutput, relay: &NormRelayUrl) -> bool {
        output.relay_demand_changes.iter().any(|change| &change.relay == relay)
            || output.transport_effects.iter().any(|effect| {
                matches!(effect, OutboxTransportEffect::SendRelayFrame { relay: effect_relay, .. } if effect_relay == relay)
            })
            || output.facts.iter().any(|fact| {
                matches!(fact, OutboxPoolFact::RelayReqStatus { relay: fact_relay, .. } if fact_relay == relay)
            })
    }

    fn pool_output_is_empty(output: &OutboxPoolOutput) -> bool {
        output.facts.is_empty()
            && output.relay_demand_changes.is_empty()
            && output.transport_effects.is_empty()
            && output.full_history_effects.is_empty()
    }

    fn apply_send_session_with<W, T>(
        pool: &mut OutboxPool,
        _wakeup: W,
        f: impl FnOnce(&mut OutboxPool, &mut OutboxPoolOutput) -> T,
    ) -> T {
        apply_send_session_and_collect_output(pool, _wakeup, f).0
    }

    fn apply_send_session_and_collect_output<W, T>(
        pool: &mut OutboxPool,
        _wakeup: W,
        f: impl FnOnce(&mut OutboxPool, &mut OutboxPoolOutput) -> T,
    ) -> (T, OutboxPoolOutput) {
        let mut output = OutboxPoolOutput::default();
        let result = f(pool, &mut output);
        (result, output)
    }

    fn relay_pkgs_from_sub(
        sub: &OutboxSubscription,
        relays: HashSet<NormRelayUrl>,
    ) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            relays,
            RelayUrlPolicy::new(
                sub.relay_url_source,
                sub.demand_priority,
                sub.routing_preference,
            )
            .with_connection_weight(sub.connection_weight),
        )
    }

    fn subscribe(
        pool: &mut OutboxPool,
        output: &mut OutboxPoolOutput,
        filters: Vec<Filter>,
        urls: RelayUrlPkgs,
    ) -> OutboxSubId {
        let id = pool.next_sub_id();
        output.extend(pool.set_live(id, filters, urls));
        id
    }

    fn subscribe_with_id(
        pool: &mut OutboxPool,
        output: &mut OutboxPoolOutput,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: RelayUrlPkgs,
    ) {
        output.extend(pool.set_live(id, filters, relays));
    }

    fn oneshot(
        pool: &mut OutboxPool,
        output: &mut OutboxPoolOutput,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: RelayUrlPkgs,
    ) {
        output.extend(pool.start_fetch(id, filters, relays));
    }

    fn unsubscribe(pool: &mut OutboxPool, output: &mut OutboxPoolOutput, id: OutboxSubId) {
        if pool.subs.get(&id).is_some_and(|sub| sub.is_oneshot) {
            output.extend(pool.clear_fetch(id));
        } else {
            output.extend(pool.clear_live(id));
        }
    }

    fn new_filters(
        pool: &mut OutboxPool,
        output: &mut OutboxPoolOutput,
        id: OutboxSubId,
        filters: Vec<Filter>,
    ) {
        let Some(sub) = pool.subs.get(&id) else {
            return;
        };
        let relays = relay_pkgs_from_sub(sub, sub.relays.clone());
        output.extend(pool.set_live(id, filters, relays));
    }

    fn new_relays(
        pool: &mut OutboxPool,
        output: &mut OutboxPoolOutput,
        id: OutboxSubId,
        relays: HashSet<NormRelayUrl>,
    ) {
        let Some(sub) = pool.subs.get(&id) else {
            return;
        };
        let filters = sub.filters.get_filters().clone();
        let relay_pkgs = relay_pkgs_from_sub(sub, relays);
        output.extend(pool.set_live(id, filters, relay_pkgs));
    }

    fn modify_full(
        pool: &mut OutboxPool,
        output: &mut OutboxPoolOutput,
        id: OutboxSubId,
        filters: Vec<Filter>,
        relays: HashSet<NormRelayUrl>,
    ) {
        let Some(sub) = pool.subs.get(&id) else {
            return;
        };
        let relay_pkgs = relay_pkgs_from_sub(sub, relays);
        output.extend(pool.set_live(id, filters, relay_pkgs));
    }

    fn collect_test_output(_pool: &mut OutboxPool, output: OutboxPoolOutput) -> OutboxPoolOutput {
        output
    }

    fn relay_route_type(
        pool: &OutboxPool,
        relay: &NormRelayUrl,
        id: OutboxSubId,
    ) -> Option<RelayType> {
        pool.relays.get(relay)?.route_type(&id)
    }

    fn relay_pkgs(
        urls: HashSet<NormRelayUrl>,
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> RelayUrlPkgs {
        relay_pkgs_with_weight(urls, demand_priority, routing_preference, 0)
    }

    fn relay_pkgs_with_weight(
        urls: HashSet<NormRelayUrl>,
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
        connection_weight: u32,
    ) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(demand_priority, routing_preference)
                .with_connection_weight(connection_weight),
        )
    }

    fn remote_relay_pkgs(
        urls: HashSet<NormRelayUrl>,
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> RelayUrlPkgs {
        RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::remote_advertised(demand_priority, routing_preference),
        )
    }

    fn filter_has_since(filter: &Filter) -> bool {
        filter.json().expect("filter json").contains("\"since\"")
    }

    fn insert_connected_test_coordinator(pool: &mut OutboxPool, relay: NormRelayUrl) {
        let mut coordinator = CoordinationData::new(RelayLimitations::default());
        let _ =
            coordinator.apply_websocket_opened(&OutboxSubscriptions::default(), Duration::ZERO, 0);
        pool.relays.insert(relay, coordinator);
    }

    fn req_status_fact(
        facts: &[OutboxPoolFact],
        id: OutboxSubId,
        relay: &NormRelayUrl,
    ) -> Option<Option<RelayReqStatus>> {
        facts.iter().find_map(|fact| match fact {
            OutboxPoolFact::RelayReqStatus {
                id: fact_id,
                relay: fact_relay,
                status,
            } if *fact_id == id && fact_relay == relay => Some(*status),
            _ => None,
        })
    }

    fn relay_eose_fact(
        facts: &[OutboxPoolFact],
        id: OutboxSubId,
    ) -> Option<Option<OutboxSubRelayEose>> {
        facts.iter().find_map(|fact| match fact {
            OutboxPoolFact::OutboxSubRelayEose {
                id: fact_id,
                relay_eose,
            } if *fact_id == id => Some(*relay_eose),
            _ => None,
        })
    }

    #[tokio::test]
    async fn deferred_relay_placement_emits_pending_aggregate_eose_fact() {
        let (_relay_task, relay, _captured, _notify) = create_req_capture_relay().await;
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let (id, output) = {
            let pkgs = relay_pkgs(
                HashSet::from([relay.clone()]),
                RelayDemandPriority::Opportunistic,
                RelayRoutingPreference::NoPreference,
            );
            apply_send_session_and_collect_output(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, trivial_filter(), pkgs)
            })
        };

        let relay_eose = relay_eose_fact(&output.facts, id)
            .expect("aggregate EOSE fact")
            .expect("retained sub aggregate");
        assert_eq!(
            relay_eose,
            OutboxSubRelayEose {
                tracked_relays: 1,
                unsupported_relays: 0,
                any_eose: false,
                all_eosed: false,
            }
        );
    }

    #[tokio::test]
    async fn unsubscribe_emits_req_status_and_aggregate_eose_cleanup_facts() {
        let (_relay_task, relay, _captured, _notify) = create_req_capture_relay().await;
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        insert_connected_test_coordinator(&mut pool, relay.clone());

        let (id, create_output) = {
            let pkgs = relay_pkgs(
                HashSet::from([relay.clone()]),
                RelayDemandPriority::Important,
                RelayRoutingPreference::RequireDedicated,
            );
            apply_send_session_and_collect_output(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(pool, session, trivial_filter(), pkgs)
            })
        };
        let create_events = create_output.facts;
        assert_eq!(
            req_status_fact(&create_events, id, &relay),
            Some(Some(RelayReqStatus::InitialQuery))
        );
        assert!(relay_eose_fact(&create_events, id)
            .expect("create aggregate EOSE fact")
            .is_some());

        let cleanup_output = {
            apply_send_session_and_collect_output(&mut pool, wakeup, |pool, session| {
                unsubscribe(pool, session, id);
            })
            .1
        };
        let cleanup_events = cleanup_output.facts;
        assert_eq!(req_status_fact(&cleanup_events, id, &relay), Some(None));
        assert_eq!(relay_eose_fact(&cleanup_events, id), Some(None));
    }

    /// Ensures cloned outbox id registries allocate from one namespace.
    #[tokio::test]
    async fn registry_generates_unique_ids() {
        let registry = OutboxIdRegistry::new();
        let other = registry.clone();

        let id1 = registry.next_sub_id();
        let id2 = other.next_sub_id();
        let id3 = registry.next_sub_id();

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    /// Existing relay coordinators with a missing websocket should be restored by ensure_relay.
    ///
    /// `ensure_relay` can build `WebsocketConn`; this needs a Tokio runtime
    /// when the ewebsock Tokio backend is used.
    #[tokio::test]
    async fn ensure_relay_creates_coordinator_without_transport_generation() {
        let mut pool = OutboxPool::default();
        let relay_id = NormRelayUrl::new("wss://relay-restore.example.com").unwrap();

        let relay = ensure_relay(&mut pool, &relay_id);
        assert!(relay.current_generation().is_none());
    }

    #[test]
    fn set_live_creates_disconnected_coordinator_without_transport_frame() {
        let relay =
            NormRelayUrl::new("wss://relay-atomic-demand.example.com").expect("valid relay");
        let id = OutboxSubId(42);
        let mut pool = OutboxPool::default();

        let output = pool.set_live(
            id,
            trivial_filter(),
            relay_pkgs(
                HashSet::from([relay.clone()]),
                RelayDemandPriority::Important,
                RelayRoutingPreference::NoPreference,
            ),
        );

        assert!(pool.subs.get(&id).is_some());
        assert_eq!(
            pool.relays
                .get(&relay)
                .expect("retained demand should create disconnected coordinator")
                .current_generation(),
            None
        );
        assert!(output.transport_effects.is_empty());
        assert!(output
            .relay_demand_changes
            .iter()
            .any(|change| { change.relay == relay && change.demand.is_some() }));
    }

    #[test]
    fn transport_open_replays_retained_live_demand() {
        let relay = NormRelayUrl::new("wss://relay-atomic-open.example.com").expect("valid relay");
        let id = OutboxSubId(43);
        let mut pool = OutboxPool::default();

        let demand_output = pool.set_live(
            id,
            trivial_filter(),
            relay_pkgs(
                HashSet::from([relay.clone()]),
                RelayDemandPriority::Important,
                RelayRoutingPreference::NoPreference,
            ),
        );
        assert!(demand_output.transport_effects.is_empty());
        assert_eq!(
            pool.relays
                .get(&relay)
                .expect("retained demand should create disconnected coordinator")
                .current_generation(),
            None
        );

        let generation = 7;
        let output = pool.apply_relay_transport_opened(relay.clone(), generation);

        assert_eq!(
            pool.relays
                .get(&relay)
                .expect("transport open should create coordinator")
                .current_generation(),
            Some(generation)
        );
        assert!(output.transport_effects.iter().any(|effect| matches!(
            effect,
            OutboxTransportEffect::SendRelayFrame {
                relay: effect_relay,
                generation: effect_generation,
                ..
            } if effect_relay == &relay && *effect_generation == generation
        )));
    }

    #[test]
    fn clear_live_emits_demand_without_admission_state() {
        let relay = NormRelayUrl::new("wss://relay-atomic-clear.example.com").expect("valid relay");
        let id = OutboxSubId(44);
        let mut pool = OutboxPool::default();

        let _ = pool.set_live(
            id,
            trivial_filter(),
            relay_pkgs(
                HashSet::from([relay.clone()]),
                RelayDemandPriority::Important,
                RelayRoutingPreference::NoPreference,
            ),
        );
        let output = pool.clear_live(id);

        assert!(pool.subs.get(&id).is_none());
        assert!(output
            .relay_demand_changes
            .iter()
            .any(|change| { change.relay == relay && change.demand.is_none() }));
    }

    #[test]
    fn remote_advertised_relay_policy_drops_blocked_urls_before_admission() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relays = [
            "wss://localhost",
            "wss://127.0.0.1",
            "wss://10.0.0.1",
            "wss://172.16.0.1",
            "wss://192.168.0.1",
            "wss://169.254.0.1",
            "wss://[::1]",
            "wss://[fc00::1]",
            "wss://[fe80::1]",
            "wss://printer.local",
            "wss://relay.onion",
            "wss://relay",
            "wss://bad_host.example.com",
        ]
        .into_iter()
        .filter_map(|url| NormRelayUrl::new(url).ok())
        .collect::<HashSet<_>>();

        let sub_id = {
            let pkgs = RelayUrlPkgs::new(
                relays.clone(),
                crate::relay::RelayUrlPolicy::remote_advertised(
                    RelayDemandPriority::Opportunistic,
                    RelayRoutingPreference::NoPreference,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, trivial_filter(), pkgs)
            })
        };

        for relay in &relays {
            assert!(
                !pool.relays.contains_key(relay),
                "{relay} should not reach relay admission"
            );
        }
        assert!(
            pool.subs.get(&sub_id).is_none(),
            "subscription should not be retained after all remote-advertised relays are blocked"
        );
    }

    #[tokio::test]
    async fn full_history_targets_fully_match_is_policy_sensitive() {
        let mut service = service();
        let relay = NormRelayUrl::new("wss://relay-full-history-match.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(10).build();
        let target = FullHistoryTarget::new(
            vec![filter.clone()],
            vec![remote_relay_pkgs(
                HashSet::from([relay.clone()]),
                RelayDemandPriority::Opportunistic,
                RelayRoutingPreference::NoPreference,
            )],
        );

        let history_id = service.id_registry().next_full_history_id();
        let _ = service.set_full_history_targets(history_id, vec![target.clone()]);

        assert!(service
            .full_history
            .full_history_targets_fully_match(history_id, vec![target]));
        assert!(!service.full_history.full_history_targets_fully_match(
            history_id,
            vec![FullHistoryTarget::new(
                vec![filter],
                vec![remote_relay_pkgs(
                    HashSet::from([relay]),
                    RelayDemandPriority::Important,
                    RelayRoutingPreference::NoPreference,
                )],
            )],
        ));
    }

    #[tokio::test]
    async fn reapplying_same_full_history_targets_preserves_stored_targets() {
        let mut service = service();
        let relay = NormRelayUrl::new("wss://relay-full-history-handler-noop.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(10).build();
        let target = FullHistoryTarget::new(
            vec![filter],
            vec![remote_relay_pkgs(
                HashSet::from([relay]),
                RelayDemandPriority::Opportunistic,
                RelayRoutingPreference::NoPreference,
            )],
        );

        let history_id = service.id_registry().next_full_history_id();
        let _ = service.set_full_history_targets(history_id, vec![target.clone()]);

        let _ = service.set_full_history_targets(history_id, vec![target.clone()]);
        assert!(service
            .full_history
            .full_history_targets_fully_match(history_id, vec![target]));
    }

    /// EOSE from relays not currently routed for a subscription should be ignored.
    #[tokio::test]
    async fn eose_tracker_ignores_non_routed_relays() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-routed.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-stale.example.com").unwrap();
        let id = OutboxSubId(1);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.mark_relay_eose(&relay_b, id, &subs);
        assert!(
            !tracker.has_any_eose(&subs, &id),
            "stale relay should not mark any EOSE progress"
        );
        assert!(
            !tracker.is_fully_eosed(&subs, &id),
            "stale relay should not mark sub fully EOSE"
        );

        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(tracker.has_any_eose(&subs, &id));
        assert!(tracker.is_fully_eosed(&subs, &id));
    }

    /// Marking one routed relay pending should invalidate cached fully-EOSE completion.
    #[tokio::test]
    async fn eose_tracker_pending_relay_leg_invalidates_cached_completion() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-clear-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-clear-b.example.com").unwrap();
        let id = OutboxSubId(2);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());
        relays.insert(relay_b.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.set_relay_leg_readiness(relay_b.clone(), id, RelayLegReadiness::PendingPlacement);
        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(!tracker.is_fully_eosed(&subs, &id));

        tracker.mark_relay_eose(&relay_b, id, &subs);
        assert!(tracker.is_fully_eosed(&subs, &id));

        tracker.set_relay_leg_readiness(relay_a.clone(), id, RelayLegReadiness::PendingPlacement);
        assert!(
            !tracker.is_fully_eosed(&subs, &id),
            "clearing one routed relay must drop cached completion"
        );
    }

    #[tokio::test]
    async fn eose_tracker_reports_only_new_full_eose_edges() {
        let relay = NormRelayUrl::new("wss://relay-eose-edge.example.com").unwrap();
        let id = OutboxSubId(25);
        let mut relays = HashSet::new();
        relays.insert(relay.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        assert!(tracker.mark_relay_eose(&relay, id, &subs));

        assert!(
            !tracker.mark_relay_eose(&relay, id, &subs),
            "duplicate EOSE must not emit a second full-EOSE edge"
        );

        assert!(
            !tracker.set_relay_leg_readiness(
                relay.clone(),
                id,
                RelayLegReadiness::PendingPlacement
            ),
            "invalidation clears completion without emitting an edge"
        );

        assert!(
            tracker.mark_relay_eose(&relay, id, &subs),
            "a fresh EOSE after invalidation is a new full-EOSE edge"
        );
    }

    /// Shrinking a subscription's relay set can complete it immediately if all
    /// remaining relays had already reached EOSE.
    #[tokio::test]
    async fn eose_tracker_reconciles_completion_when_relay_set_shrinks() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-shrink-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-shrink-b.example.com").unwrap();
        let id = OutboxSubId(22);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());
        relays.insert(relay_b.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.set_relay_leg_readiness(relay_b.clone(), id, RelayLegReadiness::PendingPlacement);
        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(!tracker.is_fully_eosed(&subs, &id));

        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());
        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            }),
        ));
        tracker.remove_relay_leg(&relay_b, id);

        assert!(tracker.is_fully_eosed(&subs, &id));
    }

    /// Expanding a subscription's relay set should drop full completion until
    /// the newly added relay also reaches EOSE.
    #[tokio::test]
    async fn eose_tracker_reconciles_incomplete_when_relay_set_expands() {
        let relay_a = NormRelayUrl::new("wss://relay-eose-expand-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-eose-expand-b.example.com").unwrap();
        let id = OutboxSubId(23);
        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());

        let mut subs = OutboxSubscriptions::default();
        subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );
        let mut tracker = EoseTracker::default();

        tracker.mark_relay_eose(&relay_a, id, &subs);
        assert!(tracker.is_fully_eosed(&subs, &id));

        let mut relays = HashSet::new();
        relays.insert(relay_a.clone());
        relays.insert(relay_b.clone());
        assert!(subs.ingest_task(
            &id,
            ModifyTask::FullRelayPkgs(FullRelayPkgsModificationTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            }),
        ));
        tracker.set_relay_leg_readiness(relay_b.clone(), id, RelayLegReadiness::PendingPlacement);

        assert!(
            !tracker.is_fully_eosed(&subs, &id),
            "adding a new relay must invalidate full completion until it EOSEs"
        );
        tracker.mark_relay_eose(&relay_b, id, &subs);
        assert!(tracker.is_fully_eosed(&subs, &id));
    }

    /// Coordinator-reported relay-leg invalidations must clear stale durable
    /// EOSE completion before any fresh REQ on that relay can complete again.
    #[tokio::test]
    async fn apply_relay_eose_delta_clears_invalidated_sub_ids() {
        let relay = NormRelayUrl::new("wss://relay-eose-delta-clear.example.com").unwrap();
        let id = OutboxSubId(3);
        let mut relays = HashSet::new();
        relays.insert(relay.clone());

        let mut pool = OutboxPool::default();
        pool.subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            false,
        );

        pool.eose_tracker.mark_relay_eose(&relay, id, &pool.subs);
        assert!(pool.all_have_eose(&id));

        let delta = RelayEoseDelta {
            sub_ids: HashSet::new(),
            invalidated_sub_ids: HashSet::from([id]),
        };
        pool.apply_relay_eose_delta(&relay, delta);

        assert!(
            !pool.all_have_eose(&id),
            "a fresh internally issued REQ must clear prior durable EOSE state"
        );
    }

    /// Receive-driven EOSE processing must apply ready fully-EOSE effects in the
    /// same transition, including oneshot cleanup.
    #[tokio::test]
    async fn apply_relay_eose_delta_applies_full_eose_effects() {
        let relay = NormRelayUrl::new("wss://relay-eose-pending-effects.example.com").unwrap();
        let id = OutboxSubId(24);
        let mut relays = HashSet::new();
        relays.insert(relay.clone());

        let mut pool = OutboxPool::default();
        pool.subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: RelayUrlPkgs::new(
                    relays,
                    crate::relay::RelayUrlPolicy::explicit(
                        crate::relay::RelayDemandPriority::Important,
                        crate::relay::RelayRoutingPreference::PreferDedicated,
                    ),
                ),
            },
            true,
        );

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        assert!(
            pool.subs.get(&id).is_none(),
            "oneshot should be removed as soon as receive-path EOSE processing completes"
        );
    }

    /// Local websocket eviction applies coordinator cleanup on the caller stack.
    #[tokio::test]
    async fn evict_relay_connection_applies_pending_effects() {
        let relay = NormRelayUrl::new("wss://relay-evict-defers-effects.example.com").unwrap();
        let mut pool = OutboxPool::default();
        let mut coordinator = CoordinationData::new(RelayLimitations::default());
        let _ =
            coordinator.apply_websocket_opened(&OutboxSubscriptions::default(), Duration::ZERO, 0);
        pool.relays.insert(relay.clone(), coordinator);

        let _ = pool.evict_relay_connection_for_reason(
            &relay,
            RelayConnectionDropReason::IdleAfterUnsubscribe,
        );
    }

    /// Unsaturated relays should place preferred dedicated requests on a
    /// dedicated leg rather than falling through to compaction.
    #[tokio::test]
    async fn prefer_dedicated_request_uses_dedicated_when_unsaturated() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay =
            NormRelayUrl::new("wss://relay-prefer-dedicated-unsaturated.example.com").unwrap();

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, trivial_filter(), pkgs)
            })
        };

        let _ = open_relay_transport_for_test(&mut pool, &relay);
        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
    }

    /// Unsaturated relays should also place no-preference requests on a
    /// dedicated leg before considering compaction fallback.
    #[tokio::test]
    async fn no_preference_request_uses_dedicated_when_unsaturated() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-no-preference-unsaturated.example.com").unwrap();

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::NoPreference,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, trivial_filter(), pkgs)
            })
        };

        let _ = open_relay_transport_for_test(&mut pool, &relay);
        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));
    }

    /// Fully EOSE'd dedicated routes should keep their original filters instead
    /// of advancing `since`, which is only safe for compaction.
    #[tokio::test]
    async fn fully_eosed_dedicated_route_does_not_optimize_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-since-dedicated.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, vec![filter], pkgs)
            })
        };

        let _ = open_relay_transport_for_test(&mut pool, &relay);
        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Transparent));

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        let filter = &pool
            .subs
            .stored_ref(&id)
            .expect("subscription")
            .filters
            .get_filters()[0];
        assert!(
            !filter_has_since(filter),
            "dedicated routes must not rewrite filters with a synthetic since cursor"
        );
    }

    /// Fully EOSE'd compaction routes should keep the base filters pristine
    /// while their compaction projection advances `since` for future shared
    /// REQs.
    #[tokio::test]
    async fn fully_eosed_compaction_route_optimizes_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-since-compaction.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let _ = ensure_relay(&mut pool, &relay);
        {
            let (subs, relays) = (&pool.subs, &mut pool.relays);
            relays.get_mut(&relay).expect("coordinator").set_limits(
                subs,
                0,
                RelayLimitations {
                    maximum_subs: 0,
                    max_json_bytes: RelayLimitations::default().max_json_bytes,
                },
            );
        }

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, vec![filter], pkgs)
            })
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.route_type(&id), Some(RelayType::Compaction));

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        let stored_filter = &pool
            .subs
            .stored_ref(&id)
            .expect("subscription")
            .filters
            .get_filters()[0];
        assert!(
            !filter_has_since(stored_filter),
            "stored filters should remain pristine after compaction catches up"
        );

        let projected = pool
            .compaction_filters(&id)
            .expect("compaction-projected filters");
        assert!(
            filter_has_since(&projected[0]),
            "compaction projection should advance since after fully catching up"
        );
    }

    #[tokio::test]
    async fn duplicate_relay_eose_does_not_advance_compaction_since_again() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-duplicate-eose-since.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let _ = ensure_relay(&mut pool, &relay);
        {
            let (subs, relays) = (&pool.subs, &mut pool.relays);
            relays.get_mut(&relay).expect("coordinator").set_limits(
                subs,
                0,
                RelayLimitations {
                    maximum_subs: 0,
                    max_json_bytes: RelayLimitations::default().max_json_bytes,
                },
            );
        }

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, vec![filter], pkgs)
            })
        };

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );
        assert!(
            pool.compaction_filters(&id).unwrap()[0].since().is_some(),
            "first full EOSE should set the compaction projection cursor"
        );

        let sentinel_since = 1;
        assert!(pool.subs.see_all(&id, sentinel_since));
        assert_eq!(
            pool.compaction_filters(&id).unwrap()[0].since(),
            Some(sentinel_since)
        );

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        assert_eq!(
            pool.compaction_filters(&id).unwrap()[0].since(),
            Some(sentinel_since),
            "duplicate EOSE for the same relay-query epoch must not advance the compaction cursor"
        );
    }

    /// Mixed routing should refuse `since` optimization until every relay leg
    /// for the subscription is owned by compaction.
    #[tokio::test]
    async fn fully_eosed_mixed_routes_do_not_optimize_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_dedicated =
            NormRelayUrl::new("wss://relay-since-mixed-dedicated.example.com").unwrap();
        let relay_compaction =
            NormRelayUrl::new("wss://relay-since-mixed-compaction.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(2).build();

        let _ = ensure_relay(&mut pool, &relay_compaction);
        {
            let (subs, relays) = (&pool.subs, &mut pool.relays);
            relays
                .get_mut(&relay_compaction)
                .expect("coordinator")
                .set_limits(
                    subs,
                    0,
                    RelayLimitations {
                        maximum_subs: 0,
                        max_json_bytes: RelayLimitations::default().max_json_bytes,
                    },
                );
        }

        let id = {
            let mut relays = HashSet::new();
            relays.insert(relay_dedicated.clone());
            relays.insert(relay_compaction.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, vec![filter], pkgs)
            })
        };

        let _ = open_relay_transport_for_test(&mut pool, &relay_dedicated);
        let dedicated = pool.relays.get(&relay_dedicated).expect("dedicated relay");
        assert_eq!(dedicated.route_type(&id), Some(RelayType::Transparent));
        let compaction = pool
            .relays
            .get(&relay_compaction)
            .expect("compaction relay");
        assert_eq!(compaction.route_type(&id), Some(RelayType::Compaction));

        pool.apply_relay_eose_delta(
            &relay_dedicated,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );
        pool.apply_relay_eose_delta(
            &relay_compaction,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        let filter = &pool
            .subs
            .stored_ref(&id)
            .expect("subscription")
            .filters
            .get_filters()[0];
        assert!(
            !filter_has_since(filter),
            "a mixed dedicated/compaction subscription must not advance since"
        );
    }

    /// A request that caught up through compaction should still use pristine
    /// stored filters if it is later promoted back to dedicated routing.
    #[tokio::test]
    async fn promoted_dedicated_route_does_not_keep_compaction_since() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-promoted-since.example.com").unwrap();
        let required_filter = Filter::new().kinds(vec![1]).limit(2).build();
        let preferred_filter = Filter::new().kinds(vec![2]).limit(2).build();

        let _ = ensure_relay(&mut pool, &relay);
        {
            let (subs, relays) = (&pool.subs, &mut pool.relays);
            relays.get_mut(&relay).expect("coordinator").set_limits(
                subs,
                0,
                RelayLimitations {
                    maximum_subs: 1,
                    max_json_bytes: RelayLimitations::default().max_json_bytes,
                },
            );
        }

        let required_id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::RequireDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(pool, session, vec![required_filter], pkgs)
            })
        };

        let preferred_id = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, vec![preferred_filter], pkgs)
            })
        };

        {
            let coordinator = pool.relays.get(&relay).expect("coordinator");
            assert_eq!(
                coordinator.route_type(&required_id),
                Some(RelayType::Transparent)
            );
            assert_eq!(
                coordinator.route_type(&preferred_id),
                Some(RelayType::Compaction)
            );
        }

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([preferred_id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        let stored_before = pool.filters(&preferred_id).expect("stored filters");
        assert!(
            !filter_has_since(&stored_before[0]),
            "stored filters should remain pristine after compaction catch-up"
        );
        let projected_before = pool
            .compaction_filters(&preferred_id)
            .expect("compaction-projected filters");
        assert!(
            filter_has_since(&projected_before[0]),
            "compaction projection should reflect the stored catch-up cursor"
        );

        {
            apply_send_session_with(&mut pool, MockWakeup::default(), |pool, session| {
                unsubscribe(pool, session, required_id);
            })
        }

        {
            let coordinator = pool.relays.get(&relay).expect("coordinator");
            assert_eq!(
                coordinator.route_type(&preferred_id),
                Some(RelayType::Transparent)
            );
        }

        let stored_after = pool
            .filters(&preferred_id)
            .expect("stored filters after promotion");
        assert!(
            !filter_has_since(&stored_after[0]),
            "promoted dedicated route must use pristine stored filters"
        );
    }

    /// Ensures applying relay limits reports all coordinator outcomes.
    #[tokio::test]
    async fn apply_relay_limitations_reports_outcomes_and_updates_state() {
        let mut pool = OutboxPool::default();
        let known = NormRelayUrl::new("wss://relay-nip11-known.example.com").unwrap();
        let unknown = NormRelayUrl::new("wss://relay-nip11-unknown.example.com").unwrap();
        let _ = ensure_relay(&mut pool, &known);

        let unknown_outcome =
            apply_relay_limit_update_for_test(&mut pool, &unknown, RelayLimitations::default());
        assert_eq!(unknown_outcome, Nip11ApplyOutcome::RelayUnknown);

        let unchanged_outcome =
            apply_relay_limit_update_for_test(&mut pool, &known, RelayLimitations::default());
        assert_eq!(unchanged_outcome, Nip11ApplyOutcome::Unchanged);

        let applied_relay = NormRelayUrl::new("wss://relay-nip11-applied.example.com").unwrap();
        let _ = ensure_relay(&mut pool, &applied_relay);
        let applied_limits = RelayLimitations {
            maximum_subs: 777,
            ..Default::default()
        };
        let applied_outcome =
            apply_relay_limit_update_for_test(&mut pool, &applied_relay, applied_limits);
        assert_eq!(applied_outcome, Nip11ApplyOutcome::Applied);

        let limits = pool
            .relays
            .get(&applied_relay)
            .expect("relay present")
            .current_limits();
        assert_eq!(limits.maximum_subs, 777);
    }

    /// Unchanged NIP-11 data must not synthesize relay effects.
    #[tokio::test]
    async fn unchanged_nip11_limits_do_not_recreate_completed_oneshot() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-nip11-unchanged-effects.example.com").unwrap();
        let id = OutboxSubId(42);
        let _ = ensure_relay(&mut pool, &relay);

        pool.subs.new_subscription(
            id,
            SubscribeTask {
                filters: trivial_filter(),
                relays: relay_pkgs(
                    HashSet::from([relay.clone()]),
                    RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            },
            true,
        );
        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([id]),
                invalidated_sub_ids: HashSet::new(),
            },
        );
        assert!(
            pool.subs.get(&id).is_none(),
            "EOSE completion should remove oneshot before NIP-11 refresh"
        );

        let outcome =
            apply_relay_limit_update_for_test(&mut pool, &relay, RelayLimitations::default());

        assert_eq!(outcome, Nip11ApplyOutcome::Unchanged);
        assert!(
            pool.subs.get(&id).is_none(),
            "unchanged NIP-11 refresh must not recreate completed oneshot"
        );
    }

    // ==================== OutboxPool tests ====================

    /// Default pool has no relays or subscriptions.
    #[tokio::test]
    async fn outbox_pool_default_empty() {
        let pool = OutboxPool::default();
        assert!(pool.relays.is_empty());
        // Verify no subscriptions by checking that a lookup returns empty status
        assert!(pool.status(&OutboxSubId(0)).is_empty());
    }

    /// has_observed_eose returns false when no relays are tracking the request.
    #[tokio::test]
    async fn outbox_pool_has_observed_eose_false_when_empty() {
        let pool = OutboxPool::default();
        assert!(!pool.has_observed_eose(&OutboxSubId(0)));
    }

    /// status() returns empty map for unknown request IDs.
    #[tokio::test]
    async fn outbox_pool_status_empty_for_unknown() {
        let pool = OutboxPool::default();
        let status = pool.status(&OutboxSubId(999));
        assert!(status.is_empty());
    }

    /// Full modifications should unsubscribe old relays and resubscribe new ones using the updated filters.
    #[tokio::test]
    async fn full_modification_updates_sessions_with_new_filters() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-b.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay_a.clone());
        let new_sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    RelayUrlPkgs::new(
                        urls,
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                )
            })
        };

        {
            let sub = pool
                .subs
                .get(&new_sub_id)
                .expect("subscription should be registered");
            assert_eq!(sub.relays.len(), 1);
            assert!(sub.relays.contains(&relay_a));
            assert!(!sub.is_oneshot);
            assert_eq!(
                sub.routing_preference,
                RelayRoutingPreference::PreferDedicated
            );
        }

        let sessions = {
            let mut updated_relays = HashSet::new();
            updated_relays.insert(relay_b.clone());

            let mut session = OutboxPoolOutput::default();
            new_filters(
                &mut pool,
                &mut session,
                new_sub_id,
                vec![Filter::new().kinds(vec![3]).limit(1).build()],
            );
            new_relays(&mut pool, &mut session, new_sub_id, updated_relays);
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay_a));
        assert!(output_touches_relay(&sessions, &relay_b));
        assert_eq!(relay_route_type(&pool, &relay_a, new_sub_id), None);
        assert_eq!(
            relay_route_type(&pool, &relay_b, new_sub_id),
            Some(RelayType::Transparent)
        );
    }

    /// Oneshot requests use the default prefer-dedicated routing policy.
    #[tokio::test]
    async fn oneshot_routes_to_prefer_dedicated() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-oneshot.example.com").unwrap();
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let filters = vec![Filter::new().kinds(vec![1]).limit(2).build()];
        let id = OutboxSubId(42);

        let mut session = OutboxPoolOutput::default();
        oneshot(
            &mut pool,
            &mut session,
            id,
            filters.clone(),
            RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    crate::relay::RelayRoutingPreference::PreferDedicated,
                ),
            ),
        );

        let sessions = collect_test_output(&mut pool, session);

        assert!(output_touches_relay(&sessions, &relay));
        assert_eq!(
            relay_route_type(&pool, &relay, id),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            pool.subs.get(&id).map(|sub| sub.routing_preference),
            Some(RelayRoutingPreference::PreferDedicated)
        );
    }

    #[tokio::test]
    async fn start_fetch_retains_duplicate_concrete_requests_in_same_batch() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-oneshot.example.com").unwrap();
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let relays = relay_pkgs(
            relays,
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        );
        let id = [1; 32];
        let filters = vec![Filter::new().ids([&id]).build()];

        let mut session = OutboxPoolOutput::default();
        oneshot(
            &mut pool,
            &mut session,
            OutboxSubId(42),
            filters.clone(),
            relays.clone(),
        );
        oneshot(
            &mut pool,
            &mut session,
            OutboxSubId(43),
            filters.clone(),
            relays,
        );

        let output = collect_test_output(&mut pool, session);
        assert!(output_touches_relay(&output, &relay));
        let retained = [OutboxSubId(42), OutboxSubId(43)]
            .into_iter()
            .filter(|id| pool.subs.get(id).is_some())
            .collect::<Vec<_>>();
        assert_eq!(retained, vec![OutboxSubId(42), OutboxSubId(43)]);
    }

    #[tokio::test]
    async fn start_fetch_retains_duplicate_concrete_request_while_active_fetch_exists() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-oneshot.example.com").unwrap();
        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let relays = relay_pkgs(
            relays,
            RelayDemandPriority::Important,
            RelayRoutingPreference::PreferDedicated,
        );
        let id = [1; 32];
        let filters = vec![Filter::new().ids([&id]).build()];

        let mut initial_session = OutboxPoolOutput::default();
        oneshot(
            &mut pool,
            &mut initial_session,
            OutboxSubId(42),
            filters.clone(),
            relays.clone(),
        );
        let initial_output = collect_test_output(&mut pool, initial_session);
        assert!(output_touches_relay(&initial_output, &relay));

        let mut duplicate_session = OutboxPoolOutput::default();
        oneshot(
            &mut pool,
            &mut duplicate_session,
            OutboxSubId(43),
            filters,
            relays,
        );
        let duplicate_output = collect_test_output(&mut pool, duplicate_session);
        assert!(output_touches_relay(&duplicate_output, &relay));
        assert!(pool.subs.get(&OutboxSubId(42)).is_some());
        assert!(pool.subs.get(&OutboxSubId(43)).is_some());
    }

    /// Unsubscribing from a multi-relay subscription emits unsubscribe tasks for each relay.
    #[tokio::test]
    async fn unsubscribe_targets_all_relays() {
        let mut pool = OutboxPool::default();
        let relay_a = NormRelayUrl::new("wss://relay-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-b.example.com").unwrap();
        let id = OutboxSubId(42);

        // Subscribe to both relays
        let mut urls = HashSet::new();
        urls.insert(relay_a.clone());
        urls.insert(relay_b.clone());

        let mut session = OutboxPoolOutput::default();
        subscribe_with_id(
            &mut pool,
            &mut session,
            id,
            trivial_filter(),
            RelayUrlPkgs::new(
                urls,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    crate::relay::RelayRoutingPreference::PreferDedicated,
                ),
            ),
        );
        collect_test_output(&mut pool, session);

        // Unsubscribe
        let mut session = OutboxPoolOutput::default();
        unsubscribe(&mut pool, &mut session, id);
        let sessions = collect_test_output(&mut pool, session);

        assert!(output_touches_relay(&sessions, &relay_a));
        assert!(output_touches_relay(&sessions, &relay_b));
        assert_eq!(relay_route_type(&pool, &relay_a, id), None);
        assert_eq!(relay_route_type(&pool, &relay_b, id), None);
    }

    /// Subscriptions with `PreferDedicated` policy route to dedicated-preferred mode.
    #[tokio::test]
    async fn subscribe_dedicated_preferred_mode() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-transparent.example.com").unwrap();
        let id = OutboxSubId(5);

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let pkgs = RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                RelayRoutingPreference::PreferDedicated,
            ),
        );

        let mut session = OutboxPoolOutput::default();
        subscribe_with_id(&mut pool, &mut session, id, trivial_filter(), pkgs);
        let sessions = collect_test_output(&mut pool, session);

        assert!(output_touches_relay(&sessions, &relay));
        assert_eq!(
            relay_route_type(&pool, &relay, id),
            Some(RelayType::Transparent)
        );
    }

    /// Modifying filters should re-subscribe the routed relays with the new filters.
    #[tokio::test]
    async fn modify_filters_reissues_subscribe_for_existing_relays() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    RelayUrlPkgs::new(
                        urls,
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                )
            })
        };

        let (sessions, expected_json) = {
            let mut session = OutboxPoolOutput::default();
            let updated_filters = vec![Filter::new().kinds(vec![7]).limit(2).build()];
            let expected_json = filters_json(&updated_filters);
            new_filters(&mut pool, &mut session, sub_id, updated_filters);
            (collect_test_output(&mut pool, session), expected_json)
        };

        let view = pool
            .subs
            .stored_ref(&sub_id)
            .expect("updated subscription ref");
        let stored_json = filters_json(view.filters.get_filters());
        assert_eq!(stored_json, expected_json);

        assert!(output_touches_relay(&sessions, &relay));
    }

    #[tokio::test]
    async fn modify_filters_same_canonical_filters_stages_no_relay_work() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify-noop.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(10).build();

        let sub_id = {
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![filter.clone()],
                    relay_pkgs(
                        HashSet::from([relay.clone()]),
                        RelayDemandPriority::Important,
                        RelayRoutingPreference::PreferDedicated,
                    ),
                )
            })
        };

        let mut session = OutboxPoolOutput::default();
        new_filters(&mut pool, &mut session, sub_id, vec![filter]);
        let sessions = collect_test_output(&mut pool, session);

        assert!(pool_output_is_empty(&sessions));
    }

    #[tokio::test]
    async fn modify_relays_same_relay_set_stages_no_relay_work() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-relays-noop.example.com").unwrap();
        let relays = HashSet::from([relay.clone()]);

        let sub_id = {
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    relay_pkgs(
                        relays.clone(),
                        RelayDemandPriority::Important,
                        RelayRoutingPreference::PreferDedicated,
                    ),
                )
            })
        };

        let mut session = OutboxPoolOutput::default();
        new_relays(&mut pool, &mut session, sub_id, relays);
        let sessions = collect_test_output(&mut pool, session);

        assert!(pool_output_is_empty(&sessions));
    }

    #[tokio::test]
    async fn reapplying_same_live_filters_and_relays_preserves_subscription() {
        let mut pool = OutboxPool::default();
        let relay = NormRelayUrl::new("wss://relay-live-handler-noop.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(10).build();
        let relays = HashSet::from([relay]);

        let (sub_id, _) = {
            apply_send_session_and_collect_output(
                &mut pool,
                MockWakeup::default(),
                |pool, session| {
                    subscribe(
                        pool,
                        session,
                        vec![filter.clone()],
                        relay_pkgs(
                            relays.clone(),
                            RelayDemandPriority::Important,
                            RelayRoutingPreference::PreferDedicated,
                        ),
                    )
                },
            )
        };

        let (_, update_output) = apply_send_session_and_collect_output(
            &mut pool,
            MockWakeup::default(),
            |pool, session| {
                new_filters(pool, session, sub_id, vec![filter.clone()]);
                new_relays(pool, session, sub_id, relays.clone());
            },
        );

        assert!(
            update_output.facts.is_empty(),
            "unchanged live declaration should not emit facts"
        );
        assert_eq!(
            filters_json(pool.filters(&sub_id).expect("subscription filters")),
            filters_json(std::slice::from_ref(&filter))
        );
        assert_eq!(pool.relays(&sub_id).expect("subscription relays"), &relays);
    }

    /// Modifying filters should preserve the default dedicated retry policy.
    #[tokio::test]
    async fn modify_filters_preserves_default_dedicated_retry_policy() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify-default-retry.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    RelayUrlPkgs::new(
                        urls,
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                )
            })
        };

        let sessions = {
            let mut session = OutboxPoolOutput::default();
            new_filters(
                &mut pool,
                &mut session,
                sub_id,
                vec![Filter::new().kinds(vec![1]).limit(7).build()],
            );
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay));
    }

    /// Modifying filters should preserve the prefer-dedicated retry policy.
    #[tokio::test]
    async fn modify_filters_preserves_preferred_dedicated_retry_policy() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-modify-preferred-retry.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let pkgs = RelayUrlPkgs::new(
            urls,
            crate::relay::RelayUrlPolicy::explicit(
                crate::relay::RelayDemandPriority::Important,
                RelayRoutingPreference::PreferDedicated,
            ),
        );
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![Filter::new().kinds(vec![1]).limit(1).build()],
                    pkgs,
                )
            })
        };

        let sessions = {
            let mut session = OutboxPoolOutput::default();
            new_filters(
                &mut pool,
                &mut session,
                sub_id,
                vec![Filter::new().kinds(vec![1]).limit(9).build()],
            );
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay));
    }

    /// Modifying relays should unsubscribe removed relays and subscribe new ones.
    #[tokio::test]
    async fn modify_relays_differs_routing_sets() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-diff-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-diff-b.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay_a.clone());
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    RelayUrlPkgs::new(
                        urls,
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                )
            })
        };

        let sessions = {
            let mut session = OutboxPoolOutput::default();
            let mut new_urls = HashSet::new();
            new_urls.insert(relay_b.clone());
            new_relays(&mut pool, &mut session, sub_id, new_urls);
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay_a));
        assert!(output_touches_relay(&sessions, &relay_b));
        assert_eq!(relay_route_type(&pool, &relay_a, sub_id), None);
        assert_eq!(
            relay_route_type(&pool, &relay_b, sub_id),
            Some(RelayType::Transparent)
        );
    }

    /// A full modification that only adds a relay should not reissue retained relay legs.
    #[tokio::test]
    async fn modify_full_relay_only_update_subscribes_only_added_relays() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-full-removed.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-full-retained.example.com").unwrap();
        let relay_c = NormRelayUrl::new("wss://relay-full-added.example.com").unwrap();

        let filter = Filter::new().kinds(vec![1]).limit(10).build();
        let mut initial_relays = HashSet::new();
        initial_relays.insert(relay_a.clone());
        initial_relays.insert(relay_b.clone());
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![filter.clone()],
                    relay_pkgs(
                        initial_relays.clone(),
                        RelayDemandPriority::Important,
                        RelayRoutingPreference::PreferDedicated,
                    ),
                )
            })
        };

        let sessions = {
            let mut session = OutboxPoolOutput::default();
            let mut updated_relays = HashSet::new();
            updated_relays.insert(relay_b.clone());
            updated_relays.insert(relay_c.clone());
            modify_full(
                &mut pool,
                &mut session,
                sub_id,
                vec![filter],
                updated_relays,
            );
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay_a));
        assert!(!output_touches_relay(&sessions, &relay_b));
        assert!(output_touches_relay(&sessions, &relay_c));
        assert_eq!(relay_route_type(&pool, &relay_a, sub_id), None);
        assert_eq!(
            relay_route_type(&pool, &relay_b, sub_id),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            relay_route_type(&pool, &relay_c, sub_id),
            Some(RelayType::Transparent)
        );
    }

    /// A full modification that changes filters should reissue retained relay legs.
    #[tokio::test]
    async fn modify_full_filter_update_reissues_retained_relays() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-full-filter-retained.example.com").unwrap();

        let mut relays = HashSet::new();
        relays.insert(relay.clone());
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![Filter::new().kinds(vec![1]).limit(10).build()],
                    relay_pkgs(
                        relays.clone(),
                        RelayDemandPriority::Important,
                        RelayRoutingPreference::PreferDedicated,
                    ),
                )
            })
        };

        let sessions = {
            let mut session = OutboxPoolOutput::default();
            modify_full(
                &mut pool,
                &mut session,
                sub_id,
                vec![Filter::new().kinds(vec![7]).limit(10).build()],
                relays,
            );
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay));
    }

    #[tokio::test]
    async fn modify_full_same_filters_and_relays_stages_no_relay_work() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-full-noop.example.com").unwrap();
        let filter = Filter::new().kinds(vec![1]).limit(10).build();
        let relays = HashSet::from([relay.clone()]);

        let sub_id = {
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![filter.clone()],
                    relay_pkgs(
                        relays.clone(),
                        RelayDemandPriority::Important,
                        RelayRoutingPreference::PreferDedicated,
                    ),
                )
            })
        };

        let mut session = OutboxPoolOutput::default();
        modify_full(&mut pool, &mut session, sub_id, vec![filter], relays);
        let sessions = collect_test_output(&mut pool, session);

        assert!(pool_output_is_empty(&sessions));
    }

    /// Full modifications that end up with no relays should drop the subscription entirely.
    #[tokio::test]
    async fn modify_full_with_empty_relays_removes_subscription() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-empty.example.com").unwrap();

        let mut urls = HashSet::new();
        urls.insert(relay.clone());
        let sub_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    RelayUrlPkgs::new(
                        urls,
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                )
            })
        };

        let sessions = {
            let mut session = OutboxPoolOutput::default();
            modify_full(
                &mut pool,
                &mut session,
                sub_id,
                vec![Filter::new().kinds(vec![9]).limit(1).build()],
                HashSet::new(),
            );
            collect_test_output(&mut pool, session)
        };

        assert!(output_touches_relay(&sessions, &relay));
        assert!(
            pool.subs.get(&sub_id).is_none(),
            "subscription metadata should be removed"
        );
    }

    /// High churn of modify/unsubscribe operations should keep active and inactive
    /// subscription state consistent without leaking relay status entries.
    #[tokio::test]
    async fn high_churn_modify_and_unsubscribe_keeps_consistent_state() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay_a = NormRelayUrl::new("wss://relay-churn-a.example.com").unwrap();
        let relay_b = NormRelayUrl::new("wss://relay-churn-b.example.com").unwrap();
        let relay_c = NormRelayUrl::new("wss://relay-churn-c.example.com").unwrap();

        let mut relays_ab = HashSet::new();
        relays_ab.insert(relay_a.clone());
        relays_ab.insert(relay_b.clone());

        let mut relays_bc = HashSet::new();
        relays_bc.insert(relay_b.clone());
        relays_bc.insert(relay_c.clone());

        let mut active_relays = relays_ab.clone();
        let mut active_id = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    trivial_filter(),
                    RelayUrlPkgs::new(
                        active_relays.clone(),
                        crate::relay::RelayUrlPolicy::explicit(
                            crate::relay::RelayDemandPriority::Important,
                            crate::relay::RelayRoutingPreference::PreferDedicated,
                        ),
                    ),
                )
            })
        };
        let _ = open_relay_transport_for_test(&mut pool, &relay_a);
        let _ = open_relay_transport_for_test(&mut pool, &relay_b);
        let _ = open_relay_transport_for_test(&mut pool, &relay_c);

        let mut inactive_ids = Vec::new();
        for i in 0..200usize {
            if i % 11 == 10 {
                let old_id = active_id;
                inactive_ids.push(old_id);

                active_relays = if i % 2 == 0 {
                    relays_ab.clone()
                } else {
                    relays_bc.clone()
                };

                active_id = {
                    apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                        unsubscribe(pool, session, old_id);
                        subscribe(
                            pool,
                            session,
                            trivial_filter(),
                            RelayUrlPkgs::new(
                                active_relays.clone(),
                                crate::relay::RelayUrlPolicy::explicit(
                                    crate::relay::RelayDemandPriority::Important,
                                    crate::relay::RelayRoutingPreference::PreferDedicated,
                                ),
                            ),
                        )
                    })
                };
            } else {
                {
                    apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                        if i % 3 == 0 {
                            active_relays = if i % 2 == 0 {
                                relays_ab.clone()
                            } else {
                                relays_bc.clone()
                            };
                            new_relays(pool, session, active_id, active_relays.clone());
                        }
                        new_filters(
                            pool,
                            session,
                            active_id,
                            vec![Filter::new().kinds(vec![(i % 5) as u64]).limit(3).build()],
                        );
                    })
                }
            }

            let active_status = pool.status(&active_id);
            assert_eq!(active_status.len(), active_relays.len());
            for relay in &active_relays {
                assert!(active_status.contains_key(relay));
            }
            for old_id in &inactive_ids {
                assert!(
                    pool.status(old_id).is_empty(),
                    "inactive subscription should not retain relay state"
                );
            }
        }
    }

    /// Under relay saturation with only prefer-dedicated subscriptions, the
    /// existing preferred dedicated route should keep its dedicated slot and the
    /// incoming preferred request should compact instead.
    #[tokio::test]
    async fn saturation_keeps_existing_preferred_dedicated_when_all_preferred_and_full() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-saturation-demotion.example.com").unwrap();

        let _ = ensure_relay(&mut pool, &relay);
        let applied = apply_relay_limit_update_for_test(
            &mut pool,
            &relay,
            RelayLimitations {
                maximum_subs: 1,
                ..Default::default()
            },
        );
        assert!(matches!(
            applied,
            Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
        ));

        let id_first = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![Filter::new().kinds(vec![2]).limit(1).build()],
                    pkgs,
                )
            })
        };

        let id_second = {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            let pkgs = RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            );
            apply_send_session_with(&mut pool, wakeup, |pool, session| {
                subscribe(pool, session, trivial_filter(), pkgs)
            })
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator should exist");
        assert_eq!(
            coordinator.route_type(&id_first),
            Some(RelayType::Transparent)
        );
        assert_eq!(
            coordinator.route_type(&id_second),
            Some(RelayType::Compaction)
        );
    }

    /// Verifies a mixed churn path where a preferred dedicated route is
    /// demoted by a NIP-11 limit shrink, a later required request queues
    /// behind a live required oneshot, receive-path EOSE cleanup frees that
    /// slot, and a later limit increase promotes the preferred route back.
    #[tokio::test]
    async fn required_queue_survives_limit_shrink_oneshot_cleanup_and_reexpand() {
        let mut pool = OutboxPool::default();
        let wakeup = MockWakeup::default();
        let relay = NormRelayUrl::new("wss://relay-required-churn.example.com").unwrap();

        let _ = ensure_relay(&mut pool, &relay);
        let initial = apply_relay_limit_update_for_test(
            &mut pool,
            &relay,
            RelayLimitations {
                maximum_subs: 2,
                ..Default::default()
            },
        );
        assert!(matches!(
            initial,
            Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
        ));
        assert_eq!(
            pool.relays
                .get(&relay)
                .expect("relay present")
                .current_limits()
                .maximum_subs,
            2
        );

        let required_pkgs = |relay: &NormRelayUrl| {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::RequireDedicated,
                ),
            )
        };
        let preferred_pkgs = |relay: &NormRelayUrl| {
            let mut relays = HashSet::new();
            relays.insert(relay.clone());
            RelayUrlPkgs::new(
                relays,
                crate::relay::RelayUrlPolicy::explicit(
                    crate::relay::RelayDemandPriority::Important,
                    RelayRoutingPreference::PreferDedicated,
                ),
            )
        };

        let preferred_dedicated = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![Filter::new().kinds(vec![1]).limit(1).build()],
                    preferred_pkgs(&relay),
                )
            })
        };

        let live_oneshot = {
            let id = pool.next_sub_id();
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                oneshot(
                    pool,
                    session,
                    id,
                    vec![Filter::new().kinds(vec![2]).limit(1).build()],
                    required_pkgs(&relay),
                );
            });
            id
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.route_type(&preferred_dedicated),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.req_status(&preferred_dedicated).is_some());
        assert_eq!(
            coordinator.route_type(&live_oneshot),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.req_status(&live_oneshot).is_some());
        assert!(pool.subs.is_oneshot(&live_oneshot));

        let shrink = apply_relay_limit_update_for_test(
            &mut pool,
            &relay,
            RelayLimitations {
                maximum_subs: 1,
                ..Default::default()
            },
        );
        assert!(matches!(
            shrink,
            Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
        ));

        assert_eq!(
            pool.relays
                .get(&relay)
                .expect("coordinator")
                .current_limits()
                .maximum_subs,
            1
        );
        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.route_type(&preferred_dedicated),
            Some(RelayType::Compaction),
            "preferred route should fall back to compaction when dedicated capacity shrinks"
        );
        assert_eq!(
            coordinator.route_type(&live_oneshot),
            Some(RelayType::Transparent),
            "the younger live oneshot should keep the only remaining dedicated slot"
        );
        assert!(coordinator.req_status(&live_oneshot).is_some());

        let queued_required = {
            apply_send_session_with(&mut pool, wakeup.clone(), |pool, session| {
                subscribe(
                    pool,
                    session,
                    vec![Filter::new().kinds(vec![3]).limit(1).build()],
                    required_pkgs(&relay),
                )
            })
        };

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.transparent_queue_len_for_test(), 1);
        assert_eq!(
            coordinator.route_type(&live_oneshot),
            Some(RelayType::Transparent),
            "the live required oneshot should still own the only dedicated slot"
        );
        assert!(coordinator.req_status(&live_oneshot).is_some());
        assert_eq!(coordinator.req_status(&queued_required), None);

        pool.apply_relay_eose_delta(
            &relay,
            RelayEoseDelta {
                sub_ids: HashSet::from([live_oneshot]),
                invalidated_sub_ids: HashSet::new(),
            },
        );

        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert!(
            pool.subs.get(&live_oneshot).is_none(),
            "receive-path cleanup should remove the completed oneshot"
        );
        assert_eq!(
            coordinator.route_type(&queued_required),
            Some(RelayType::Transparent),
            "oneshot cleanup should immediately promote the queued required sub"
        );
        assert!(coordinator.req_status(&queued_required).is_some());
        assert_eq!(
            coordinator.route_type(&preferred_dedicated),
            Some(RelayType::Compaction),
            "preferred route should remain compacted until dedicated capacity expands again"
        );

        let expand = apply_relay_limit_update_for_test(
            &mut pool,
            &relay,
            RelayLimitations {
                maximum_subs: 2,
                ..Default::default()
            },
        );
        assert!(matches!(
            expand,
            Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
        ));

        assert_eq!(
            pool.relays
                .get(&relay)
                .expect("coordinator")
                .current_limits()
                .maximum_subs,
            2
        );
        let coordinator = pool.relays.get(&relay).expect("coordinator");
        assert_eq!(coordinator.transparent_queue_len_for_test(), 0);
        assert_eq!(
            coordinator.route_type(&preferred_dedicated),
            Some(RelayType::Transparent),
            "limit expansion should promote the preferred compaction route back to dedicated"
        );
        assert!(coordinator.req_status(&preferred_dedicated).is_some());
        assert_eq!(
            coordinator.route_type(&queued_required),
            Some(RelayType::Transparent)
        );
        assert!(coordinator.req_status(&queued_required).is_some());
    }
}
