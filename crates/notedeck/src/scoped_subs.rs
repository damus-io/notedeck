use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::{Accounts, Outbox};
use enostr::{
    same_canonical_filter_set, FullHistoryConfig, FullHistorySubId, NormRelayUrl, OutboxSubId,
    Pubkey, RelayReqStatus, RelayRoutingPreference, RelayUrlPkgs,
};
use hashbrown::{HashMap, HashSet};
use nostrdb::Filter;

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

/// Opaque owner slot id.
///
/// Host/app containers create one slot per UI lifecycle owner (route/view instance)
/// and use it to attach scoped subscription intent to that owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubSlotId(u64);

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

/// Relay selection policy for a subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelaySelection {
    /// Resolve relay set from the currently selected account's read relays.
    AccountsRead,
    /// Use an explicit relay set.
    Explicit(HashSet<NormRelayUrl>),
}

/// Realization config for one scoped subscription identity.
///
/// This is configuration only; identity is carried by [`ScopedSubIdentity`]
/// (`owner + key + scope`).
#[derive(Clone, Debug)]
pub struct SubConfig {
    relays: RelaySelection,
    live_filters: Vec<Filter>,
    full_history: Option<FullHistoryConfig>,
    /// Routing intent when dedicated relay capacity is constrained.
    routing_preference: RelayRoutingPreference,
}

/// Builder for a scoped subscription declaration.
#[derive(Clone, Debug)]
pub struct SubConfigBuilder {
    relays: RelaySelection,
    live_filters: Vec<Filter>,
    full_history: Option<FullHistoryConfig>,
    routing_preference: RelayRoutingPreference,
}

impl SubConfig {
    /// Start a builder with a live subscription declaration.
    pub fn live(filters: Vec<Filter>) -> SubConfigBuilder {
        SubConfigBuilder {
            relays: RelaySelection::AccountsRead,
            live_filters: filters,
            full_history: None,
            routing_preference: RelayRoutingPreference::default(),
        }
    }

    /// Returns the live filter set for this subscription.
    pub(crate) fn live_filters(&self) -> &[Filter] {
        &self.live_filters
    }

    /// Returns the configured background full-history declaration.
    pub(crate) fn full_history_config(&self) -> Option<&FullHistoryConfig> {
        self.full_history.as_ref()
    }
}

impl SubConfigBuilder {
    /// Add or replace the full-history declaration.
    pub fn full_history(mut self, full_history: FullHistoryConfig) -> Self {
        self.full_history = Some(full_history);
        self
    }

    /// Use an explicit relay set for this subscription.
    pub fn explicit_relays(mut self, relays: HashSet<NormRelayUrl>) -> Self {
        self.relays = RelaySelection::Explicit(relays);
        self
    }

    /// Use one explicit relay for this subscription.
    pub fn explicit_relay(self, relay: NormRelayUrl) -> Self {
        let mut relays = HashSet::new();
        relays.insert(relay);
        self.explicit_relays(relays)
    }

    /// Override the relay routing preference for this subscription.
    pub fn routing_preference(mut self, routing_preference: RelayRoutingPreference) -> Self {
        self.routing_preference = routing_preference;
        self
    }

    /// Build a normalized scoped subscription config.
    pub fn build(self) -> SubConfig {
        let live_filters = normalize_filters(self.live_filters);
        assert!(
            !live_filters.is_empty(),
            "SubConfig requires at least one live filter"
        );
        let full_history = self
            .full_history
            .filter(|full_history| !full_history.is_empty());

        SubConfig {
            relays: self.relays,
            live_filters,
            full_history,
            routing_preference: self.routing_preference,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ScopedSubKey {
    scope: ResolvedSubScope,
    key: SubKey,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ResolvedSubScope {
    Account(Pubkey),
    Global,
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

/// Result of clearing one `(slot, key)` ownership link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearSubResult {
    Cleared,
    StillInUse,
    NotFound,
}

/// Result of dropping a whole slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropSlotResult {
    Dropped,
    NotFound,
}

/// Aggregate EOSE status for one live scoped subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopedSubLiveEoseStatus {
    /// Number of relay legs currently tracking this request.
    pub tracked_relays: usize,
    /// Whether any tracked relay has reached EOSE.
    pub any_eose: bool,
    /// Whether all tracked relays have reached EOSE.
    ///
    /// This is false when `tracked_relays == 0`.
    pub all_eosed: bool,
}

/// EOSE state for one owner-scoped logical subscription key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopedSubEoseStatus {
    /// No owned scoped subscription exists for the requested `(owner, key, scope)`.
    Missing,
    /// Owned desired state exists, but no live outbox subscription is active.
    ///
    /// This occurs for empty-filter specs and for account-scoped subs while switched away.
    Inactive,
    /// Live outbox subscription exists; aggregate EOSE state is available.
    Live(ScopedSubLiveEoseStatus),
}

/// Host-owned runtime for scoped subscription desired/live state and ownership.
///
/// The runtime never leaks outbox subscription ids to app code. Apps talk in
/// terms of identity + config and the runtime handles lifecycles, relay
/// mutations, and account-switch restore semantics.
///
/// `live` and `full_history` are separate outbox lifecycles. This runtime owns
/// their pairing by [`ScopedSubKey`] and removes both when the scoped sub is no
/// longer desired.
pub(crate) struct ScopedSubRuntime {
    desired: HashMap<ScopedSubKey, SubConfig>,
    live: HashMap<ScopedSubKey, OutboxSubId>,
    full_history: HashMap<ScopedSubKey, FullHistorySubId>,
    owners_by_sub: HashMap<ScopedSubKey, HashSet<SubSlotId>>,
    subs_by_slot: HashMap<SubSlotId, HashSet<ScopedSubKey>>,
    next_slot_id: u64,
}

impl Default for ScopedSubRuntime {
    fn default() -> Self {
        Self {
            desired: HashMap::default(),
            live: HashMap::default(),
            full_history: HashMap::default(),
            owners_by_sub: HashMap::default(),
            subs_by_slot: HashMap::default(),
            next_slot_id: 1,
        }
    }
}

impl ScopedSubRuntime {
    fn scoped_key(scope: ResolvedSubScope, key: SubKey) -> ScopedSubKey {
        ScopedSubKey { scope, key }
    }

    /// Create one owner slot for a UI lifecycle owner.
    pub(crate) fn create_slot(&mut self) -> SubSlotId {
        let slot = self.allocate_slot();
        self.subs_by_slot.entry(slot).or_default();
        slot
    }

    /// Internal upsert path using selected-account relay resolution.
    pub(crate) fn set_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
        slot: SubSlotId,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> SetSubResult {
        let account_read_relays = accounts.selected_account_read_relays();
        let selected_account_pubkey = *accounts.selected_account_pubkey();
        self.set_sub_with_relays(
            pool,
            &account_read_relays,
            selected_account_pubkey,
            slot,
            scope,
            key,
            config,
        )
    }

    /// Internal create-if-absent path using selected-account relay resolution.
    pub(crate) fn ensure_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
        slot: SubSlotId,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> EnsureSubResult {
        let account_read_relays = accounts.selected_account_read_relays();
        let selected_account_pubkey = *accounts.selected_account_pubkey();
        self.ensure_sub_with_relays(
            pool,
            &account_read_relays,
            selected_account_pubkey,
            slot,
            scope,
            key,
            config,
        )
    }

    /// Create desired state for one `(slot, key)` only if absent, with pre-resolved relays.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ensure_sub_with_relays(
        &mut self,
        pool: &mut Outbox<'_>,
        account_read_relays: &HashSet<NormRelayUrl>,
        selected_account_pubkey: Pubkey,
        slot: SubSlotId,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> EnsureSubResult {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        self.register_ownership(slot, &scoped);
        if self.desired.contains_key(&scoped) {
            return EnsureSubResult::AlreadyExists;
        }

        self.desired.insert(scoped.clone(), config.clone());
        self.apply_scoped_sub(pool, account_read_relays, &scoped, &config, false);
        EnsureSubResult::Created
    }

