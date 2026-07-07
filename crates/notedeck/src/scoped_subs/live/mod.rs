use enostr::{
    NormRelayUrl, OutboxIdRegistry, OutboxSubId, Pubkey, RelayDemandPriority,
    RelayRoutingPreference, RelayUrlPkgs, RelayUrlPolicy, RelayUrlSource,
};
use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;
use std::collections::{BTreeMap, VecDeque};

use crate::author_outbox::{RoutedFilterShape, RoutedRelayPriority};

use super::config::{ScopedSubKey, SubConfig, SubExecution, SubRelayPolicy};
use super::planner::{AuthorOutboxPlanGeneration, PlannedRoutedRelay};
use super::route_work::RouteWorkResult;
use super::ScopedSubOutboxOps;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SetSubLiveOp {
    EnsurePresent,
    ReplaceExisting,
    ModifyExisting,
}

pub(super) fn plan_set_sub_live_op(
    previous: Option<&SubConfig>,
    next: &SubConfig,
    has_live: bool,
) -> SetSubLiveOp {
    let Some(previous) = previous else {
        return SetSubLiveOp::EnsurePresent;
    };

    if !has_live {
        return SetSubLiveOp::EnsurePresent;
    }

    if previous.baseline_policy() != next.baseline_policy() {
        return SetSubLiveOp::ReplaceExisting;
    }

    SetSubLiveOp::ModifyExisting
}

#[derive(Clone, Debug)]
pub(super) struct RoutedLiveSub {
    pub(super) relay: NormRelayUrl,
    // One relay-specific route. `live_id` is absent until the current author
    // set is materialized into an outbox live sub.
    pub(super) live_id: Option<OutboxSubId>,
    relay_priority: RoutedRelayPriority,
    desired_filters: Vec<Filter>,
    // Desired author coverage for this relay. The materialized copy records
    // the last author set staged into `live_id`, so repeated refreshes can
    // coalesce before issuing another relay-local REQ replacement.
    authors_by_filter_index: HashMap<usize, HashSet<Pubkey>>,
    pending_route_shape_refresh: bool,
    materialized_authors_by_filter_index: Option<HashMap<usize, HashSet<Pubkey>>>,
    materialized_connection_weight: Option<u32>,
}

#[cfg(test)]
impl RoutedLiveSub {
    pub(super) fn author_sets_for_test(&self) -> Vec<Vec<Pubkey>> {
        let mut authors = self
            .authors_by_filter_index
            .iter()
            .map(|(_, authors)| {
                let mut authors = authors.iter().copied().collect::<Vec<_>>();
                authors.sort_unstable();
                authors
            })
            .collect::<Vec<_>>();
        authors.sort();
        authors
    }
}

#[derive(Clone, Debug)]
pub(super) struct RoutedLiveState {
    pub(super) demand_priority: RelayDemandPriority,
    pub(super) routing_preference: RelayRoutingPreference,
    pub(super) relay_url_source: RelayUrlSource,
    route_shape: Option<RoutedFilterShape>,
    // Relay-specific additive coverage for one `ScopedSubKey`.
    pub(super) legs: BTreeMap<NormRelayUrl, RoutedLiveSub>,
    pending_relays: VecDeque<NormRelayUrl>,
    pending_relay_set: HashSet<NormRelayUrl>,
    pending_unsubscribes: Vec<OutboxSubId>,
    applied_plan_generation: Option<AuthorOutboxPlanGeneration>,
    pending_plan: Option<RoutedLivePendingPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RoutedLivePolicy {
    demand_priority: RelayDemandPriority,
    routing_preference: RelayRoutingPreference,
    relay_url_source: RelayUrlSource,
}

#[derive(Clone, Debug)]
struct RoutedLivePendingPlan {
    generation: AuthorOutboxPlanGeneration,
    policy: RoutedLivePolicy,
    route_shape: Option<RoutedFilterShape>,
    route_shape_changed: bool,
    next_plan_index: usize,
    seen_relays: HashSet<NormRelayUrl>,
    cleanup_relays: VecDeque<NormRelayUrl>,
}

struct RoutedLiveRefresh<'a> {
    policy: SubRelayPolicy,
    relay_url_source: RelayUrlSource,
    route_shape: Option<RoutedFilterShape>,
    plan_generation: Option<AuthorOutboxPlanGeneration>,
    routed_relays: &'a [PlannedRoutedRelay],
}

pub(super) struct AuthorOutboxLiveRefresh<'a> {
    pub(super) account_read_relays: &'a HashSet<NormRelayUrl>,
    pub(super) scoped: ScopedSubKey,
    pub(super) spec: &'a SubConfig,
    pub(super) previous: Option<&'a SubConfig>,
    pub(super) plan_generation: Option<AuthorOutboxPlanGeneration>,
    pub(super) routed_relays: &'a [PlannedRoutedRelay],
}

#[derive(Clone, Debug)]
pub(super) struct BaselineLive {
    id: Option<OutboxSubId>,
    policy: SubRelayPolicy,
}

impl BaselineLive {
    fn empty(baseline_policy: SubRelayPolicy) -> Self {
        Self {
            id: None,
            policy: baseline_policy,
        }
    }

    fn has_live_state(&self) -> bool {
        self.id.is_some()
    }

    fn refresh(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        let next_policy = spec.baseline_policy();
        let (id, outbox_ops) =
            refresh_baseline_live_id(ids, account_read_relays, self.id, self.policy, spec);
        self.id = id;
        self.policy = next_policy;
        outbox_ops
    }

    fn unsubscribe(self, _ids: &OutboxIdRegistry) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        if let Some(id) = self.id {
            outbox_ops.unsubscribe(id);
        }
        outbox_ops
    }
}

#[derive(Clone, Debug)]
pub(super) struct SharedLive {
    pub(super) id: OutboxSubId,
    policy: SubRelayPolicy,
    relay_url_source: RelayUrlSource,
}

#[derive(Clone, Debug)]
pub(super) struct AugmentedLive<T> {
    // Selected-account read-relay coverage for the original filters.
    baseline: BaselineLive,
    extra: Option<Box<T>>,
}

impl<T> AugmentedLive<T> {
    fn empty(baseline_policy: SubRelayPolicy) -> Self {
        Self {
            baseline: BaselineLive::empty(baseline_policy),
            extra: None,
        }
    }

