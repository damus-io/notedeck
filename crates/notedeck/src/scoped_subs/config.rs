use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use enostr::{
    FullHistoryConfig, NormRelayUrl, Pubkey, RelayDemandPriority, RelayRoutingPreference,
    RelayUrlSource,
};
use hashbrown::HashSet;
use nostrdb::{Filter, SendFilter};

/// Stable key used by apps to identify a logical subscription.
///
/// This follows an `egui::Id` style API: callers provide any hashable value,
/// and we store the resulting hashed key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubKey(u64);

/// Stable key for host-owned scoped subscription lifecycle owners.
///
/// This is a semantic alias over [`SubKey`] to keep the callsites explicit
/// about ownership identity vs. logical subscription identity.
pub type SubOwnerKey = SubKey;

impl SubKey {
    /// Build a key from any hashable value.
    pub fn new(value: impl Hash) -> Self {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        Self(hasher.finish())
    }

    /// Access the raw hashed value.
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    /// Start a typed key builder seeded with a stable namespace/root.
    pub fn builder(seed: impl Hash) -> SubKeyBuilder {
        SubKeyBuilder::new(seed)
    }
}

/// Incremental builder for stable subscription keys.
///
/// This avoids ad-hoc string formatting and keeps key construction typed.
pub struct SubKeyBuilder {
    hasher: DefaultHasher,
}

impl SubKeyBuilder {
    /// Create a new builder with a required seed/root.
    pub fn new(seed: impl Hash) -> Self {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        Self { hasher }
    }

    /// Append one typed part to the key path.
    pub fn with(mut self, part: impl Hash) -> Self {
        part.hash(&mut self.hasher);
        self
    }

    /// Finalize into a stable `SubKey`.
    pub fn finish(self) -> SubKey {
        SubKey(self.hasher.finish())
    }
}

/// Scope associated with a subscription.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SubScope {
    /// Scoped to the current account; runtime resolves this to a concrete pubkey.
    Account,
    /// Cross-account scope.
    Global,
}

/// Full logical identity of one scoped subscription declaration.
///
/// Thread-centric mental model (recommended):
/// - `owner`: one thread view lifecycle token (for example one open thread pane)
/// - `key`: the shareable thread remote stream identity, e.g. `replies-by-root(root_id)`
/// - `scope`: whether that thread key is account-scoped or global (usually account-scoped)
///
/// If two thread views open the same root on the same account, they should use:
/// - different `owner`
/// - the same `key`
/// - the same `scope = SubScope::Account`
///
/// The runtime then shares one live outbox subscription for that resolved `(scope, key)`.
///
/// `SubScope::Account` already partitions by account, so do not encode the account pubkey
/// into the `key`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScopedSubIdentity {
    pub owner: SubOwnerKey,
    pub key: SubKey,
    pub scope: SubScope,
}

impl ScopedSubIdentity {
    pub fn new(owner: SubOwnerKey, key: SubKey, scope: SubScope) -> Self {
        Self { owner, key, scope }
    }

    pub fn account(owner: SubOwnerKey, key: SubKey) -> Self {
        Self::new(owner, key, SubScope::Account)
    }

    pub fn global(owner: SubOwnerKey, key: SubKey) -> Self {
        Self::new(owner, key, SubScope::Global)
    }
}

/// Relay behavior for one live subscription created by a scoped subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubRelayPolicy {
    pub(super) demand_priority: RelayDemandPriority,
    pub(super) routing_preference: RelayRoutingPreference,
}

impl SubRelayPolicy {
    /// Relay policy for important selected-account read-relay subscriptions.
    pub fn accounts_read_important() -> Self {
        Self::accounts_read_important_with_preference(RelayRoutingPreference::default())
    }

    /// Relay policy for important selected-account read-relay subscriptions with explicit routing.
    pub fn accounts_read_important_with_preference(
        routing_preference: RelayRoutingPreference,
    ) -> Self {
        Self::new(RelayDemandPriority::Important, routing_preference)
    }

    /// Relay policy for critical selected-account read-relay subscriptions with explicit routing.
    pub fn accounts_read_critical_with_preference(
        routing_preference: RelayRoutingPreference,
    ) -> Self {
        Self::new(RelayDemandPriority::Critical, routing_preference)
    }

    /// Relay policy for additive author-outbox coverage.
    pub fn author_outbox_augmentation() -> Self {
        Self::new(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        )
    }