    /// Create-or-update desired state for one `(slot, key)` with pre-resolved relays.
    ///
    /// This is equivalent to [`Self::set_sub`] but avoids relay lookup from
    /// `Accounts` when the caller already has the selected relay set.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_sub_with_relays(
        &mut self,
        pool: &mut Outbox<'_>,
        account_read_relays: &HashSet<NormRelayUrl>,
        selected_account_pubkey: Pubkey,
        slot: SubSlotId,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> SetSubResult {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        self.register_ownership(slot, &scoped);

        let has_live = self.live.contains_key(&scoped);
        let has_full_history = self.full_history.contains_key(&scoped);

        if let Some(previous) = self.desired.get(&scoped) {
            if same_sub_config(previous, &config) {
                if needs_apply_sub(&config, has_live, has_full_history) {
                    self.apply_scoped_sub(pool, account_read_relays, &scoped, &config, false);
                    return SetSubResult::Updated;
                }

                return SetSubResult::Unchanged;
            }
        }

        let previous = self.desired.insert(scoped.clone(), config.clone());
        let replace_existing = previous.as_ref().is_some_and(|previous| {
            has_live && previous.routing_preference != config.routing_preference
        });

        if previous.is_none() {
            self.apply_scoped_sub(pool, account_read_relays, &scoped, &config, false);
            return SetSubResult::Created;
        }

        self.apply_scoped_sub(
            pool,
            account_read_relays,
            &scoped,
            &config,
            replace_existing,
        );

        SetSubResult::Updated
    }

    /// Clear one `(slot, key)` ownership link.
    pub(crate) fn clear_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
        slot: SubSlotId,
        key: SubKey,
        scope: SubScope,
    ) -> ClearSubResult {
        let selected_account_pubkey = *accounts.selected_account_pubkey();
        self.clear_sub_with_selected(pool, selected_account_pubkey, slot, key, scope)
    }

    /// Clear one `(slot, key)` with explicit selected account.
    pub(crate) fn clear_sub_with_selected(
        &mut self,
        pool: &mut Outbox<'_>,
        selected_account_pubkey: Pubkey,
        slot: SubSlotId,
        key: SubKey,
        scope: SubScope,
    ) -> ClearSubResult {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        let Some(slot_entries) = self.subs_by_slot.get_mut(&slot) else {
            return ClearSubResult::NotFound;
        };

        if !slot_entries.remove(&scoped) {
            return ClearSubResult::NotFound;
        }

        if slot_entries.is_empty() {
            self.subs_by_slot.remove(&slot);
        }

        self.release_slot_from_scoped_sub(pool, slot, &scoped)
    }

    /// Query aggregate EOSE state for one `(slot, key)` using the selected account scope.
    pub(crate) fn sub_eose_status(
        &self,
        pool: &Outbox<'_>,
        accounts: &Accounts,
        slot: SubSlotId,
        key: SubKey,
        scope: SubScope,
    ) -> ScopedSubEoseStatus {
        let selected_account_pubkey = *accounts.selected_account_pubkey();
        self.sub_eose_status_with_selected(pool, selected_account_pubkey, slot, key, scope)
    }

    /// Query aggregate EOSE state for one `(slot, key)` using an explicit selected account.
    pub(crate) fn sub_eose_status_with_selected(
        &self,
        pool: &Outbox<'_>,
        selected_account_pubkey: Pubkey,
        slot: SubSlotId,
        key: SubKey,
        scope: SubScope,
    ) -> ScopedSubEoseStatus {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        let Some(slot_entries) = self.subs_by_slot.get(&slot) else {
            return ScopedSubEoseStatus::Missing;
        };

        if !slot_entries.contains(&scoped) {
            return ScopedSubEoseStatus::Missing;
        }

        if let Some(live_id) = self.live.get(&scoped).copied() {
            let relay_statuses = pool.status(&live_id);
            return ScopedSubEoseStatus::Live(aggregate_eose_status(
                relay_statuses.values().copied(),
            ));
        }

        if self.desired.contains_key(&scoped) {
            ScopedSubEoseStatus::Inactive
        } else {
            ScopedSubEoseStatus::Missing
        }
    }

    /// Drop all ownership links attached to one slot.
    pub(crate) fn drop_slot(&mut self, pool: &mut Outbox<'_>, slot: SubSlotId) -> DropSlotResult {
        let Some(scoped_keys) = self.subs_by_slot.remove(&slot) else {
            return DropSlotResult::NotFound;
        };

        for scoped in scoped_keys {
            let _ = self.release_slot_from_scoped_sub(pool, slot, &scoped);
        }

        DropSlotResult::Dropped
    }

    /// Handle centralized account switching using host account relay resolution.
    pub fn on_account_switched(
        &mut self,
        pool: &mut Outbox<'_>,
        old_pk: Pubkey,
        new_pk: Pubkey,
        accounts: &Accounts,
    ) {
        let new_account_read_relays = accounts.selected_account_read_relays();
        self.on_account_switched_with_relays(pool, old_pk, new_pk, &new_account_read_relays);
    }

    /// Handle centralized account switching with pre-resolved new account relays.
    pub(crate) fn on_account_switched_with_relays(
        &mut self,
        pool: &mut Outbox<'_>,
        old_pk: Pubkey,
        new_pk: Pubkey,
        new_account_read_relays: &HashSet<NormRelayUrl>,
    ) {
        if old_pk == new_pk {
            return;
        }

        let old_scope = ResolvedSubScope::Account(old_pk);
        let new_scope = ResolvedSubScope::Account(new_pk);

        self.unsubscribe_scope(pool, &old_scope);

        let new_desired_keys =
            owned_desired_keys_for_scope(&self.desired, &self.owners_by_sub, &new_scope);

        for scoped in new_desired_keys {
            let Some(spec) = self.desired.get(&scoped).cloned() else {
                continue;
            };
            if matches!(spec.relays, RelaySelection::AccountsRead) {
                continue;
            }

            self.apply_scoped_sub(pool, new_account_read_relays, &scoped, &spec, false);
        }

        self.retarget_selected_account_read_relays_with_relays(
            pool,
            new_pk,
            new_account_read_relays,
        );
    }

    /// Retarget scoped subscriptions that depend on the selected account's read relay set.
    ///
    /// This updates all owned scoped subscriptions whose relay selection is
    /// [`RelaySelection::AccountsRead`] and whose resolved scope is either:
    /// - the currently selected account (`SubScope::Account` resolved), or
    /// - global (`SubScope::Global`)
    ///
    /// This is used when the selected account's kind `10002` relay list changes
    /// without switching accounts.
    pub fn retarget_selected_account_read_relays(
        &mut self,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
    ) {
        let selected_account_pubkey = *accounts.selected_account_pubkey();
        let account_read_relays = accounts.selected_account_read_relays();
        self.retarget_selected_account_read_relays_with_relays(
            pool,
            selected_account_pubkey,
            &account_read_relays,
        );
    }

    /// Retarget selected-account-dependent scoped subscriptions with pre-resolved read relays.
    pub(crate) fn retarget_selected_account_read_relays_with_relays(
        &mut self,
        pool: &mut Outbox<'_>,
        selected_account_pubkey: Pubkey,
        account_read_relays: &HashSet<NormRelayUrl>,
    ) {
        let account_scope = ResolvedSubScope::Account(selected_account_pubkey);
        let scoped_keys: Vec<_> = self
            .desired
            .keys()
            .filter(|scoped| {
                (scoped.scope == account_scope || scoped.scope == ResolvedSubScope::Global)
                    && has_owners(&self.owners_by_sub, scoped)
            })
            .cloned()
            .collect();

        for scoped in scoped_keys {
            let Some(spec) = self.desired.get(&scoped).cloned() else {
                continue;
            };

            if !matches!(spec.relays, RelaySelection::AccountsRead) {
                continue;
            }

            self.apply_scoped_sub(pool, account_read_relays, &scoped, &spec, false);
        }
    }