    fn from_transition(
        ids: &OutboxIdRegistry,
        previous: Option<&SubConfig>,
        live_state: LiveSubState,
        fallback_policy: SubRelayPolicy,
    ) -> (Self, ScopedSubOutboxOps) {
        let (baseline, outbox_ops) =
            take_accounts_read_baseline_for_transition(ids, previous, live_state, fallback_policy);
        (
            Self {
                baseline,
                extra: None,
            },
            outbox_ops,
        )
    }

    fn refresh_baseline(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        self.baseline.refresh(ids, account_read_relays, spec)
    }

    pub(super) fn baseline_id(&self) -> Option<OutboxSubId> {
        self.baseline.id
    }

    fn has_live_state(&self) -> bool {
        self.baseline.has_live_state() || self.extra.is_some()
    }
}

impl AugmentedLive<SharedLive> {
    fn refresh_shared(
        &mut self,
        ids: &OutboxIdRegistry,
        policy: SubRelayPolicy,
        relay_url_source: RelayUrlSource,
        relays: HashSet<NormRelayUrl>,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        let (extra, outbox_ops) = refresh_shared_live(
            ids,
            self.extra.take().map(|shared| *shared),
            policy,
            relay_url_source,
            relays,
            spec,
        );
        self.extra = extra.map(Box::new);
        outbox_ops
    }

    pub(super) fn shared_id(&self) -> Option<OutboxSubId> {
        self.extra.as_ref().map(|shared| shared.id)
    }

    #[cfg(test)]
    pub(super) fn shared_source_for_id(&self, live_id: OutboxSubId) -> Option<RelayUrlSource> {
        self.extra
            .as_ref()
            .filter(|shared| shared.id == live_id)
            .map(|shared| shared.relay_url_source)
    }
}

impl AugmentedLive<RoutedLiveState> {
    fn refresh_routed(
        &mut self,
        ids: &OutboxIdRegistry,
        refresh: RoutedLiveRefresh<'_>,
    ) -> (RouteWorkResult, ScopedSubOutboxOps) {
        let ((extra, result), outbox_ops) = refresh_routed_live_state(
            ids,
            self.extra.take().map(|routed| *routed),
            RoutedLivePolicy {
                demand_priority: refresh.policy.demand_priority(),
                routing_preference: refresh.policy.routing_preference(),
                relay_url_source: refresh.relay_url_source,
            },
            refresh.route_shape,
            refresh.plan_generation,
            refresh.routed_relays,
        );
        self.extra = extra.map(Box::new);
        (result, outbox_ops)
    }

    pub(super) fn routed(&self) -> Option<&RoutedLiveState> {
        self.extra.as_deref()
    }
}

#[derive(Clone, Debug)]
pub(super) enum LiveSubState {
    Single(OutboxSubId),
    AccountsReadPlusExplicit(AugmentedLive<SharedLive>),
    AccountsReadWithAuthorOutbox(AugmentedLive<RoutedLiveState>),
}

impl LiveSubState {
    pub(super) fn contains_live_id(&self, live_id: OutboxSubId) -> bool {
        match self {
            Self::Single(id) => *id == live_id,
            Self::AccountsReadPlusExplicit(state) => {
                state.baseline_id() == Some(live_id) || state.shared_id() == Some(live_id)
            }
            Self::AccountsReadWithAuthorOutbox(state) => {
                if state.baseline_id() == Some(live_id) {
                    return true;
                }

                state.routed().is_some_and(|routed| {
                    routed.legs.values().any(|leg| leg.live_id == Some(live_id))
                })
            }
        }
    }
}

/// Live outbox subscriptions keyed by retained scoped-sub identity.
#[derive(Default)]
pub(super) struct ScopedSubLiveRuntime {
    states: HashMap<ScopedSubKey, LiveSubState>,
}

impl ScopedSubLiveRuntime {
    /// Return whether one scoped key currently has live relay state.
    pub(super) fn contains_key(&self, scoped: &ScopedSubKey) -> bool {
        self.states.contains_key(scoped)
    }

    /// Return live relay state for one scoped key.
    pub(super) fn get(&self, scoped: &ScopedSubKey) -> Option<&LiveSubState> {
        self.states.get(scoped)
    }

    /// Return scoped keys whose live readiness depends on one outbox sub id.
    pub(super) fn scoped_keys_for_live_id(&self, live_id: OutboxSubId) -> Vec<ScopedSubKey> {
        self.states
            .iter()
            .filter_map(|(scoped, state)| state.contains_live_id(live_id).then_some(scoped.clone()))
            .collect()
    }