    /// Relay policy for additive observed-relay coverage.
    pub fn observed_relay_augmentation() -> Self {
        Self::new(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        )
    }

    /// Construct relay behavior from already-complete policy axes.
    pub fn new(
        demand_priority: RelayDemandPriority,
        routing_preference: RelayRoutingPreference,
    ) -> Self {
        Self {
            demand_priority,
            routing_preference,
        }
    }

    /// Inspect the relay demand priority.
    pub fn demand_priority(&self) -> RelayDemandPriority {
        self.demand_priority
    }

    /// Inspect the routing preference.
    pub fn routing_preference(&self) -> RelayRoutingPreference {
        self.routing_preference
    }
}

/// Realization mode for a scoped subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SubExecution {
    /// Realize one live subscription from the selected account's read relays.
    AccountsRead { baseline: SubRelayPolicy },
    /// Realize one live subscription from an explicit relay set.
    Explicit {
        relays: HashSet<NormRelayUrl>,
        policy: SubRelayPolicy,
    },
    /// Realize selected-account read relays plus retained additive relay coverage.
    AccountsReadPlusExplicit {
        baseline: SubRelayPolicy,
        explicit: SubRelayPolicy,
        explicit_relays: HashSet<NormRelayUrl>,
        explicit_source: RelayUrlSource,
    },
    /// Realize a selected-account read-relay baseline plus author-outbox routed coverage.
    AccountsReadWithAuthorOutbox {
        baseline: SubRelayPolicy,
        author_outbox: SubRelayPolicy,
    },
}

/// Realization config for one scoped subscription identity.
///
/// This is configuration only (`execution`, `filters`, optional full-history policy).
/// Identity is carried by [`ScopedSubIdentity`] (`owner + key + scope`).
#[derive(Clone, Debug)]
pub struct SubConfig {
    /// Complete remote realization mode and relay policy.
    pub(super) execution: SubExecution,
    /// Requested remote filters.
    pub(super) filters: Vec<SendFilter>,
    /// Optional background full-history reconciliation request paired to this
    /// scoped subscription.
    pub(super) full_history: Option<SubFullHistoryConfig>,
}

/// Sendable full-history filter set retained by one scoped-sub config.
#[derive(Clone, Debug)]
pub(crate) struct SubFullHistoryConfig {
    filters: Vec<SendFilter>,
}

impl SubFullHistoryConfig {
    /// Build a sendable scoped-sub full-history config from raw filters.
    fn new(filters: Vec<Filter>) -> Self {
        Self {
            filters: normalize_full_history_filters(filters),
        }
    }

    /// Borrow the retained sendable full-history filters.
    pub(crate) fn filters(&self) -> &[SendFilter] {
        &self.filters
    }

    /// Clone retained full-history filters into owned nostrdb filters for relay work.
    pub(crate) fn owned_filters(&self) -> Vec<Filter> {
        owned_filters(&self.filters)
    }

    /// Returns whether this config contains no meaningful history filters.
    fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }
}

impl PartialEq for SubConfig {
    fn eq(&self, other: &Self) -> bool {
        self.execution == other.execution
            && same_canonical_send_filter_set(&self.filters, &other.filters)
            && full_history_configs_have_same_canonical_attributes(
                self.full_history.as_ref(),
                other.full_history.as_ref(),
            )
    }
}

impl Eq for SubConfig {}

impl SubConfig {
    /// Start building a scoped subscription config from its retained filters.
    pub fn builder(filters: Vec<Filter>) -> SubConfigBuilder {
        SubConfigBuilder {
            filters,
            full_history: None,
        }
    }

    fn accounts_read(filters: Vec<Filter>, baseline: SubRelayPolicy) -> Self {
        let filters = normalize_sub_config_filters(filters);
        Self {
            execution: SubExecution::AccountsRead { baseline },
            filters,
            full_history: None,
        }
    }

    fn explicit(
        filters: Vec<Filter>,
        relays: HashSet<NormRelayUrl>,
        policy: SubRelayPolicy,
    ) -> Self {
        let filters = normalize_sub_config_filters(filters);
        Self {
            execution: SubExecution::Explicit { relays, policy },
            filters,
            full_history: None,
        }
    }