    fn register_ownership(&mut self, slot: SubSlotId, scoped: &ScopedSubKey) {
        self.subs_by_slot
            .entry(slot)
            .or_default()
            .insert(scoped.clone());
        self.owners_by_sub
            .entry(scoped.clone())
            .or_default()
            .insert(slot);
    }

    fn apply_scoped_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: &ScopedSubKey,
        spec: &SubConfig,
        replace_existing: bool,
    ) {
        let relays = resolve_relays(account_read_relays, &spec.relays);
        if relays.is_empty() {
            self.remove_scoped_sub(pool, scoped);
            return;
        }

        if replace_existing {
            self.remove_live_sub(pool, scoped);
        }

        let filters = spec.live_filters();
        if let Some(live_id) = self.live.get(scoped).copied() {
            pool.modify_full(live_id, filters.to_vec(), relays.clone());
        } else {
            let relay_pkgs = RelayUrlPkgs::with_preference(relays.clone(), spec.routing_preference);
            let live_id = pool.subscribe(filters.to_vec(), relay_pkgs);
            self.live.insert(scoped.clone(), live_id);
        }

        self.apply_full_history(pool, scoped, spec.full_history_config(), relays);
    }

    fn remove_live_sub(&mut self, pool: &mut Outbox<'_>, scoped: &ScopedSubKey) {
        if let Some(live_id) = self.live.remove(scoped) {
            pool.unsubscribe(live_id);
        }
    }

    fn apply_full_history(
        &mut self,
        pool: &mut Outbox<'_>,
        scoped: &ScopedSubKey,
        full_history: Option<&FullHistoryConfig>,
        relays: HashSet<NormRelayUrl>,
    ) {
        let Some(full_history) = full_history else {
            self.remove_full_history(pool, scoped);
            return;
        };

        if let Some(history_id) = self.full_history.get(scoped).copied() {
            pool.modify_full_history(history_id, full_history.clone(), relays);
            return;
        }

        let history_id = pool.subscribe_full_history(full_history.clone(), relays);
        self.full_history.insert(scoped.clone(), history_id);
    }

    fn remove_scoped_sub(&mut self, pool: &mut Outbox<'_>, scoped: &ScopedSubKey) {
        if let Some(live_id) = self.live.remove(scoped) {
            pool.unsubscribe(live_id);
        }
        self.remove_full_history(pool, scoped);
    }

    fn remove_full_history(&mut self, pool: &mut Outbox<'_>, scoped: &ScopedSubKey) {
        if let Some(history_id) = self.full_history.remove(scoped) {
            pool.remove_full_history(history_id);
        }
    }

    fn unsubscribe_scope(&mut self, pool: &mut Outbox<'_>, scope: &ResolvedSubScope) {
        let scoped_keys: HashSet<_> = self
            .live
            .keys()
            .chain(self.full_history.keys())
            .filter(|scoped| scoped.scope == *scope)
            .cloned()
            .collect();

        for scoped in scoped_keys {
            self.remove_scoped_sub(pool, &scoped);
        }
    }

    fn release_slot_from_scoped_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        slot: SubSlotId,
        scoped: &ScopedSubKey,
    ) -> ClearSubResult {
        let Some(owners) = self.owners_by_sub.get_mut(scoped) else {
            return ClearSubResult::NotFound;
        };

        if !owners.remove(&slot) {
            return ClearSubResult::NotFound;
        }

        if !owners.is_empty() {
            return ClearSubResult::StillInUse;
        }

        self.owners_by_sub.remove(scoped);
        self.desired.remove(scoped);
        self.remove_scoped_sub(pool, scoped);

        ClearSubResult::Cleared
    }

    fn allocate_slot(&mut self) -> SubSlotId {
        loop {
            if self.next_slot_id == 0 {
                self.next_slot_id = 1;
            }
            let slot = SubSlotId(self.next_slot_id);
            self.next_slot_id = self.next_slot_id.wrapping_add(1);
            if !self.subs_by_slot.contains_key(&slot) {
                return slot;
            }
        }
    }
}
#[cfg(test)]
impl ScopedSubRuntime {
    pub(crate) fn live_id_with_selected(
        &self,
        selected_account_pubkey: Pubkey,
        key: SubKey,
        scope: SubScope,
    ) -> Option<OutboxSubId> {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);
        self.live.get(&scoped).copied()
    }
}

fn needs_apply_sub(config: &SubConfig, has_live: bool, has_full_history: bool) -> bool {
    !has_live || config.full_history_config().is_some() && !has_full_history
}

fn owned_desired_keys_for_scope(
    desired: &HashMap<ScopedSubKey, SubConfig>,
    owners_by_sub: &HashMap<ScopedSubKey, HashSet<SubSlotId>>,
    scope: &ResolvedSubScope,
) -> Vec<ScopedSubKey> {
    desired
        .keys()
        .filter(|key| key.scope == *scope && has_owners(owners_by_sub, key))
        .cloned()
        .collect()
}

fn has_owners(
    owners_by_sub: &HashMap<ScopedSubKey, HashSet<SubSlotId>>,
    scoped: &ScopedSubKey,
) -> bool {
    owners_by_sub
        .get(scoped)
        .is_some_and(|owners| !owners.is_empty())
}

fn normalize_filters(filters: Vec<Filter>) -> Vec<Filter> {
    filters
        .into_iter()
        .filter(|filter| filter.num_elements() != 0)
        .collect()
}

/// Compare two `SubConfig` values, ignoring filter ordering but preserving
/// canonical filter semantics for live and full-history filters.
fn same_sub_config(previous: &SubConfig, next: &SubConfig) -> bool {
    previous.relays == next.relays
        && previous.routing_preference == next.routing_preference
        && same_canonical_filter_set(previous.live_filters(), next.live_filters())
        && same_full_history_config(previous.full_history_config(), next.full_history_config())
}

fn same_full_history_config(
    previous: Option<&FullHistoryConfig>,
    next: Option<&FullHistoryConfig>,
) -> bool {
    match (previous, next) {
        (Some(previous), Some(next)) => {
            same_canonical_filter_set(previous.filters(), next.filters())
        }
        (None, None) => true,
        _ => false,
    }
}

fn resolve_scope(scope: &SubScope, selected_account_pubkey: Pubkey) -> ResolvedSubScope {
    match scope {
        SubScope::Account => ResolvedSubScope::Account(selected_account_pubkey),
        SubScope::Global => ResolvedSubScope::Global,
    }
}

fn resolve_relays(
    account_read_relays: &HashSet<NormRelayUrl>,
    selection: &RelaySelection,
) -> HashSet<NormRelayUrl> {
    match selection {
        RelaySelection::AccountsRead => account_read_relays.clone(),
        RelaySelection::Explicit(relays) => relays.clone(),
    }
}