    /// Return live-state entry count for scoped-sub tests.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.states.len()
    }

    /// Subscribe a single live state for `spec`.
    pub(super) fn ensure_live_sub(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: ScopedSubKey,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        let (state, outbox_ops) = subscribe_live(ids, account_read_relays, spec);
        if let Some(state) = state {
            self.states.insert(scoped, state);
        }
        outbox_ops
    }

    /// Replace any existing live state for `scoped` with a fresh single live sub.
    pub(super) fn replace_live_sub(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: &ScopedSubKey,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        let mut outbox_ops = self.remove_live_sub(ids, scoped);
        let ops = self.ensure_live_sub(ids, account_read_relays, scoped.clone(), spec);
        outbox_ops.extend(ops);
        outbox_ops
    }

    /// Remove and unsubscribe all live relay state for `scoped`.
    pub(super) fn remove_live_sub(
        &mut self,
        ids: &OutboxIdRegistry,
        scoped: &ScopedSubKey,
    ) -> ScopedSubOutboxOps {
        if let Some(live_state) = self.states.remove(scoped) {
            return unsubscribe_live_state(ids, live_state);
        }
        ScopedSubOutboxOps::default()
    }

    /// Convert previous account-read baseline coverage into `Single` state.
    pub(super) fn adopt_accounts_read_baseline_as_single(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: ScopedSubKey,
        previous: &SubConfig,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        let existing = self.states.remove(&scoped);
        let baseline = existing
            .map(|live_state| {
                let (baseline, ops) = take_accounts_read_baseline_for_transition(
                    ids,
                    Some(previous),
                    live_state,
                    spec.baseline_policy(),
                );
                outbox_ops.extend(ops);
                baseline
            })
            .unwrap_or_else(|| BaselineLive::empty(spec.baseline_policy()));

        let (live_id, ops) =
            refresh_baseline_live_id(ids, account_read_relays, baseline.id, baseline.policy, spec);
        outbox_ops.extend(ops);
        if let Some(live_id) = live_id {
            self.states.insert(scoped, LiveSubState::Single(live_id));
        }
        outbox_ops
    }

    /// Ensure account-read baseline plus explicit additive live coverage.
    pub(super) fn ensure_accounts_read_plus_explicit_live_sub(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: ScopedSubKey,
        spec: &SubConfig,
        previous: Option<&SubConfig>,
    ) -> ScopedSubOutboxOps {
        let Some(explicit_policy) = spec.explicit_augmentation_policy() else {
            return ScopedSubOutboxOps::default();
        };
        let mut outbox_ops = ScopedSubOutboxOps::default();

        let baseline_policy = spec.baseline_policy();
        let existing = self.states.remove(&scoped);
        let mut state = match existing {
            Some(LiveSubState::AccountsReadPlusExplicit(state)) => state,
            Some(live_state) => {
                let (state, ops) =
                    AugmentedLive::from_transition(ids, previous, live_state, baseline_policy);
                outbox_ops.extend(ops);
                state
            }
            None => AugmentedLive::empty(baseline_policy),
        };

        let ops = state.refresh_baseline(ids, account_read_relays, spec);
        outbox_ops.extend(ops);
        let explicit_source = match &spec.execution {
            SubExecution::AccountsReadPlusExplicit {
                explicit_source, ..
            } => *explicit_source,
            _ => RelayUrlSource::Explicit,
        };
        let ops = state.refresh_shared(
            ids,
            explicit_policy,
            explicit_source,
            accounts_read_plus_explicit_additive_relays(spec, account_read_relays),
            spec,
        );
        outbox_ops.extend(ops);

        if state.has_live_state() {
            self.states
                .insert(scoped, LiveSubState::AccountsReadPlusExplicit(state));
        }
        outbox_ops
    }

    /// Refresh account-read baseline and author-outbox routed live coverage together.
    pub(super) fn refresh_author_outbox_live_sub(
        &mut self,
        ids: &OutboxIdRegistry,
        refresh: AuthorOutboxLiveRefresh<'_>,
    ) -> (RouteWorkResult, ScopedSubOutboxOps) {
        let Some(author_outbox_policy) = refresh.spec.author_outbox_policy() else {
            return (RouteWorkResult::Complete, ScopedSubOutboxOps::default());
        };
        let mut outbox_ops = ScopedSubOutboxOps::default();

        let baseline_policy = refresh.spec.baseline_policy();
        let existing = self.states.remove(&refresh.scoped);
        let mut state = match existing {
            Some(LiveSubState::AccountsReadWithAuthorOutbox(state)) => state,
            Some(live_state) => {
                let (state, ops) = AugmentedLive::from_transition(
                    ids,
                    refresh.previous,
                    live_state,
                    baseline_policy,
                );
                outbox_ops.extend(ops);
                state
            }
            None => AugmentedLive::empty(baseline_policy),
        };

        let ops = state.refresh_baseline(ids, refresh.account_read_relays, refresh.spec);
        outbox_ops.extend(ops);
        let (result, ops) = state.refresh_routed(
            ids,
            RoutedLiveRefresh {
                policy: author_outbox_policy,
                relay_url_source: RelayUrlSource::RemoteAdvertised,
                route_shape: RoutedFilterShape::from_filters(&refresh.spec.owned_filters()),
                plan_generation: refresh.plan_generation,
                routed_relays: refresh.routed_relays,
            },
        );
        outbox_ops.extend(ops);

        if state.has_live_state() {
            self.states.insert(
                refresh.scoped,
                LiveSubState::AccountsReadWithAuthorOutbox(state),
            );
        }
        (result, outbox_ops)
    }

    /// Reconcile a desired-config transition whose next execution is single.
    pub(super) fn update_single_live_state_for_set_sub(
        &mut self,
        ids: &OutboxIdRegistry,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: ScopedSubKey,
        previous: &SubConfig,
        spec: &SubConfig,
    ) -> ScopedSubOutboxOps {
        if !is_single_execution(previous) && is_accounts_read_single(spec) {
            return self.adopt_accounts_read_baseline_as_single(
                ids,
                account_read_relays,
                scoped,
                previous,
                spec,
            );
        }

        if !is_single_execution(previous) {
            let mut outbox_ops = self.remove_live_sub(ids, &scoped);
            let ops = self.ensure_live_sub(ids, account_read_relays, scoped, spec);
            outbox_ops.extend(ops);
            return outbox_ops;
        }

        let mut outbox_ops = ScopedSubOutboxOps::default();
        let op = plan_set_sub_live_op(Some(previous), spec, self.states.contains_key(&scoped));

        match op {
            SetSubLiveOp::EnsurePresent => {
                let ops = self.ensure_live_sub(ids, account_read_relays, scoped, spec);
                outbox_ops.extend(ops);
            }
            SetSubLiveOp::ReplaceExisting => {
                let ops = self.replace_live_sub(ids, account_read_relays, &scoped, spec);
                outbox_ops.extend(ops);
            }
            SetSubLiveOp::ModifyExisting => {
                let live_id = match self.states.get(&scoped) {
                    Some(LiveSubState::Single(id)) => *id,
                    _ => return outbox_ops,
                };

                let (live_id, ops) =
                    refresh_single_live_id(ids, account_read_relays, live_id, spec);
                outbox_ops.extend(ops);
                if live_id.is_none() {
                    self.states.remove(&scoped);
                }
            }
        }
        outbox_ops
    }
}

fn unsubscribe_shared_live(_ids: &OutboxIdRegistry, shared: SharedLive) -> ScopedSubOutboxOps {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    outbox_ops.unsubscribe(shared.id);
    outbox_ops
}

fn take_accounts_read_baseline_for_transition(
    ids: &OutboxIdRegistry,
    previous: Option<&SubConfig>,
    live_state: LiveSubState,
    fallback_policy: SubRelayPolicy,
) -> (BaselineLive, ScopedSubOutboxOps) {
    match live_state {
        LiveSubState::Single(live_id) => {
            // A single live sub is reusable as a baseline only when the previous
            // desired config proves it was selected-account read-relay coverage.
            if let Some(previous) = previous.filter(|spec| is_single_accounts_read(spec)) {
                return (
                    BaselineLive {
                        id: Some(live_id),
                        policy: previous.baseline_policy(),
                    },
                    ScopedSubOutboxOps::default(),
                );
            }

            let mut outbox_ops = ScopedSubOutboxOps::default();
            outbox_ops.unsubscribe(live_id);
            (BaselineLive::empty(fallback_policy), outbox_ops)
        }
        LiveSubState::AccountsReadPlusExplicit(state) => {
            let mut outbox_ops = ScopedSubOutboxOps::default();
            // Keep the baseline leg and drop only mode-specific additive coverage.
            if let Some(shared) = state.extra {
                outbox_ops.extend(unsubscribe_shared_live(ids, *shared));
            }
            (state.baseline, outbox_ops)
        }
        LiveSubState::AccountsReadWithAuthorOutbox(state) => {
            let mut outbox_ops = ScopedSubOutboxOps::default();
            // Keep the baseline leg and drop only mode-specific additive coverage.
            if let Some(routed) = state.extra {
                outbox_ops.extend(unsubscribe_routed_live_state(ids, *routed));
            }
            (state.baseline, outbox_ops)
        }
    }
}