    fn accounts_read_plus_explicit_parts(
        filters: Vec<Filter>,
        explicit_relays: HashSet<NormRelayUrl>,
        explicit_source: RelayUrlSource,
        baseline: SubRelayPolicy,
        explicit: SubRelayPolicy,
    ) -> Self {
        let filters = normalize_sub_config_filters(filters);
        Self {
            execution: SubExecution::AccountsReadPlusExplicit {
                baseline,
                explicit,
                explicit_relays,
                explicit_source,
            },
            filters,
            full_history: None,
        }
    }

    /// Runtime author-outbox relay policy, if this config has one.
    pub(super) fn author_outbox_policy(&self) -> Option<SubRelayPolicy> {
        match &self.execution {
            SubExecution::AccountsRead { .. } => None,
            SubExecution::Explicit { .. } => None,
            SubExecution::AccountsReadPlusExplicit { .. } => None,
            SubExecution::AccountsReadWithAuthorOutbox { author_outbox, .. } => {
                Some(*author_outbox)
            }
        }
    }

    /// Runtime explicit relay augmentation policy, if this config has one.
    pub(super) fn explicit_augmentation_policy(&self) -> Option<SubRelayPolicy> {
        match &self.execution {
            SubExecution::AccountsReadPlusExplicit { explicit, .. } => Some(*explicit),
            SubExecution::AccountsRead { .. }
            | SubExecution::Explicit { .. }
            | SubExecution::AccountsReadWithAuthorOutbox { .. } => None,
        }
    }

    /// Inspect the configured background full-history declaration.
    pub(crate) fn full_history_config(&self) -> Option<&SubFullHistoryConfig> {
        self.full_history.as_ref()
    }

    /// Borrow the retained sendable live filters.
    pub(super) fn filters(&self) -> &[SendFilter] {
        &self.filters
    }

    /// Clone retained live filters into owned nostrdb filters for relay work.
    pub(super) fn owned_filters(&self) -> Vec<Filter> {
        owned_filters(&self.filters)
    }

    pub(super) fn baseline_policy(&self) -> SubRelayPolicy {
        match &self.execution {
            SubExecution::AccountsRead { baseline }
            | SubExecution::AccountsReadPlusExplicit { baseline, .. }
            | SubExecution::AccountsReadWithAuthorOutbox { baseline, .. } => *baseline,
            SubExecution::Explicit { policy, .. } => *policy,
        }
    }

    fn accounts_read_with_author_outbox_parts(
        filters: Vec<Filter>,
        baseline: SubRelayPolicy,
        author_outbox: SubRelayPolicy,
    ) -> Self {
        let filters = normalize_sub_config_filters(filters);
        Self {
            execution: SubExecution::AccountsReadWithAuthorOutbox {
                baseline,
                author_outbox,
            },
            filters,
            full_history: None,
        }
    }

    pub(super) fn uses_author_outbox(&self) -> bool {
        self.author_outbox_policy().is_some()
    }

    pub(super) fn uses_accounts_read_plus_explicit(&self) -> bool {
        self.explicit_augmentation_policy().is_some()
    }

    pub(super) fn depends_on_accounts_read(&self) -> bool {
        matches!(
            &self.execution,
            SubExecution::AccountsRead { .. }
                | SubExecution::AccountsReadPlusExplicit { .. }
                | SubExecution::AccountsReadWithAuthorOutbox { .. }
        )
    }

    pub(super) fn merged_owner_configs(configs: &[&SubConfig]) -> Option<SubConfig> {
        let latest = configs.last()?.to_owned().clone();
        let latest_additive = configs
            .iter()
            .rev()
            .find(|config| config.uses_accounts_read_plus_explicit());
        let Some(additive_template) = latest_additive else {
            return Some(latest);
        };

        if configs
            .iter()
            .any(|config| !same_accounts_read_plus_explicit_base(additive_template, config))
        {
            return Some(latest);
        }

        let mut merged = (*additive_template).clone();
        let SubExecution::AccountsReadPlusExplicit {
            explicit_relays, ..
        } = &mut merged.execution
        else {
            return Some(merged);
        };

        explicit_relays.clear();
        for config in configs {
            let SubExecution::AccountsReadPlusExplicit {
                explicit_relays: owner_relays,
                ..
            } = &config.execution
            else {
                continue;
            };
            explicit_relays.extend(owner_relays.iter().cloned());
        }

        Some(merged)
    }
}