fn aggregate_eose_status(
    relay_statuses: impl IntoIterator<Item = RelayReqStatus>,
) -> ScopedSubLiveEoseStatus {
    let mut tracked_relays = 0usize;
    let mut any_eose = false;
    let mut all_eosed = true;

    for status in relay_statuses {
        tracked_relays += 1;
        if status == RelayReqStatus::Eose {
            any_eose = true;
        } else {
            all_eosed = false;
        }
    }

    if tracked_relays == 0 {
        all_eosed = false;
    }

    ScopedSubLiveEoseStatus {
        tracked_relays,
        any_eose,
        all_eosed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EguiWakeup;
    use enostr::{OutboxPool, OutboxSessionHandler};
    use std::hash::Hash;

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    enum FakeApp {
        Timelines,
        Threads,
        Messages,
    }

    fn live_config() -> SubConfig {
        SubConfig::live(vec![Filter::new().kinds(vec![1]).limit(5).build()]).build()
    }

    fn live_config_with_filters(filters: Vec<Filter>) -> SubConfig {
        SubConfig::live(filters).build()
    }

    fn live_full_history_config(
        live_filters: Vec<Filter>,
        history_filters: Vec<Filter>,
    ) -> SubConfig {
        SubConfig::live(live_filters)
            .full_history(FullHistoryConfig::new(history_filters))
            .build()
    }

    fn full_history_config() -> SubConfig {
        let filters = vec![Filter::new().kinds(vec![1]).limit(5).build()];
        live_full_history_config(filters.clone(), filters)
    }

    fn account_pk(tag: u8) -> Pubkey {
        Pubkey::new([tag; 32])
    }

    fn make_key(parts: impl Hash) -> SubKey {
        SubKey::new(parts)
    }

    fn relay_set(url: &str) -> HashSet<NormRelayUrl> {
        let mut relays = HashSet::new();
        relays.insert(NormRelayUrl::new(url).unwrap());
        relays
    }

    fn accountsread_spec(kind: u64, limit: u64) -> SubConfig {
        SubConfig::live(vec![Filter::new().kinds(vec![kind]).limit(limit).build()]).build()
    }

    fn explicit_account_spec() -> SubConfig {
        let explicit_relay = NormRelayUrl::new("wss://relay-explicit.example.com").unwrap();
        SubConfig::live(vec![Filter::new().kinds(vec![10002]).limit(1).build()])
            .explicit_relays({
                let mut set = HashSet::new();
                set.insert(explicit_relay);
                set
            })
            .build()
    }

    // Dropping this handler can build `WebsocketConn` through relay coordination;
    // keep callers under `#[tokio::test]` when the ewebsock Tokio backend is used.
    fn outbox<'a>(pool: &'a mut OutboxPool) -> Outbox<'a> {
        OutboxSessionHandler::new(pool, EguiWakeup::new(egui::Context::default()))
    }

    fn slot_status(
        runtime: &ScopedSubRuntime,
        pool: &mut OutboxPool,
        selected_account_pubkey: Pubkey,
        slot: SubSlotId,
        key: SubKey,
        scope: SubScope,
    ) -> ScopedSubEoseStatus {
        let outbox = outbox(pool);
        runtime.sub_eose_status_with_selected(&outbox, selected_account_pubkey, slot, key, scope)
    }
    #[tokio::test]
    async fn set_sub_is_upsert_for_existing_key() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = relay_set("wss://relay-a.example.com");
        let key = SubKey::new(("messages", "dm-list", 7u8));
        let scope = SubScope::Global;
        let slot = runtime.create_slot();
        let config = live_config();

        let first = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            scope,
            key,
            config.clone(),
        );
        let second = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            scope,
            key,
            config,
        );

        assert!(matches!(first, SetSubResult::Created));
        assert!(matches!(second, SetSubResult::Unchanged));
        assert_eq!(runtime.desired.len(), 1);
        assert_eq!(runtime.live.len(), 1);
        assert_eq!(runtime.subs_by_slot.len(), 1);
    }
    #[tokio::test]
    async fn ensure_sub_is_create_or_ignore_for_existing_key() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = relay_set("wss://relay-a.example.com");
        let key = SubKey::new(("messages", "dm-list", 9u8));
        let slot = runtime.create_slot();

        let first = runtime.ensure_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            live_config(),
        );

        let second = runtime.ensure_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            live_config(),
        );

        assert!(matches!(first, EnsureSubResult::Created));
        assert!(matches!(second, EnsureSubResult::AlreadyExists));
        assert_eq!(runtime.desired.len(), 1);
        assert_eq!(runtime.live.len(), 1);
        assert_eq!(runtime.subs_by_slot.len(), 1);
    }
    #[tokio::test]
    async fn ensure_sub_does_not_modify_existing_live_sub() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = relay_set("wss://relay-a.example.com");
        let key = SubKey::new(("timeline", "home", 1u8));
        let slot = runtime.create_slot();

        let initial =
            live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(10).build()]);

        let created = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            initial,
        );
        assert!(matches!(created, SetSubResult::Created));

        let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
        let live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        let before = pool
            .filters(&live_id)
            .expect("stored filters before ensure")
            .iter()
            .map(|f| f.json().expect("filter json"))
            .collect::<Vec<_>>();

        let replacement =
            live_config_with_filters(vec![Filter::new().kinds(vec![3]).limit(1).build()]);
        let ensured = runtime.ensure_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            replacement,
        );
        assert!(matches!(ensured, EnsureSubResult::AlreadyExists));

        let after = pool
            .filters(&live_id)
            .expect("stored filters after ensure")
            .iter()
            .map(|f| f.json().expect("filter json"))
            .collect::<Vec<_>>();
        assert_eq!(before, after);
    }
    #[tokio::test]
    async fn aggregate_eose_status_zero_tracked_relays_is_not_all_eosed() {
        let status = aggregate_eose_status(std::iter::empty());
        assert_eq!(
            status,
            ScopedSubLiveEoseStatus {
                tracked_relays: 0,
                any_eose: false,
                all_eosed: false,
            }
        );
    }
    #[tokio::test]
    async fn aggregate_eose_status_mixed_relays_reports_partial_eose() {
        let status = aggregate_eose_status([
            RelayReqStatus::InitialQuery,
            RelayReqStatus::Eose,
            RelayReqStatus::Closed,
        ]);
        assert_eq!(
            status,
            ScopedSubLiveEoseStatus {
                tracked_relays: 3,
                any_eose: true,
                all_eosed: false,
            }
        );
    }
    #[tokio::test]
    async fn aggregate_eose_status_all_relays_eose_reports_all_eosed() {
        let status = aggregate_eose_status([RelayReqStatus::Eose, RelayReqStatus::Eose]);
        assert_eq!(
            status,
            ScopedSubLiveEoseStatus {
                tracked_relays: 2,
                any_eose: true,
                all_eosed: true,
            }
        );
    }
    #[tokio::test]
    async fn sub_eose_status_missing_when_slot_does_not_own_key() {
        let runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let status = slot_status(
            &runtime,
            &mut pool,
            account_pk(0x01),
            SubSlotId(999),
            make_key(("missing", 1u8)),
            SubScope::Global,
        );
        assert_eq!(status, ScopedSubEoseStatus::Missing);
    }
    #[tokio::test]
    async fn sub_eose_status_inactive_for_desired_without_live_sub() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = HashSet::new();
        let slot = runtime.create_slot();
        let key = make_key(("inactive", 1u8));
        let selected = account_pk(0x01);

        let _ = runtime.ensure_sub_with_relays(
            &mut outbox(&mut pool),
            &relays,
            selected,
            slot,
            SubScope::Global,
            key,
            live_config(),
        );

        let status = slot_status(&runtime, &mut pool, selected, slot, key, SubScope::Global);
        assert_eq!(status, ScopedSubEoseStatus::Inactive);
    }
    #[tokio::test]
    async fn sub_eose_status_live_reports_tracked_relays_and_eose_flags() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = relay_set("wss://relay-a.example.com");
        let slot = runtime.create_slot();
        let key = make_key(("live", 1u8));
        let selected = account_pk(0x01);

        let _ = runtime.set_sub_with_relays(
            &mut outbox(&mut pool),
            &relays,
            selected,
            slot,
            SubScope::Global,
            key,
            live_config(),
        );

        let status = slot_status(&runtime, &mut pool, selected, slot, key, SubScope::Global);
        let ScopedSubEoseStatus::Live(live) = status else {
            panic!("expected live status, got {status:?}");
        };

        assert_eq!(live.tracked_relays, 1);
        assert!(!live.any_eose);
        assert!(!live.all_eosed);
    }
    #[tokio::test]
    async fn account_scoped_sub_eose_status_transitions_inactive_and_restores_on_switch_back() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays_a = relay_set("wss://relay-a.example.com");
        let relays_b = relay_set("wss://relay-b.example.com");
        let account_a = account_pk(0x0A);
        let account_b = account_pk(0x0B);
        let slot = runtime.create_slot();
        let key = make_key(("account-scoped", 1u8));

        let _ = runtime.set_sub_with_relays(
            &mut outbox(&mut pool),
            &relays_a,
            account_a,
            slot,
            SubScope::Account,
            key,
            live_config(),
        );

        let before = slot_status(&runtime, &mut pool, account_a, slot, key, SubScope::Account);
        assert!(matches!(before, ScopedSubEoseStatus::Live(_)));

        runtime.on_account_switched_with_relays(
            &mut outbox(&mut pool),
            account_a,
            account_b,
            &relays_b,
        );

        let old_while_switched =
            slot_status(&runtime, &mut pool, account_a, slot, key, SubScope::Account);
        assert_eq!(old_while_switched, ScopedSubEoseStatus::Inactive);

        let new_missing = slot_status(&runtime, &mut pool, account_b, slot, key, SubScope::Account);
        assert_eq!(new_missing, ScopedSubEoseStatus::Missing);

        runtime.on_account_switched_with_relays(
            &mut outbox(&mut pool),
            account_b,
            account_a,
            &relays_a,
        );

        let restored = slot_status(&runtime, &mut pool, account_a, slot, key, SubScope::Account);
        assert!(matches!(restored, ScopedSubEoseStatus::Live(_)));
    }
    #[tokio::test]
    async fn set_sub_upsert_modifies_live_sub() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let key = SubKey::new(("timeline", 1u64));
        let scope = SubScope::Global;
        let relays_a = relay_set("wss://relay-a.example.com");
        let relays_b = relay_set("wss://relay-b.example.com");
        let slot = runtime.create_slot();

        let spec = live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

        let first = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_a,
            account_pk(0x01),
            slot,
            scope,
            key,
            spec.clone(),
        );
        assert!(matches!(first, SetSubResult::Created));

        let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
        let live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        assert_eq!(pool.filters(&live_id).expect("stored filters").len(), 1);

        let updated = live_config_with_filters(vec![Filter::new().kinds(vec![3]).limit(1).build()]);

        let res = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_b,
            account_pk(0x01),
            slot,
            scope,
            key,
            updated.clone(),
        );
        assert!(matches!(res, SetSubResult::Updated));

        assert_eq!(
            pool.filters(&live_id)
                .expect("updated filters should exist")
                .len(),
            1
        );

        let transparent_update =
            SubConfig::live(vec![Filter::new().kinds(vec![3]).limit(1).build()])
                .routing_preference(RelayRoutingPreference::RequireDedicated)
                .build();

        let res = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_b,
            account_pk(0x01),
            slot,
            scope,
            key,
            transparent_update,
        );
        assert!(matches!(res, SetSubResult::Updated));

        let new_live_id = runtime.live.get(&scoped).copied().expect("replacement id");
        assert_ne!(live_id, new_live_id);
        assert!(pool.filters(&live_id).is_none());
    }
    #[tokio::test]
    async fn set_sub_is_unchanged_when_live_config_matches_existing_state() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let key = SubKey::new(("timeline", 2u64));
        let scope = SubScope::Global;
        let relays = relay_set("wss://relay-a.example.com");
        let slot = runtime.create_slot();

        let spec = live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

        let created = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            scope,
            key,
            spec.clone(),
        );
        assert!(matches!(created, SetSubResult::Created));

        let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
        let live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        let before = pool
            .filters(&live_id)
            .expect("stored filters before no-op")
            .iter()
            .map(|filter| filter.json().expect("filter json"))
            .collect::<Vec<_>>();

        let unchanged = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            scope,
            key,
            spec,
        );
        assert!(matches!(unchanged, SetSubResult::Unchanged));

        let after_live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        let after = pool
            .filters(&after_live_id)
            .expect("stored filters after no-op")
            .iter()
            .map(|filter| filter.json().expect("filter json"))
            .collect::<Vec<_>>();

        assert_eq!(live_id, after_live_id);
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn sub_config_declares_live_and_full_history_explicitly() {
        let live_filter = Filter::new().kinds(vec![1]).limit(500).build();
        let history_filter = Filter::new().kinds(vec![1]).since(123).build();
        let config = SubConfig::live(vec![live_filter.clone()])
            .full_history(FullHistoryConfig::new(vec![history_filter.clone()]))
            .build();

        let live_filters = config.live_filters();
        assert_eq!(live_filters.len(), 1);
        assert!(live_filters[0].same_canonical_attributes(&live_filter));
        let full_history = config.full_history_config().expect("full-history config");
        assert_eq!(full_history.filters().len(), 1);
        assert!(full_history.filters()[0].same_canonical_attributes(&history_filter));
    }

    #[test]
    fn sub_config_builder_constructs_live_declarations() {
        let live_filter = Filter::new().kinds(vec![1]).limit(500).build();
        let history_filter = Filter::new().kinds(vec![1]).since(123).build();

        let live = SubConfig::live(vec![live_filter.clone()]).build();
        assert_eq!(live.live_filters().len(), 1);
        assert!(live.full_history_config().is_none());

        let paired = SubConfig::live(vec![live_filter])
            .full_history(FullHistoryConfig::new(vec![history_filter]))
            .build();
        assert!(!paired.live_filters().is_empty());
        assert!(paired.full_history_config().is_some());
    }

    #[test]
    #[should_panic(expected = "SubConfig requires at least one live filter")]
    fn sub_config_builder_rejects_empty_live_declaration() {
        let _ = SubConfig::live(Vec::new()).build();
    }

    #[tokio::test]
    async fn clear_sub_unsubscribes_live_subscription() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let key = SubKey::new(("timeline", 1u64));
        let relays = relay_set("wss://relay-a.example.com");
        let slot = runtime.create_slot();

        let spec = live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

        runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            spec,
        );

        let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
        let live_id = runtime.live.get(&scoped).copied().expect("live sub id");

        assert!(matches!(
            runtime.clear_sub_with_selected(
                &mut OutboxSessionHandler::new(
                    &mut pool,
                    EguiWakeup::new(egui::Context::default())
                ),
                account_pk(0x01),
                slot,
                key,
                SubScope::Global
            ),
            ClearSubResult::Cleared
        ));

        assert_eq!(runtime.desired.len(), 0);
        assert_eq!(runtime.live.len(), 0);
        assert_eq!(runtime.subs_by_slot.len(), 0);
        assert!(pool.filters(&live_id).is_none());

        assert!(matches!(
            runtime.clear_sub_with_selected(
                &mut OutboxSessionHandler::new(
                    &mut pool,
                    EguiWakeup::new(egui::Context::default())
                ),
                account_pk(0x01),
                slot,
                key,
                SubScope::Global
            ),
            ClearSubResult::NotFound
        ));
    }
    #[tokio::test]
    async fn multiple_slots_share_single_live_sub_until_last_clear() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = relay_set("wss://relay-a.example.com");
        let account = account_pk(0x33);
        let key = SubKey::new(("thread", [9u8; 32]));

        let spec = live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(25).build()]);

        let slot_a = runtime.create_slot();
        let slot_b = runtime.create_slot();

        let a = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account,
            slot_a,
            SubScope::Account,
            key,
            spec.clone(),
        );
        let b = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account,
            slot_b,
            SubScope::Account,
            key,
            spec,
        );

        assert!(matches!(a, SetSubResult::Created));
        assert!(matches!(b, SetSubResult::Unchanged));

        let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account), key);
        let live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        assert_eq!(runtime.desired.len(), 1);
        assert_eq!(runtime.live.len(), 1);
        assert_eq!(runtime.subs_by_slot.len(), 2);
        assert!(pool.filters(&live_id).is_some());

        assert!(matches!(
            runtime.clear_sub_with_selected(
                &mut OutboxSessionHandler::new(
                    &mut pool,
                    EguiWakeup::new(egui::Context::default())
                ),
                account,
                slot_a,
                key,
                SubScope::Account
            ),
            ClearSubResult::StillInUse
        ));

        assert_eq!(runtime.desired.len(), 1);
        assert_eq!(runtime.live.len(), 1);
        assert_eq!(runtime.subs_by_slot.len(), 1);
        assert!(pool.filters(&live_id).is_some());

        assert!(matches!(
            runtime.clear_sub_with_selected(
                &mut OutboxSessionHandler::new(
                    &mut pool,
                    EguiWakeup::new(egui::Context::default())
                ),
                account,
                slot_b,
                key,
                SubScope::Account
            ),
            ClearSubResult::Cleared
        ));

        assert_eq!(runtime.desired.len(), 0);
        assert_eq!(runtime.live.len(), 0);
        assert_eq!(runtime.subs_by_slot.len(), 0);
        assert!(pool.filters(&live_id).is_none());
    }
    #[tokio::test]
    async fn drop_slot_clears_all_owned_subs() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let account = account_pk(0x4A);
        let relays = relay_set("wss://relay-a.example.com");
        let slot = runtime.create_slot();

        let key_account = SubKey::new(("timeline", "home"));
        let key_global = SubKey::new(("global", "discovery"));

        let account_spec =
            live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(5).build()]);

        let global_spec =
            live_config_with_filters(vec![Filter::new().kinds(vec![0]).limit(5).build()]);

        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account,
            slot,
            SubScope::Account,
            key_account,
            account_spec,
        );
        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays,
            account,
            slot,
            SubScope::Global,
            key_global,
            global_spec,
        );

        assert_eq!(runtime.desired.len(), 2);
        assert_eq!(runtime.live.len(), 2);
        assert_eq!(runtime.subs_by_slot.len(), 1);

        assert!(matches!(
            runtime.drop_slot(
                &mut OutboxSessionHandler::new(
                    &mut pool,
                    EguiWakeup::new(egui::Context::default())
                ),
                slot
            ),
            DropSlotResult::Dropped
        ));

        assert_eq!(runtime.desired.len(), 0);
        assert_eq!(runtime.live.len(), 0);
        assert_eq!(runtime.subs_by_slot.len(), 0);

        assert!(matches!(
            runtime.drop_slot(
                &mut OutboxSessionHandler::new(
                    &mut pool,
                    EguiWakeup::new(egui::Context::default())
                ),
                slot
            ),
            DropSlotResult::NotFound
        ));
    }
    #[tokio::test]
    async fn account_switch_unsubscribes_old_scope_and_restores_new_scope() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let account_a = account_pk(0xAA);
        let account_b = account_pk(0xBB);
        let relays_a = relay_set("wss://relay-a.example.com");
        let relays_b = relay_set("wss://relay-b.example.com");
        let key = SubKey::new(("timeline", "account-scoped"));
        let slot = runtime.create_slot();

        let scoped_spec =
            live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(2).build()]);

        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_a,
            account_a,
            slot,
            SubScope::Account,
            key,
            scoped_spec,
        );

        let scoped_a = ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key);
        let initial_live_id = runtime.live.get(&scoped_a).copied().expect("live id for A");
        assert!(pool.filters(&initial_live_id).is_some());

        runtime.on_account_switched_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            account_a,
            account_b,
            &relays_b,
        );

        assert!(runtime.live.get(&scoped_a).is_none());
        assert!(pool.filters(&initial_live_id).is_none());
        assert_eq!(runtime.desired.len(), 1);

        runtime.on_account_switched_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            account_b,
            account_a,
            &relays_a,
        );

        let restored_live_id = runtime
            .live
            .get(&scoped_a)
            .copied()
            .expect("account A should be restored on switch back");
        assert!(pool.filters(&restored_live_id).is_some());
    }
    #[tokio::test]
    async fn account_switch_contract_with_multiple_apps_and_mixed_scopes() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let account_a = account_pk(0xA1);
        let account_b = account_pk(0xB2);
        let peer_pk = account_pk(0xCC);

        let relays_a = relay_set("wss://relay-a.example.com");
        let relays_b = relay_set("wss://relay-b.example.com");
        let explicit_relay = NormRelayUrl::new("wss://relay-explicit.example.com").expect("relay");

        let key_timeline_a = make_key((FakeApp::Timelines, "home", 1u64, account_a));
        let key_thread_a = make_key((FakeApp::Threads, "root", [7u8; 32], account_a));
        let key_messages_a = make_key((FakeApp::Messages, "dm-relay-list", peer_pk, account_a));
        let key_global = make_key((FakeApp::Timelines, "global-discovery", 99u64));

        let timeline_spec_a =
            live_config_with_filters(vec![Filter::new().kinds(vec![1]).limit(50).build()]);

        let thread_spec_a = SubConfig::live(vec![Filter::new().kinds(vec![1]).limit(200).build()])
            .routing_preference(RelayRoutingPreference::RequireDedicated)
            .build();

        let messages_spec_a =
            SubConfig::live(vec![Filter::new().kinds(vec![10002]).limit(20).build()])
                .explicit_relays({
                    let mut set = HashSet::new();
                    set.insert(explicit_relay.clone());
                    set
                })
                .build();

        let global_spec = SubConfig::live(vec![Filter::new().kinds(vec![0]).limit(10).build()])
            .explicit_relays({
                let mut set = HashSet::new();
                set.insert(explicit_relay.clone());
                set
            })
            .build();

        let slot_timeline = runtime.create_slot();
        let slot_thread = runtime.create_slot();
        let slot_messages = runtime.create_slot();
        let slot_global = runtime.create_slot();

        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_a,
            account_a,
            slot_timeline,
            SubScope::Account,
            key_timeline_a,
            timeline_spec_a,
        );
        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_a,
            account_a,
            slot_thread,
            SubScope::Account,
            key_thread_a,
            thread_spec_a,
        );
        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_a,
            account_a,
            slot_messages,
            SubScope::Account,
            key_messages_a,
            messages_spec_a,
        );
        let _ = runtime.set_sub_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            &relays_a,
            account_a,
            slot_global,
            SubScope::Global,
            key_global,
            global_spec,
        );

        let scoped_timeline_a =
            ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_timeline_a);
        let scoped_thread_a =
            ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_thread_a);
        let scoped_messages_a =
            ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(account_a), key_messages_a);
        let scoped_global = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key_global);

        let timeline_id_a = runtime
            .live
            .get(&scoped_timeline_a)
            .copied()
            .expect("timeline A live");
        let thread_id_a = runtime
            .live
            .get(&scoped_thread_a)
            .copied()
            .expect("thread A live");
        let messages_id_a = runtime
            .live
            .get(&scoped_messages_a)
            .copied()
            .expect("messages A live");
        let global_id = runtime
            .live
            .get(&scoped_global)
            .copied()
            .expect("global live");

        assert!(pool.filters(&timeline_id_a).is_some());
        assert!(pool.filters(&thread_id_a).is_some());
        assert!(pool.filters(&messages_id_a).is_some());
        assert!(pool.filters(&global_id).is_some());

        runtime.on_account_switched_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            account_a,
            account_b,
            &relays_b,
        );

        assert!(
            runtime.live.get(&scoped_timeline_a).is_none()
                && runtime.live.get(&scoped_thread_a).is_none()
                && runtime.live.get(&scoped_messages_a).is_none()
        );
        assert!(
            pool.filters(&timeline_id_a).is_none()
                && pool.filters(&thread_id_a).is_none()
                && pool.filters(&messages_id_a).is_none()
        );
        assert!(runtime.live.get(&scoped_global).is_some() && pool.filters(&global_id).is_some());
        assert_eq!(runtime.desired.len(), 4);

        runtime.on_account_switched_with_relays(
            &mut OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default())),
            account_b,
            account_a,
            &relays_a,
        );

        let restored_timeline_id = runtime
            .live
            .get(&scoped_timeline_a)
            .copied()
            .expect("timeline A restored");
        let restored_thread_id = runtime
            .live
            .get(&scoped_thread_a)
            .copied()
            .expect("thread A restored");
        let restored_messages_id = runtime
            .live
            .get(&scoped_messages_a)
            .copied()
            .expect("messages A restored");

        assert!(pool.filters(&restored_timeline_id).is_some());
        assert!(pool.filters(&restored_thread_id).is_some());
        assert!(pool.filters(&restored_messages_id).is_some());
    }

    #[derive(Clone)]
    struct SubmittedSub {
        scoped: ScopedSubKey,
        live_id: OutboxSubId,
        history_id: Option<FullHistorySubId>,
    }

    // Scenario harness for selected-account read-relay retarget tests.
    // Keep this narrow; it is intentionally not a generic scoped-subs fixture.
    struct RetargetReadRelaysTest {
        runtime: ScopedSubRuntime,
        pool: OutboxPool,
        selected_account: Pubkey,
        other_account: Pubkey,
        relay_a: HashSet<NormRelayUrl>,
        relay_b: HashSet<NormRelayUrl>,
    }

    impl RetargetReadRelaysTest {
        fn new() -> Self {
            Self {
                runtime: ScopedSubRuntime::default(),
                pool: OutboxPool::default(),
                selected_account: account_pk(0xA1),
                other_account: account_pk(0xB2),
                relay_a: relay_set("wss://relay-a.example.com"),
                relay_b: relay_set("wss://relay-b.example.com"),
            }
        }

        fn submit_accountsread_account_home(&mut self) -> SubmittedSub {
            self.submit_sub(
                SubScope::Account,
                make_key((FakeApp::Timelines, "home", 1u64)),
                accountsread_spec(1, 50),
            )
        }

        fn submit_accountsread_global_feed(&mut self) -> SubmittedSub {
            self.submit_sub(
                SubScope::Global,
                make_key((FakeApp::Timelines, "global-ish", 2u64)),
                accountsread_spec(0, 10),
            )
        }

        fn submit_account_explicit_messages(&mut self) -> SubmittedSub {
            self.submit_sub(
                SubScope::Account,
                make_key((FakeApp::Messages, "explicit", 3u64)),
                explicit_account_spec(),
            )
        }

        fn submit_accountsread_other_account_home(&mut self) -> SubmittedSub {
            self.submit_sub_for_account(
                self.other_account,
                SubScope::Account,
                make_key((FakeApp::Timelines, "home", 99u64)),
                accountsread_spec(1, 25),
            )
        }

        fn submit_sub(&mut self, scope: SubScope, key: SubKey, spec: SubConfig) -> SubmittedSub {
            self.submit_sub_for_account(self.selected_account, scope, key, spec)
        }

        fn submit_sub_for_account(
            &mut self,
            account: Pubkey,
            scope: SubScope,
            key: SubKey,
            spec: SubConfig,
        ) -> SubmittedSub {
            let expects_full_history = spec.full_history_config().is_some();
            let slot = self.runtime.create_slot();
            let _ = self.runtime.set_sub_with_relays(
                &mut outbox(&mut self.pool),
                &self.relay_a,
                account,
                slot,
                scope,
                key,
                spec,
            );

            let resolved_scope = match scope {
                SubScope::Account => ResolvedSubScope::Account(account),
                SubScope::Global => ResolvedSubScope::Global,
            };
            let scoped = ScopedSubRuntime::scoped_key(resolved_scope, key);
            let live_id = self.runtime.live.get(&scoped).copied().unwrap();
            let history_id = self.runtime.full_history.get(&scoped).copied();
            assert_eq!(history_id.is_some(), expects_full_history);

            SubmittedSub {
                scoped,
                live_id,
                history_id,
            }
        }

        fn retarget_to_relay_b(&mut self) {
            self.runtime
                .retarget_selected_account_read_relays_with_relays(
                    &mut outbox(&mut self.pool),
                    self.selected_account,
                    &self.relay_b,
                );
        }

        fn retarget_to_empty_relays(&mut self) {
            self.runtime
                .retarget_selected_account_read_relays_with_relays(
                    &mut outbox(&mut self.pool),
                    self.selected_account,
                    &HashSet::new(),
                );
        }

        fn assert_live_id_unchanged(&self, sub: &SubmittedSub) {
            assert_eq!(self.runtime.live.get(&sub.scoped), Some(&sub.live_id));
        }

        fn assert_history_id_unchanged(&self, sub: &SubmittedSub) {
            if let Some(history_id) = sub.history_id {
                assert_eq!(
                    self.runtime.full_history.get(&sub.scoped),
                    Some(&history_id)
                );
            }
        }

        fn assert_still_live(&self, sub: &SubmittedSub) {
            assert!(self.pool.filters(&sub.live_id).is_some());
        }

        fn assert_live_relays(&self, sub: &SubmittedSub, expected: &HashSet<NormRelayUrl>) {
            let actual = self
                .pool
                .status(&sub.live_id)
                .keys()
                .map(|relay| (*relay).clone())
                .collect::<HashSet<_>>();
            assert_eq!(actual, *expected);
        }

        fn switch_selected_account_away(&mut self) {
            self.runtime.on_account_switched_with_relays(
                &mut outbox(&mut self.pool),
                self.selected_account,
                self.other_account,
                &self.relay_b,
            );
        }

        fn assert_not_live(&self, sub: &SubmittedSub) {
            assert!(self.runtime.live.get(&sub.scoped).is_none());
            assert!(self.runtime.full_history.get(&sub.scoped).is_none());
            assert!(self.pool.filters(&sub.live_id).is_none());
        }

        fn assert_live_recreated(&self, sub: &SubmittedSub) {
            let recreated_live_id = self.runtime.live.get(&sub.scoped).copied().unwrap();
            assert_ne!(recreated_live_id, sub.live_id);
            assert!(self.pool.filters(&recreated_live_id).is_some());
            assert!(self.pool.filters(&sub.live_id).is_none());

            if let Some(history_id) = sub.history_id {
                let recreated_history_id = self.runtime.full_history.get(&sub.scoped).copied();
                assert!(recreated_history_id.is_some());
                assert_ne!(recreated_history_id, Some(history_id));
            }
        }
    }
    #[tokio::test]
    async fn selected_account_relay_refresh_updates_account_and_global_accountsread_subs() {
        let mut t = RetargetReadRelaysTest::new();

        let account_home = t.submit_accountsread_account_home();
        let global_feed = t.submit_accountsread_global_feed();
        let explicit_messages = t.submit_account_explicit_messages();

        t.retarget_to_relay_b();

        t.assert_live_id_unchanged(&account_home);
        t.assert_live_id_unchanged(&global_feed);
        t.assert_live_id_unchanged(&explicit_messages);

        t.assert_still_live(&account_home);
        t.assert_still_live(&global_feed);
        t.assert_still_live(&explicit_messages);
    }
    #[tokio::test]
    async fn selected_account_relay_refresh_keeps_full_history_live_id_without_recreating_sub() {
        let mut t = RetargetReadRelaysTest::new();

        let full_history_home = t.submit_sub(
            SubScope::Account,
            make_key((FakeApp::Timelines, "full-history-home", 7u64)),
            full_history_config(),
        );
        t.retarget_to_relay_b();

        t.assert_live_id_unchanged(&full_history_home);
        t.assert_history_id_unchanged(&full_history_home);
        t.assert_still_live(&full_history_home);
    }
    #[tokio::test]
    async fn selected_account_relay_empty_then_non_empty_recreates_full_history_sub() {
        let mut t = RetargetReadRelaysTest::new();

        let full_history_home = t.submit_sub(
            SubScope::Account,
            make_key((FakeApp::Timelines, "full-history-home", 10u64)),
            full_history_config(),
        );
        t.retarget_to_empty_relays();

        assert!(t.runtime.live.get(&full_history_home.scoped).is_none());
        assert!(t
            .runtime
            .full_history
            .get(&full_history_home.scoped)
            .is_none());
        assert!(t.pool.filters(&full_history_home.live_id).is_none());
        assert_eq!(t.runtime.desired.len(), 1);

        t.retarget_to_relay_b();

        t.assert_live_recreated(&full_history_home);
    }
    #[tokio::test]
    async fn selected_account_relay_retarget_recreates_missing_live_sub() {
        let mut t = RetargetReadRelaysTest::new();

        let account_home = t.submit_accountsread_account_home();
        t.switch_selected_account_away();
        t.assert_not_live(&account_home);

        t.retarget_to_relay_b();

        t.assert_live_recreated(&account_home);
    }
    #[tokio::test]
    async fn selected_account_relay_retarget_recreates_missing_full_history_sub() {
        let mut t = RetargetReadRelaysTest::new();

        let full_history_home = t.submit_sub(
            SubScope::Account,
            make_key((FakeApp::Timelines, "full-history-home", 8u64)),
            full_history_config(),
        );
        t.switch_selected_account_away();
        t.assert_not_live(&full_history_home);

        t.retarget_to_relay_b();

        let recreated_live_id = t
            .runtime
            .live
            .get(&full_history_home.scoped)
            .copied()
            .expect("recreated live id");
        assert_ne!(recreated_live_id, full_history_home.live_id);
        let recreated_history_id = t
            .runtime
            .full_history
            .get(&full_history_home.scoped)
            .copied()
            .expect("recreated full-history id");
        assert_ne!(Some(recreated_history_id), full_history_home.history_id);
        assert!(t.pool.filters(&recreated_live_id).is_some());
        assert!(t.pool.filters(&full_history_home.live_id).is_none());
    }
    #[tokio::test]
    async fn selected_account_relay_retarget_ignores_other_account_scoped_subs() {
        let mut t = RetargetReadRelaysTest::new();

        let selected_account_home = t.submit_accountsread_account_home();
        let other_account_home = t.submit_accountsread_other_account_home();

        t.retarget_to_relay_b();

        t.assert_live_id_unchanged(&selected_account_home);
        t.assert_live_id_unchanged(&other_account_home);
        t.assert_still_live(&selected_account_home);
        t.assert_still_live(&other_account_home);
    }

    #[tokio::test]
    async fn account_switch_retargets_global_accountsread_subs() {
        let mut t = RetargetReadRelaysTest::new();
        let global_feed = t.submit_accountsread_global_feed();
        let relay_a = t.relay_a.clone();
        let relay_b = t.relay_b.clone();

        t.assert_live_relays(&global_feed, &relay_a);
        t.switch_selected_account_away();

        t.assert_live_id_unchanged(&global_feed);
        t.assert_live_relays(&global_feed, &relay_b);
    }
    #[tokio::test]
    async fn subkey_builder_is_stable_and_typed() {
        let key_a = SubKey::builder(FakeApp::Messages)
            .with("dm-relay-list")
            .with(account_pk(0x11))
            .with(42u64)
            .finish();
        let key_b = SubKey::builder(FakeApp::Messages)
            .with("dm-relay-list")
            .with(account_pk(0x11))
            .with(42u64)
            .finish();
        let key_c = SubKey::builder(FakeApp::Messages)
            .with("dm-relay-list")
            .with(account_pk(0x11))
            .with(43u64)
            .finish();

        assert_eq!(key_a, key_b);
        assert_ne!(key_a, key_c);
    }
    #[tokio::test]
    async fn full_history_subscriptions_create_modify_and_remove_live_outbox_state() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relays = relay_set("wss://relay-full-history.example.com");
        let key = SubKey::new(("timeline", "home", 1u8));
        let slot = runtime.create_slot();

        let created = runtime.set_sub_with_relays(
            &mut outbox(&mut pool),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            full_history_config(),
        );
        assert!(matches!(created, SetSubResult::Created));

        let scoped = ScopedSubRuntime::scoped_key(ResolvedSubScope::Global, key);
        let live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        let history_id = runtime
            .full_history
            .get(&scoped)
            .copied()
            .expect("full-history id");
        assert!(pool.filters(&live_id).is_some());

        let updated_config = live_full_history_config(
            vec![Filter::new().kinds(vec![42]).limit(7).build()],
            vec![Filter::new().kinds(vec![1]).limit(5).build()],
        );
        let updated = runtime.set_sub_with_relays(
            &mut outbox(&mut pool),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            updated_config.clone(),
        );
        assert!(matches!(updated, SetSubResult::Updated));
        assert_eq!(runtime.full_history.get(&scoped), Some(&history_id));
        assert_eq!(
            pool.filters(&live_id)
                .expect("updated live filters")
                .iter()
                .map(|filter| filter.json().expect("updated filter json"))
                .collect::<Vec<_>>(),
            [Filter::new().kinds(vec![42]).limit(7).build()]
                .iter()
                .map(|filter| filter.json().expect("expected filter json"))
                .collect::<Vec<_>>()
        );

        let routing_update = SubConfig::live(vec![Filter::new().kinds(vec![42]).limit(7).build()])
            .full_history(FullHistoryConfig::new(vec![Filter::new()
                .kinds(vec![1])
                .limit(5)
                .build()]))
            .routing_preference(RelayRoutingPreference::RequireDedicated)
            .build();
        let routing_updated = runtime.set_sub_with_relays(
            &mut outbox(&mut pool),
            &relays,
            account_pk(0x01),
            slot,
            SubScope::Global,
            key,
            routing_update,
        );
        assert!(matches!(routing_updated, SetSubResult::Updated));
        let replaced_live_id = runtime.live.get(&scoped).copied().expect("live sub id");
        assert_ne!(replaced_live_id, live_id);
        assert!(pool.filters(&live_id).is_none());
        assert_eq!(runtime.full_history.get(&scoped), Some(&history_id));

        let cleared = runtime.clear_sub_with_selected(
            &mut outbox(&mut pool),
            account_pk(0x01),
            slot,
            key,
            SubScope::Global,
        );
        assert!(matches!(cleared, ClearSubResult::Cleared));
        assert!(pool.filters(&live_id).is_none());
        assert!(pool.filters(&replaced_live_id).is_none());
        assert!(runtime.full_history.get(&scoped).is_none());
    }
    #[tokio::test]
    async fn account_switch_restores_full_history_account_subscriptions() {
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let relay_a = relay_set("wss://relay-account-a.example.com");
        let relay_b = relay_set("wss://relay-account-b.example.com");
        let old_pk = account_pk(0x11);
        let new_pk = account_pk(0x22);
        let key = SubKey::new(("timeline", "account-home", 9u8));
        let slot = runtime.create_slot();

        let created = runtime.set_sub_with_relays(
            &mut outbox(&mut pool),
            &relay_a,
            old_pk,
            slot,
            SubScope::Account,
            key,
            full_history_config(),
        );
        assert!(matches!(created, SetSubResult::Created));
        let resolved_old = ScopedSubRuntime::scoped_key(ResolvedSubScope::Account(old_pk), key);
        let old_live_id = runtime
            .live
            .get(&resolved_old)
            .copied()
            .expect("old live id");
        let old_history_id = runtime
            .full_history
            .get(&resolved_old)
            .copied()
            .expect("old full-history id");
        assert!(pool.filters(&old_live_id).is_some());

        runtime.on_account_switched_with_relays(&mut outbox(&mut pool), old_pk, new_pk, &relay_b);
        assert!(pool.filters(&old_live_id).is_none());
        assert!(runtime.full_history.get(&resolved_old).is_none());

        runtime.on_account_switched_with_relays(&mut outbox(&mut pool), new_pk, old_pk, &relay_a);

        let restored_live_id = runtime
            .live
            .get(&resolved_old)
            .copied()
            .expect("restored live id");
        let restored_history_id = runtime
            .full_history
            .get(&resolved_old)
            .copied()
            .expect("restored full-history id");
        assert!(pool.filters(&restored_live_id).is_some());
        assert_ne!(restored_history_id, old_history_id);
    }
}