fn is_single_accounts_read(spec: &SubConfig) -> bool {
    is_accounts_read_single(spec)
}

fn is_single_execution(spec: &SubConfig) -> bool {
    matches!(
        &spec.execution,
        SubExecution::AccountsRead { .. } | SubExecution::Explicit { .. }
    )
}

fn is_accounts_read_single(spec: &SubConfig) -> bool {
    matches!(&spec.execution, SubExecution::AccountsRead { .. })
}

pub(super) fn resolve_relays(
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> HashSet<NormRelayUrl> {
    match &spec.execution {
        SubExecution::AccountsRead { .. } | SubExecution::AccountsReadWithAuthorOutbox { .. } => {
            account_read_relays.clone()
        }
        SubExecution::Explicit { relays, .. } => relays.clone(),
        SubExecution::AccountsReadPlusExplicit {
            explicit_relays, ..
        } => account_read_relays
            .union(explicit_relays)
            .cloned()
            .collect::<HashSet<_>>(),
    }
}

pub(super) fn baseline_sub_config(spec: &SubConfig) -> SubConfig {
    SubConfig {
        execution: SubExecution::AccountsRead {
            baseline: spec.baseline_policy(),
        },
        filters: spec.filters.clone(),
        full_history: spec.full_history.clone(),
    }
}

fn accounts_read_plus_explicit_additive_relays(
    spec: &SubConfig,
    account_read_relays: &HashSet<NormRelayUrl>,
) -> HashSet<NormRelayUrl> {
    let SubExecution::AccountsReadPlusExplicit {
        explicit_relays,
        explicit_source,
        ..
    } = &spec.execution
    else {
        return HashSet::new();
    };

    explicit_relays
        .difference(account_read_relays)
        .filter(|relay| relay.allowed_for_source(*explicit_source))
        .cloned()
        .collect()
}

pub(super) fn subscribe_live(
    ids: &OutboxIdRegistry,
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> (Option<LiveSubState>, ScopedSubOutboxOps) {
    let (id, outbox_ops) = subscribe_single_live_id(ids, account_read_relays, spec);
    (id.map(LiveSubState::Single), outbox_ops)
}

pub(super) fn subscribe_single_live_id(
    ids: &OutboxIdRegistry,
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> (Option<OutboxSubId>, ScopedSubOutboxOps) {
    let relays = resolve_relays(account_read_relays, spec);
    if relays.is_empty() {
        return (None, ScopedSubOutboxOps::default());
    }

    let policy = spec.baseline_policy();
    let relay_pkgs = RelayUrlPkgs::new(
        relays,
        enostr::RelayUrlPolicy::explicit(policy.demand_priority(), policy.routing_preference()),
    );
    stage_live_subscribe(ids, spec.owned_filters(), relay_pkgs)
}

pub(super) fn refresh_baseline_live_id(
    ids: &OutboxIdRegistry,
    account_read_relays: &HashSet<NormRelayUrl>,
    existing: Option<OutboxSubId>,
    existing_policy: SubRelayPolicy,
    spec: &SubConfig,
) -> (Option<OutboxSubId>, ScopedSubOutboxOps) {
    let next_policy = spec.baseline_policy();
    let baseline_spec = baseline_sub_config(spec);
    if resolve_relays(account_read_relays, &baseline_spec).is_empty() {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        if let Some(live_id) = existing {
            outbox_ops.unsubscribe(live_id);
        }
        return (None, outbox_ops);
    }

    if let Some(live_id) = existing {
        if existing_policy == next_policy {
            return refresh_single_live_id(ids, account_read_relays, live_id, &baseline_spec);
        }

        let mut outbox_ops = ScopedSubOutboxOps::default();
        outbox_ops.unsubscribe(live_id);
        let (id, ops) = subscribe_single_live_id(ids, account_read_relays, &baseline_spec);
        outbox_ops.extend(ops);
        return (id, outbox_ops);
    }

    subscribe_single_live_id(ids, account_read_relays, &baseline_spec)
}

fn subscribe_shared_live(
    ids: &OutboxIdRegistry,
    relays: HashSet<NormRelayUrl>,
    spec: &SubConfig,
    policy: SubRelayPolicy,
    relay_url_source: RelayUrlSource,
) -> (Option<SharedLive>, ScopedSubOutboxOps) {
    if relays.is_empty() || spec.filters().is_empty() {
        return (None, ScopedSubOutboxOps::default());
    }

    let relay_pkgs = RelayUrlPkgs::new(
        relays,
        enostr::RelayUrlPolicy::new(
            relay_url_source,
            policy.demand_priority(),
            policy.routing_preference(),
        ),
    );
    let (live_id, outbox_ops) = stage_live_subscribe(ids, spec.owned_filters(), relay_pkgs);
    (
        live_id.map(|live_id| SharedLive {
            id: live_id,
            policy,
            relay_url_source,
        }),
        outbox_ops,
    )
}

fn refresh_shared_live(
    ids: &OutboxIdRegistry,
    existing: Option<SharedLive>,
    policy: SubRelayPolicy,
    relay_url_source: RelayUrlSource,
    relays: HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> (Option<SharedLive>, ScopedSubOutboxOps) {
    if relays.is_empty() || spec.filters().is_empty() {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        if let Some(existing) = existing {
            outbox_ops.extend(unsubscribe_shared_live(ids, existing));
        }
        return (None, outbox_ops);
    }

    if let Some(existing) = existing {
        if existing.policy == policy && existing.relay_url_source == relay_url_source {
            let relay_pkgs = RelayUrlPkgs::new(
                relays,
                enostr::RelayUrlPolicy::new(
                    relay_url_source,
                    policy.demand_priority(),
                    policy.routing_preference(),
                ),
            );
            let (_, outbox_ops) =
                stage_live_modify(ids, existing.id, spec.owned_filters(), relay_pkgs);
            return (Some(existing), outbox_ops);
        }

        let mut outbox_ops = unsubscribe_shared_live(ids, existing);
        let (shared, ops) = subscribe_shared_live(ids, relays, spec, policy, relay_url_source);
        outbox_ops.extend(ops);
        return (shared, outbox_ops);
    }

    subscribe_shared_live(ids, relays, spec, policy, relay_url_source)
}

fn refresh_single_live_id(
    ids: &OutboxIdRegistry,
    account_read_relays: &HashSet<NormRelayUrl>,
    live_id: OutboxSubId,
    spec: &SubConfig,
) -> (Option<OutboxSubId>, ScopedSubOutboxOps) {
    let relays = resolve_relays(account_read_relays, spec);
    if relays.is_empty() {
        let mut outbox_ops = ScopedSubOutboxOps::default();
        outbox_ops.unsubscribe(live_id);
        return (None, outbox_ops);
    }

    let policy = spec.baseline_policy();
    let relay_pkgs = RelayUrlPkgs::new(
        relays,
        enostr::RelayUrlPolicy::explicit(policy.demand_priority(), policy.routing_preference()),
    );
    let (_, outbox_ops) = stage_live_modify(ids, live_id, spec.owned_filters(), relay_pkgs);
    (Some(live_id), outbox_ops)
}

fn stage_live_subscribe(
    ids: &OutboxIdRegistry,
    filters: Vec<Filter>,
    relay_pkgs: RelayUrlPkgs,
) -> (Option<OutboxSubId>, ScopedSubOutboxOps) {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    let id = outbox_ops.try_subscribe(ids, filters, relay_pkgs);
    (id, outbox_ops)
}

fn stage_live_modify(
    _ids: &OutboxIdRegistry,
    live_id: OutboxSubId,
    filters: Vec<Filter>,
    relay_pkgs: RelayUrlPkgs,
) -> (bool, ScopedSubOutboxOps) {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    outbox_ops.set_live(live_id, filters, relay_pkgs);
    (true, outbox_ops)
}

fn empty_routed_live_state(
    policy: RoutedLivePolicy,
    route_shape: Option<RoutedFilterShape>,
) -> RoutedLiveState {
    RoutedLiveState {
        demand_priority: policy.demand_priority,
        routing_preference: policy.routing_preference,
        relay_url_source: policy.relay_url_source,
        route_shape,
        legs: BTreeMap::new(),
        pending_relays: VecDeque::new(),
        pending_relay_set: HashSet::new(),
        pending_unsubscribes: Vec::new(),
        applied_plan_generation: None,
        pending_plan: None,
    }
}

fn refresh_routed_live_state(
    ids: &OutboxIdRegistry,
    existing: Option<RoutedLiveState>,
    policy: RoutedLivePolicy,
    route_shape: Option<RoutedFilterShape>,
    plan_generation: Option<AuthorOutboxPlanGeneration>,
    routed_relays: &[PlannedRoutedRelay],
) -> (
    (Option<RoutedLiveState>, RouteWorkResult),
    ScopedSubOutboxOps,
) {
    let Some(mut next_state) = prepare_routed_live_state(existing, policy, route_shape.clone())
    else {
        return (
            (None, RouteWorkResult::Complete),
            ScopedSubOutboxOps::default(),
        );
    };

    if let Some(generation) = plan_generation {
        let apply_result = apply_routed_live_plan(
            &mut next_state,
            policy,
            route_shape,
            generation,
            routed_relays,
        );
        if apply_result != RouteWorkResult::Complete {
            return (
                (
                    finish_routed_live_state(next_state),
                    RouteWorkResult::FullRefreshRequired,
                ),
                ScopedSubOutboxOps::default(),
            );
        }
    }

    let (result, outbox_ops) = drain_routed_live_ops(ids, &mut next_state, policy);
    ((finish_routed_live_state(next_state), result), outbox_ops)
}

fn prepare_routed_live_state(
    existing: Option<RoutedLiveState>,
    policy: RoutedLivePolicy,
    route_shape: Option<RoutedFilterShape>,
) -> Option<RoutedLiveState> {
    match existing {
        None => Some(empty_routed_live_state(policy, route_shape)),
        Some(existing)
            if existing.routing_preference != policy.routing_preference
                || existing.demand_priority != policy.demand_priority
                || existing.relay_url_source != policy.relay_url_source =>
        {
            let mut next = empty_routed_live_state(policy, route_shape);
            next.pending_unsubscribes = routed_live_state_live_ids(existing);
            Some(next)
        }
        Some(mut existing) => {
            existing.demand_priority = policy.demand_priority;
            existing.routing_preference = policy.routing_preference;
            existing.relay_url_source = policy.relay_url_source;
            Some(existing)
        }
    }
}

fn apply_routed_live_plan(
    state: &mut RoutedLiveState,
    policy: RoutedLivePolicy,
    route_shape: Option<RoutedFilterShape>,
    generation: AuthorOutboxPlanGeneration,
    planned_by_relay: &[PlannedRoutedRelay],
) -> RouteWorkResult {
    ensure_routed_live_pending_plan(state, policy, route_shape, generation);
    advance_routed_live_plan_application(state, policy, planned_by_relay)
}

fn ensure_routed_live_pending_plan(
    state: &mut RoutedLiveState,
    policy: RoutedLivePolicy,
    route_shape: Option<RoutedFilterShape>,
    generation: AuthorOutboxPlanGeneration,
) {
    if state.pending_plan.as_ref().is_some_and(|pending| {
        pending.generation == generation
            && pending.policy == policy
            && pending.route_shape.as_ref() == route_shape.as_ref()
    }) {
        return;
    }

    if state.pending_plan.is_none()
        && state.applied_plan_generation == Some(generation)
        && state.route_shape.as_ref() == route_shape.as_ref()
        && state.demand_priority == policy.demand_priority
        && state.routing_preference == policy.routing_preference
        && state.relay_url_source == policy.relay_url_source
    {
        return;
    }

    let route_shape_changed = state.route_shape.as_ref() != route_shape.as_ref();
    state.demand_priority = policy.demand_priority;
    state.routing_preference = policy.routing_preference;
    state.relay_url_source = policy.relay_url_source;
    state.route_shape = route_shape.clone();
    state.pending_plan = Some(RoutedLivePendingPlan {
        generation,
        policy,
        route_shape,
        route_shape_changed,
        next_plan_index: 0,
        seen_relays: HashSet::new(),
        cleanup_relays: state.legs.keys().cloned().collect(),
    });
}

fn advance_routed_live_plan_application(
    state: &mut RoutedLiveState,
    policy: RoutedLivePolicy,
    planned_by_relay: &[PlannedRoutedRelay],
) -> RouteWorkResult {
    let Some(mut pending) = state.pending_plan.take() else {
        return RouteWorkResult::Complete;
    };

    loop {
        if pending.next_plan_index >= planned_by_relay.len() && pending.cleanup_relays.is_empty() {
            state.applied_plan_generation = Some(pending.generation);
            return RouteWorkResult::Complete;
        }

        if pending.next_plan_index < planned_by_relay.len() {
            let plan = &planned_by_relay[pending.next_plan_index];
            pending.next_plan_index += 1;
            apply_one_routed_live_plan_relay(state, &mut pending, policy, plan);
            continue;
        }

        if let Some(relay) = pending.cleanup_relays.pop_front() {
            cleanup_one_removed_routed_live_relay(state, &pending, &relay);
            continue;
        }

        debug_assert!(
            false,
            "routed live plan loop should complete before this point"
        );
        state.pending_plan = Some(pending);
        return RouteWorkResult::RebuildRequired;
    }
}

fn apply_one_routed_live_plan_relay(
    state: &mut RoutedLiveState,
    pending: &mut RoutedLivePendingPlan,
    policy: RoutedLivePolicy,
    plan: &PlannedRoutedRelay,
) {
    if !plan.relay.allowed_for_source(policy.relay_url_source) {
        return;
    }
    pending.seen_relays.insert(plan.relay.clone());

    let Some(existing_leg) = state.legs.get_mut(&plan.relay) else {
        enqueue_pending_routed_relay(
            &mut state.pending_relays,
            &mut state.pending_relay_set,
            plan.relay.clone(),
        );
        state.legs.insert(
            plan.relay.clone(),
            RoutedLiveSub {
                relay: plan.relay.clone(),
                live_id: None,
                relay_priority: plan.relay_priority,
                desired_filters: plan.filters.clone(),
                authors_by_filter_index: plan.authors_by_filter_index.clone(),
                pending_route_shape_refresh: false,
                materialized_authors_by_filter_index: None,
                materialized_connection_weight: None,
            },
        );
        return;
    };

    existing_leg.desired_filters = plan.filters.clone();
    existing_leg.authors_by_filter_index = plan.authors_by_filter_index.clone();
    if existing_leg.relay_priority != plan.relay_priority {
        existing_leg.relay_priority = plan.relay_priority;
        enqueue_pending_routed_relay(
            &mut state.pending_relays,
            &mut state.pending_relay_set,
            existing_leg.relay.clone(),
        );
    } else if pending.route_shape_changed {
        existing_leg.pending_route_shape_refresh = true;
        enqueue_pending_routed_relay(
            &mut state.pending_relays,
            &mut state.pending_relay_set,
            existing_leg.relay.clone(),
        );
    } else {
        enqueue_routed_leg_if_op_required(
            &mut state.pending_relays,
            &mut state.pending_relay_set,
            existing_leg,
        );
    }
}

fn cleanup_one_removed_routed_live_relay(
    state: &mut RoutedLiveState,
    pending: &RoutedLivePendingPlan,
    relay: &NormRelayUrl,
) {
    if pending.seen_relays.contains(relay) {
        return;
    }
    let Some(existing_leg) = state.legs.get_mut(relay) else {
        return;
    };

    existing_leg.authors_by_filter_index.clear();
    existing_leg.desired_filters.clear();
    existing_leg.pending_route_shape_refresh = false;
    enqueue_routed_leg_if_op_required(
        &mut state.pending_relays,
        &mut state.pending_relay_set,
        existing_leg,
    );
}

fn routed_live_state_live_ids(state: RoutedLiveState) -> Vec<OutboxSubId> {
    let mut live_ids = state.pending_unsubscribes;
    live_ids.extend(state.legs.into_values().filter_map(|leg| leg.live_id));
    live_ids
}

fn enqueue_routed_leg_if_op_required(
    pending_relays: &mut VecDeque<NormRelayUrl>,
    pending_relay_set: &mut HashSet<NormRelayUrl>,
    leg: &RoutedLiveSub,
) {
    if planned_routed_leg_op(leg) != RoutedLiveLegOp::Noop {
        enqueue_pending_routed_relay(pending_relays, pending_relay_set, leg.relay.clone());
    }
}

fn enqueue_pending_routed_relay(
    pending_relays: &mut VecDeque<NormRelayUrl>,
    pending_relay_set: &mut HashSet<NormRelayUrl>,
    relay: NormRelayUrl,
) {
    if pending_relay_set.insert(relay.clone()) {
        pending_relays.push_back(relay);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoutedLiveLegOp {
    Noop,
    Subscribe,
    SetLive,
    ReplaceLive,
    Unsubscribe,
}

fn planned_routed_leg_op(leg: &RoutedLiveSub) -> RoutedLiveLegOp {
    if leg.desired_filters.is_empty() || leg.authors_by_filter_index.is_empty() {
        return if leg.live_id.is_some() {
            RoutedLiveLegOp::Unsubscribe
        } else {
            RoutedLiveLegOp::Noop
        };
    }

    let Some(_) = leg.live_id else {
        return RoutedLiveLegOp::Subscribe;
    };

    if leg.materialized_connection_weight != Some(leg.relay_priority.connection_weight) {
        return RoutedLiveLegOp::ReplaceLive;
    }

    if leg.pending_route_shape_refresh
        || leg.materialized_authors_by_filter_index.as_ref() != Some(&leg.authors_by_filter_index)
    {
        return RoutedLiveLegOp::SetLive;
    }

    RoutedLiveLegOp::Noop
}

fn drain_routed_live_ops(
    ids: &OutboxIdRegistry,
    existing: &mut RoutedLiveState,
    policy: RoutedLivePolicy,
) -> (RouteWorkResult, ScopedSubOutboxOps) {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    while let Some(live_id) = existing.pending_unsubscribes.pop() {
        outbox_ops.unsubscribe(live_id);
    }

    while let Some(relay) = existing.pending_relays.pop_front() {
        if !existing.pending_relay_set.remove(&relay) {
            continue;
        }

        let Some(leg) = existing.legs.get(&relay) else {
            continue;
        };
        let op = planned_routed_leg_op(leg);
        if op == RoutedLiveLegOp::Noop {
            continue;
        }
        let next_filters = leg.desired_filters.clone();
        if op == RoutedLiveLegOp::Unsubscribe {
            let Some(leg) = existing.legs.remove(&relay) else {
                continue;
            };
            if let Some(live_id) = leg.live_id {
                outbox_ops.unsubscribe(live_id);
            }
            continue;
        }

        let mut subscribe_after_op = matches!(
            op,
            RoutedLiveLegOp::Subscribe | RoutedLiveLegOp::ReplaceLive
        );
        let relay_policy = {
            let leg = existing
                .legs
                .get_mut(&relay)
                .expect("pending routed relay should still have a leg");
            let relay_policy = routed_leg_relay_policy(policy, leg);

            match op {
                RoutedLiveLegOp::SetLive => {
                    let Some(live_id) = leg.live_id else {
                        continue;
                    };
                    let relay_pkgs =
                        RelayUrlPkgs::new(HashSet::from_iter([relay.clone()]), relay_policy);
                    let (modified, ops) =
                        stage_live_modify(ids, live_id, next_filters.clone(), relay_pkgs);
                    outbox_ops.extend(ops);
                    if modified {
                        leg.materialized_authors_by_filter_index =
                            Some(leg.authors_by_filter_index.clone());
                        leg.materialized_connection_weight =
                            Some(leg.relay_priority.connection_weight);
                        leg.pending_route_shape_refresh = false;
                    }
                    subscribe_after_op = false;
                }
                RoutedLiveLegOp::ReplaceLive => {
                    if let Some(live_id) = leg.live_id {
                        outbox_ops.unsubscribe(live_id);
                        leg.live_id = None;
                    }
                }
                RoutedLiveLegOp::Subscribe => {}
                RoutedLiveLegOp::Noop | RoutedLiveLegOp::Unsubscribe => unreachable!(),
            }
            relay_policy
        };

        if !subscribe_after_op {
            continue;
        }

        let relay_pkgs = RelayUrlPkgs::new(HashSet::from_iter([relay.clone()]), relay_policy);
        let (live_id, ops) = stage_live_subscribe(ids, next_filters, relay_pkgs);
        outbox_ops.extend(ops);
        let Some(live_id) = live_id else {
            continue;
        };
        let leg = existing
            .legs
            .get_mut(&relay)
            .expect("subscribed routed relay should still have a leg");
        leg.live_id = Some(live_id);
        leg.materialized_authors_by_filter_index = Some(leg.authors_by_filter_index.clone());
        leg.materialized_connection_weight = Some(leg.relay_priority.connection_weight);
        leg.pending_route_shape_refresh = false;
    }

    (RouteWorkResult::Complete, outbox_ops)
}

fn routed_leg_relay_policy(policy: RoutedLivePolicy, leg: &RoutedLiveSub) -> RelayUrlPolicy {
    RelayUrlPolicy::new(
        policy.relay_url_source,
        policy.demand_priority,
        policy.routing_preference,
    )
    .with_connection_weight(leg.relay_priority.connection_weight)
}

fn finish_routed_live_state(mut existing: RoutedLiveState) -> Option<RoutedLiveState> {
    existing
        .legs
        .retain(|_, leg| leg.live_id.is_some() || !leg.authors_by_filter_index.is_empty());
    existing
        .pending_relay_set
        .retain(|relay| existing.legs.contains_key(relay));
    existing
        .pending_relays
        .retain(|relay| existing.pending_relay_set.contains(relay));
    if existing.legs.is_empty()
        && existing.pending_unsubscribes.is_empty()
        && existing.pending_plan.is_none()
    {
        return None;
    }

    Some(RoutedLiveState {
        demand_priority: existing.demand_priority,
        routing_preference: existing.routing_preference,
        relay_url_source: existing.relay_url_source,
        route_shape: existing.route_shape,
        legs: existing.legs,
        pending_relays: existing.pending_relays,
        pending_relay_set: existing.pending_relay_set,
        pending_unsubscribes: existing.pending_unsubscribes,
        applied_plan_generation: existing.applied_plan_generation,
        pending_plan: existing.pending_plan,
    })
}

pub(super) fn unsubscribe_routed_live_state(
    _ids: &OutboxIdRegistry,
    state: RoutedLiveState,
) -> ScopedSubOutboxOps {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    for live_id in state.pending_unsubscribes {
        outbox_ops.unsubscribe(live_id);
    }
    for leg in state.legs.into_values() {
        if let Some(live_id) = leg.live_id {
            outbox_ops.unsubscribe(live_id);
        }
    }
    outbox_ops
}

pub(super) fn unsubscribe_live_state(
    ids: &OutboxIdRegistry,
    live_state: LiveSubState,
) -> ScopedSubOutboxOps {
    let mut outbox_ops = ScopedSubOutboxOps::default();
    match live_state {
        LiveSubState::Single(id) => {
            outbox_ops.unsubscribe(id);
        }
        LiveSubState::AccountsReadPlusExplicit(state) => {
            outbox_ops.extend(state.baseline.unsubscribe(ids));
            if let Some(shared) = state.extra {
                outbox_ops.extend(unsubscribe_shared_live(ids, *shared));
            }
        }
        LiveSubState::AccountsReadWithAuthorOutbox(state) => {
            outbox_ops.extend(state.baseline.unsubscribe(ids));
            if let Some(routed) = state.extra {
                outbox_ops.extend(unsubscribe_routed_live_state(ids, *routed));
            }
        }
    }
    outbox_ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::RemoteOutboxReadModelHarness;

    fn test_pubkey(byte: u8) -> Pubkey {
        Pubkey::new([byte; 32])
    }

    fn ensure_routed_live_state(
        policy: RoutedLivePolicy,
        route_shape: Option<RoutedFilterShape>,
        routed_relays: &[PlannedRoutedRelay],
    ) -> Option<RoutedLiveState> {
        let mut state = empty_routed_live_state(policy, route_shape);

        for plan in routed_relays {
            if !plan.relay.allowed_for_source(policy.relay_url_source) {
                continue;
            }
            let leg = RoutedLiveSub {
                relay: plan.relay.clone(),
                live_id: None,
                relay_priority: plan.relay_priority,
                desired_filters: plan.filters.clone(),
                authors_by_filter_index: plan.authors_by_filter_index.clone(),
                pending_route_shape_refresh: false,
                materialized_authors_by_filter_index: None,
                materialized_connection_weight: None,
            };
            state.legs.insert(plan.relay.clone(), leg);
            enqueue_pending_routed_relay(
                &mut state.pending_relays,
                &mut state.pending_relay_set,
                plan.relay.clone(),
            );
        }

        if state.legs.is_empty() {
            return None;
        }

        Some(state)
    }

    #[test]
    fn planned_routed_leg_op_is_noop_when_materialized_authors_match() {
        let relay = NormRelayUrl::new("wss://materialized-routed.example.com").expect("relay");
        let author = test_pubkey(0xC3);
        let authors_by_filter_index = HashMap::from([(0, HashSet::from([author]))]);
        let desired_filters = vec![Filter::new().authors([author.bytes()]).kinds([1]).build()];
        let leg = RoutedLiveSub {
            relay,
            live_id: Some(OutboxSubId(42)),
            relay_priority: RoutedRelayPriority::default(),
            desired_filters,
            authors_by_filter_index: authors_by_filter_index.clone(),
            pending_route_shape_refresh: false,
            materialized_authors_by_filter_index: Some(authors_by_filter_index),
            materialized_connection_weight: Some(0),
        };

        assert_eq!(planned_routed_leg_op(&leg), RoutedLiveLegOp::Noop);
    }

    #[test]
    fn planned_routed_leg_op_sets_live_when_desired_authors_change() {
        let relay = NormRelayUrl::new("wss://set-routed.example.com").expect("relay");
        let old_author = test_pubkey(0xC4);
        let new_author = test_pubkey(0xC5);
        let desired_filters = vec![Filter::new()
            .authors([new_author.bytes()])
            .kinds([1])
            .build()];
        let leg = RoutedLiveSub {
            relay,
            live_id: Some(OutboxSubId(43)),
            relay_priority: RoutedRelayPriority::default(),
            desired_filters,
            authors_by_filter_index: HashMap::from([(0, HashSet::from([new_author]))]),
            pending_route_shape_refresh: false,
            materialized_authors_by_filter_index: Some(HashMap::from([(
                0,
                HashSet::from([old_author]),
            )])),
            materialized_connection_weight: Some(0),
        };

        assert_eq!(planned_routed_leg_op(&leg), RoutedLiveLegOp::SetLive);
    }

    #[test]
    fn planned_routed_leg_op_replaces_live_when_connection_weight_changes() {
        let relay = NormRelayUrl::new("wss://weighted-routed.example.com").expect("relay");
        let author = test_pubkey(0xC7);
        let authors_by_filter_index = HashMap::from([(0, HashSet::from([author]))]);
        let desired_filters = vec![Filter::new().authors([author.bytes()]).kinds([1]).build()];
        let leg = RoutedLiveSub {
            relay,
            live_id: Some(OutboxSubId(44)),
            relay_priority: RoutedRelayPriority {
                connection_weight: 5,
                order: 0,
            },
            desired_filters,
            authors_by_filter_index: authors_by_filter_index.clone(),
            pending_route_shape_refresh: false,
            materialized_authors_by_filter_index: Some(authors_by_filter_index),
            materialized_connection_weight: Some(1),
        };

        assert_eq!(planned_routed_leg_op(&leg), RoutedLiveLegOp::ReplaceLive);
    }

    #[test]
    fn routed_plan_application_materializes_all_planned_relays() {
        let author = test_pubkey(0xD0);
        let source_filter = Filter::new()
            .authors([author.bytes()])
            .kinds([1])
            .limit(10)
            .build();
        let planned = (0..10)
            .map(|index| {
                let relay = NormRelayUrl::new(&format!("wss://route-apply-{index}.example.com"))
                    .expect("relay");
                PlannedRoutedRelay {
                    relay,
                    relay_priority: RoutedRelayPriority::default(),
                    filters: vec![source_filter.clone()],
                    authors_by_filter_index: HashMap::from([(0, HashSet::from([author]))]),
                }
            })
            .collect::<Vec<_>>();
        let policy = RoutedLivePolicy {
            demand_priority: RelayDemandPriority::Opportunistic,
            routing_preference: RelayRoutingPreference::NoPreference,
            relay_url_source: RelayUrlSource::RemoteAdvertised,
        };
        let mut bridge = RemoteOutboxReadModelHarness::default();

        let (state, result) = bridge.with_returned_outbox(|ids| {
            refresh_routed_live_state(
                ids,
                None,
                policy,
                RoutedFilterShape::from_filters(std::slice::from_ref(&source_filter)),
                Some(1),
                &planned,
            )
        });
        let state = state.expect("routed state");

        assert_eq!(result, RouteWorkResult::Complete);
        assert_eq!(state.legs.len(), planned.len());
        assert!(state.pending_plan.is_none());
        assert!(state.pending_relay_set.is_empty());
        assert!(state.legs.values().all(|leg| leg.live_id.is_some()));
    }

    #[test]
    fn routed_live_ops_materialize_all_pending_relays() {
        let author = test_pubkey(0xC9);
        let source_filter = Filter::new()
            .authors([author.bytes()])
            .kinds([1])
            .limit(10)
            .build();
        let planned = (0..4)
            .map(|index| {
                let relay = NormRelayUrl::new(&format!("wss://window-grow-{index}.example.com"))
                    .expect("relay");
                PlannedRoutedRelay {
                    relay,
                    relay_priority: RoutedRelayPriority::default(),
                    filters: vec![source_filter.clone()],
                    authors_by_filter_index: HashMap::from([(0, HashSet::from([author]))]),
                }
            })
            .collect::<Vec<_>>();
        let mut state = ensure_routed_live_state(
            RoutedLivePolicy {
                demand_priority: RelayDemandPriority::Opportunistic,
                routing_preference: RelayRoutingPreference::NoPreference,
                relay_url_source: RelayUrlSource::RemoteAdvertised,
            },
            RoutedFilterShape::from_filters(std::slice::from_ref(&source_filter)),
            &planned,
        )
        .expect("routed state");
        let mut bridge = RemoteOutboxReadModelHarness::default();

        let result = bridge.with_returned_outbox(|ids| {
            drain_routed_live_ops(
                ids,
                &mut state,
                RoutedLivePolicy {
                    demand_priority: RelayDemandPriority::Opportunistic,
                    routing_preference: RelayRoutingPreference::NoPreference,
                    relay_url_source: RelayUrlSource::RemoteAdvertised,
                },
            )
        });

        assert_eq!(result, RouteWorkResult::Complete);
        assert_eq!(
            state
                .legs
                .values()
                .filter(|leg| leg.live_id.is_some())
                .count(),
            planned.len()
        );
        assert!(state.pending_relay_set.is_empty());
        assert!(state.pending_relays.is_empty());
    }
}