fn normalize_sub_config_filters(filters: Vec<Filter>) -> Vec<SendFilter> {
    let filters = normalize_send_filters(filters, "SubConfig requires sendable filters");
    assert!(
        !filters.is_empty(),
        "SubConfig requires at least one filter"
    );
    filters
}

fn normalize_full_history_filters(filters: Vec<Filter>) -> Vec<SendFilter> {
    normalize_send_filters(filters, "SubConfig requires sendable full-history filters")
}

fn normalize_send_filters(filters: Vec<Filter>, sendable_message: &'static str) -> Vec<SendFilter> {
    filters
        .into_iter()
        .filter(|filter| filter.num_elements() != 0)
        .map(|filter| SendFilter::try_from_filter(filter).expect(sendable_message))
        .collect()
}

fn owned_filters(filters: &[SendFilter]) -> Vec<Filter> {
    filters
        .iter()
        .cloned()
        .map(SendFilter::into_filter)
        .collect()
}

fn same_canonical_send_filter_set(left: &[SendFilter], right: &[SendFilter]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    if left
        .iter()
        .zip(right.iter())
        .all(|(left_filter, right_filter)| {
            left_filter
                .as_filter()
                .same_canonical_attributes(right_filter.as_filter())
        })
    {
        return true;
    }

    let mut matched = vec![false; right.len()];
    for left_filter in left {
        let Some((index, _)) = right.iter().enumerate().find(|(index, right_filter)| {
            !matched[*index]
                && left_filter
                    .as_filter()
                    .same_canonical_attributes(right_filter.as_filter())
        }) else {
            return false;
        };
        matched[index] = true;
    }

    true
}

fn same_accounts_read_plus_explicit_base(left: &SubConfig, right: &SubConfig) -> bool {
    let SubExecution::AccountsReadPlusExplicit {
        baseline: left_baseline,
        explicit: left_explicit,
        explicit_source: left_source,
        ..
    } = &left.execution
    else {
        return false;
    };

    let same_relay_mode = match &right.execution {
        // Plain accounts-read owners contribute no additive relays, but they do
        // not subtract additive relays still owned by compatible owners.
        SubExecution::AccountsRead { baseline } => baseline == left_baseline,
        SubExecution::AccountsReadPlusExplicit {
            baseline,
            explicit,
            explicit_source: right_source,
            ..
        } => baseline == left_baseline && explicit == left_explicit && left_source == right_source,
        SubExecution::Explicit { .. } | SubExecution::AccountsReadWithAuthorOutbox { .. } => false,
    };

    same_relay_mode
        && same_canonical_send_filter_set(&left.filters, &right.filters)
        && full_history_configs_have_same_canonical_attributes(
            left.full_history.as_ref(),
            right.full_history.as_ref(),
        )
}

/// Builder entry point for a scoped subscription config.
pub struct SubConfigBuilder {
    pub(super) filters: Vec<Filter>,
    full_history: Option<SubFullHistoryConfig>,
}

impl SubConfigBuilder {
    /// Add or replace the background full-history declaration.
    pub fn full_history(mut self, full_history: FullHistoryConfig) -> Self {
        self.full_history = normalize_full_history_policy(Some(full_history));
        self
    }

    /// Start a subscription resolved from the selected account's read relays.
    pub fn accounts_read(self, baseline: SubRelayPolicy) -> AccountsReadBuilder {
        AccountsReadBuilder {
            filters: self.filters,
            baseline,
            full_history: self.full_history,
        }
    }

    /// Start an important selected-account read-relay subscription.
    pub fn accounts_read_important(self) -> AccountsReadBuilder {
        self.accounts_read(SubRelayPolicy::accounts_read_important())
    }

    /// Start an important selected-account read-relay subscription with explicit routing.
    pub fn accounts_read_important_with_preference(
        self,
        routing_preference: RelayRoutingPreference,
    ) -> AccountsReadBuilder {
        self.accounts_read(SubRelayPolicy::accounts_read_important_with_preference(
            routing_preference,
        ))
    }

    /// Start a critical selected-account read-relay subscription with explicit routing.
    pub fn accounts_read_critical_with_preference(
        self,
        routing_preference: RelayRoutingPreference,
    ) -> AccountsReadBuilder {
        self.accounts_read(SubRelayPolicy::accounts_read_critical_with_preference(
            routing_preference,
        ))
    }

    /// Start a subscription using only the provided explicit relay set.
    pub fn explicit(
        self,
        relays: impl IntoIterator<Item = NormRelayUrl>,
        policy: SubRelayPolicy,
    ) -> ExplicitOnlyBuilder {
        ExplicitOnlyBuilder {
            filters: self.filters,
            relays: relays.into_iter().collect(),
            policy,
            full_history: self.full_history,
        }
    }
}

/// Builder for explicit-only relay coverage.
pub struct ExplicitOnlyBuilder {
    pub(super) filters: Vec<Filter>,
    relays: HashSet<NormRelayUrl>,
    policy: SubRelayPolicy,
    full_history: Option<SubFullHistoryConfig>,
}

impl ExplicitOnlyBuilder {
    /// Add or replace the background full-history declaration.
    pub fn full_history(mut self, full_history: FullHistoryConfig) -> Self {
        self.full_history = normalize_full_history_policy(Some(full_history));
        self
    }

    /// Finish building an explicit-only scoped subscription config.
    pub fn build(self) -> SubConfig {
        SubConfig::explicit(self.filters, self.relays, self.policy)
            .with_full_history(self.full_history)
    }
}

/// Builder for selected-account read-relay coverage.
pub struct AccountsReadBuilder {
    pub(super) filters: Vec<Filter>,
    baseline: SubRelayPolicy,
    full_history: Option<SubFullHistoryConfig>,
}

impl AccountsReadBuilder {
    /// Add or replace the background full-history declaration.
    pub fn full_history(mut self, full_history: FullHistoryConfig) -> Self {
        self.full_history = normalize_full_history_policy(Some(full_history));
        self
    }

    /// Finish building an accounts-read scoped subscription config.
    pub fn build(self) -> SubConfig {
        SubConfig::accounts_read(self.filters, self.baseline).with_full_history(self.full_history)
    }

    /// Add author-outbox coverage to the accounts-read baseline.
    pub fn with_author_outbox(self, author_outbox: SubRelayPolicy) -> AuthorOutboxBuilder {
        AuthorOutboxBuilder {
            filters: self.filters,
            baseline: self.baseline,
            author_outbox,
            full_history: self.full_history,
        }
    }

    /// Add additive author-outbox coverage to the accounts-read baseline.
    pub fn with_author_outbox_augmentation(self) -> AuthorOutboxBuilder {
        self.with_author_outbox(SubRelayPolicy::author_outbox_augmentation())
    }

    /// Add retained explicit relay coverage to the accounts-read baseline.
    ///
    /// `explicit_relays` is retained declared demand. Runtime derives the live
    /// additive legs by subtracting the current account read relay set, so
    /// account relay retargeting can move relays between baseline and
    /// augmentation without losing coverage.
    pub fn with_explicit_relays(
        self,
        explicit_relays: impl IntoIterator<Item = NormRelayUrl>,
        explicit: SubRelayPolicy,
    ) -> ExplicitRelaysBuilder {
        ExplicitRelaysBuilder {
            filters: self.filters,
            explicit_relays: explicit_relays.into_iter().collect(),
            explicit_source: RelayUrlSource::Explicit,
            baseline: self.baseline,
            explicit,
            full_history: self.full_history,
        }
    }

    /// Add retained observed-relay coverage to the accounts-read baseline.
    pub fn with_observed_relays(
        self,
        observed_relays: impl IntoIterator<Item = NormRelayUrl>,
    ) -> ExplicitRelaysBuilder {
        self.with_explicit_relays(
            observed_relays,
            SubRelayPolicy::observed_relay_augmentation(),
        )
        .source(RelayUrlSource::RemoteAdvertised)
    }
}

/// Builder for selected-account read-relay coverage plus explicit relay coverage.
pub struct ExplicitRelaysBuilder {
    pub(super) filters: Vec<Filter>,
    explicit_relays: HashSet<NormRelayUrl>,
    explicit_source: RelayUrlSource,
    baseline: SubRelayPolicy,
    explicit: SubRelayPolicy,
    full_history: Option<SubFullHistoryConfig>,
}

impl ExplicitRelaysBuilder {
    /// Add or replace the background full-history declaration.
    pub fn full_history(mut self, full_history: FullHistoryConfig) -> Self {
        self.full_history = normalize_full_history_policy(Some(full_history));
        self
    }

    fn source(mut self, source: RelayUrlSource) -> Self {
        self.explicit_source = source;
        self
    }

    /// Finish building the baseline-plus-explicit scoped subscription config.
    pub fn build(self) -> SubConfig {
        SubConfig::accounts_read_plus_explicit_parts(
            self.filters,
            self.explicit_relays,
            self.explicit_source,
            self.baseline,
            self.explicit,
        )
        .with_full_history(self.full_history)
    }
}

/// Builder for selected-account read-relay coverage plus author-outbox coverage.
pub struct AuthorOutboxBuilder {
    pub(super) filters: Vec<Filter>,
    baseline: SubRelayPolicy,
    author_outbox: SubRelayPolicy,
    full_history: Option<SubFullHistoryConfig>,
}

impl AuthorOutboxBuilder {
    /// Add or replace generic full-history catchup on the resolved scoped-sub relay set.
    pub fn full_history(mut self, full_history: FullHistoryConfig) -> Self {
        self.full_history = normalize_full_history_policy(Some(full_history));
        self
    }

    /// Finish building the baseline-plus-author-outbox scoped subscription config.
    pub fn build(self) -> SubConfig {
        SubConfig::accounts_read_with_author_outbox_parts(
            self.filters,
            self.baseline,
            self.author_outbox,
        )
        .with_full_history(self.full_history)
    }
}

/// One resolved runtime key for retained/live scoped-sub state.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ScopedSubKey {
    pub(super) scope: ResolvedSubScope,
    pub(super) key: SubKey,
}

impl ScopedSubKey {
    pub(super) fn is_active_for_account(&self, selected_account_pubkey: Pubkey) -> bool {
        self.scope.is_active_for_account(selected_account_pubkey)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum ResolvedSubScope {
    Account(Pubkey),
    Global,
}

impl ResolvedSubScope {
    pub(super) fn is_active_for_account(&self, selected_account_pubkey: Pubkey) -> bool {
        matches!(self, Self::Global)
            || matches!(self, Self::Account(account_pubkey) if *account_pubkey == selected_account_pubkey)
    }
}

/// Result of setting a desired subscription entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetSubResult {
    Created,
    Updated,
    Unchanged,
}

/// Result of ensuring a desired subscription entry exists without mutating it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnsureSubResult {
    Created,
    AlreadyExists,
}

/// Result of clearing one `(owner, key)` ownership link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearSubResult {
    Cleared,
    StillInUse,
    NotFound,
}

/// Relay EOSE status for one live scoped subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedSubRelayEoseStatus {
    /// Number of serviceable relay legs considered for readiness.
    ///
    /// This includes materialized relay statuses and pending relay legs whose
    /// outbox route exists but has not yet produced a relay-local status.
    pub tracked_relays: usize,
    /// Number of desired relay legs the outbox cannot service.
    pub unsupported_relays: usize,
    /// Whether any tracked relay has reached EOSE.
    pub any_eose: bool,
    /// Whether all tracked relays have reached EOSE.
    ///
    /// This is false when `tracked_relays == 0`.
    pub all_eosed: bool,
}

/// Readiness for one live scoped subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedSubLiveReadiness {
    /// Protocol EOSE status for relay-local legs backing the scoped subscription.
    pub relay_eose: ScopedSubRelayEoseStatus,
}

/// Readiness state for one owner-scoped logical subscription key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopedSubReadiness {
    /// No owned scoped subscription exists for the requested `(owner, key, scope)`.
    Missing,
    /// Owned desired state exists, but no live outbox subscription is active.
    ///
    /// This occurs for account-scoped subs while switched away.
    Inactive,
    /// Live outbox subscription exists; readiness state is available.
    Live(ScopedSubLiveReadiness),
}

fn normalize_full_history_policy(
    full_history: Option<FullHistoryConfig>,
) -> Option<SubFullHistoryConfig> {
    full_history
        .map(|full_history| SubFullHistoryConfig::new(full_history.into_filters()))
        .filter(|full_history| !full_history.is_empty())
}

fn full_history_configs_have_same_canonical_attributes(
    left: Option<&SubFullHistoryConfig>,
    right: Option<&SubFullHistoryConfig>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            same_canonical_send_filter_set(left.filters(), right.filters())
        }
        (None, None) => true,
        _ => false,
    }
}

impl SubConfig {
    fn with_full_history(mut self, full_history: Option<SubFullHistoryConfig>) -> Self {
        self.full_history = full_history.filter(|full_history| !full_history.is_empty());
        self
    }
}
