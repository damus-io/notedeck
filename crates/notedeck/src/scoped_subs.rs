use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::{Accounts, Outbox};
use enostr::{NormRelayUrl, OutboxSubId, Pubkey, RelayReqStatus, RelayUrlPkgs};
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
/// This is configuration only (`relays`, `filters`, transport mode). Identity is carried by
/// [`ScopedSubIdentity`] (`owner + key + scope`).
#[derive(Clone, Debug)]
pub struct SubConfig {
    pub relays: RelaySelection,
    pub filters: Vec<Filter>,
    pub use_transparent: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetSubLiveOp {
    EnsurePresent,
    ReplaceExisting,
    ModifyExisting,
    RemoveExisting,
}

/// Result of setting a desired subscription entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetSubResult {
    Created,
    Updated,
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
pub(crate) struct ScopedSubRuntime {
    desired: HashMap<ScopedSubKey, SubConfig>,
    live: HashMap<ScopedSubKey, OutboxSubId>,
    owners_by_sub: HashMap<ScopedSubKey, HashSet<SubSlotId>>,
    subs_by_slot: HashMap<SubSlotId, HashSet<ScopedSubKey>>,
    next_slot_id: u64,
}

impl Default for ScopedSubRuntime {
    fn default() -> Self {
        Self {
            desired: HashMap::default(),
            live: HashMap::default(),
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
        mut config: SubConfig,
    ) -> EnsureSubResult {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        self.register_ownership(slot, &scoped);
        if self.desired.contains_key(&scoped) {
            return EnsureSubResult::AlreadyExists;
        }

        config.filters = normalize_filters(config.filters);
        self.desired.insert(scoped.clone(), config.clone());
        self.ensure_live_sub(pool, account_read_relays, scoped, &config);
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
        mut config: SubConfig,
    ) -> SetSubResult {
        let resolved_scope = resolve_scope(&scope, selected_account_pubkey);
        let scoped = Self::scoped_key(resolved_scope, key);

        self.register_ownership(slot, &scoped);

        config.filters = normalize_filters(config.filters);
        let previous = self.desired.insert(scoped.clone(), config.clone());
        let op = plan_set_sub_live_op(previous.as_ref(), &config, self.live.contains_key(&scoped));

        if previous.is_none() {
            self.ensure_live_sub(pool, account_read_relays, scoped, &config);
            return SetSubResult::Created;
        }

        match op {
            SetSubLiveOp::EnsurePresent => {
                self.ensure_live_sub(pool, account_read_relays, scoped, &config);
            }
            SetSubLiveOp::ReplaceExisting => {
                self.replace_live_sub(pool, account_read_relays, &scoped, &config);
            }
            SetSubLiveOp::ModifyExisting => {
                if let Some(id) = self.live.get(&scoped).copied() {
                    Self::modify_live_sub(pool, account_read_relays, id, &config);
                }
            }
            SetSubLiveOp::RemoveExisting => {
                self.remove_live_sub(pool, &scoped);
            }
        }

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
            let relay_statuses = pool.outbox.status(&live_id);
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
            if self.live.contains_key(&scoped) {
                continue;
            }

            let Some(spec) = self.desired.get(&scoped) else {
                continue;
            };

            if let Some(live_id) = subscribe_live(pool, new_account_read_relays, spec) {
                self.live.insert(scoped, live_id);
            }
        }
    }

    /// Retarget live subscriptions that depend on the selected account's read relay set.
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

    /// Retarget selected-account-dependent live subscriptions with pre-resolved read relays.
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

            let has_live = self.live.get(&scoped).copied();

            if spec.filters.is_empty() {
                if has_live.is_some() {
                    self.remove_live_sub(pool, &scoped);
                }
                continue;
            }

            if let Some(live_id) = has_live {
                pool.modify_relays(live_id, resolve_relays(account_read_relays, &spec.relays));
            } else {
                self.ensure_live_sub(pool, account_read_relays, scoped, &spec);
            }
        }
    }

    pub fn desired_len(&self) -> usize {
        self.desired.len()
    }

    pub fn live_len(&self) -> usize {
        self.live.len()
    }

    pub fn slot_len(&self) -> usize {
        self.subs_by_slot.len()
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

    fn ensure_live_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: ScopedSubKey,
        spec: &SubConfig,
    ) {
        if let Some(id) = subscribe_live(pool, account_read_relays, spec) {
            self.live.insert(scoped, id);
        }
    }

    fn replace_live_sub(
        &mut self,
        pool: &mut Outbox<'_>,
        account_read_relays: &HashSet<NormRelayUrl>,
        scoped: &ScopedSubKey,
        spec: &SubConfig,
    ) {
        self.remove_live_sub(pool, scoped);
        self.ensure_live_sub(pool, account_read_relays, scoped.clone(), spec);
    }

    fn modify_live_sub(
        pool: &mut Outbox<'_>,
        account_read_relays: &HashSet<NormRelayUrl>,
        live_id: OutboxSubId,
        spec: &SubConfig,
    ) {
        pool.modify_filters(live_id, spec.filters.clone());
        pool.modify_relays(live_id, resolve_relays(account_read_relays, &spec.relays));
    }

    fn remove_live_sub(&mut self, pool: &mut Outbox<'_>, scoped: &ScopedSubKey) {
        if let Some(live_id) = self.live.remove(scoped) {
            pool.unsubscribe(live_id);
        }
    }

    fn unsubscribe_scope(&mut self, pool: &mut Outbox<'_>, scope: &ResolvedSubScope) {
        self.live.retain(|scoped, sub_id| {
            if scoped.scope == *scope {
                pool.unsubscribe(*sub_id);
                false
            } else {
                true
            }
        });
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
        if let Some(sub_id) = self.live.remove(scoped) {
            pool.unsubscribe(sub_id);
        }

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

fn plan_set_sub_live_op(
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

    if previous.use_transparent != next.use_transparent {
        return SetSubLiveOp::ReplaceExisting;
    }

    if next.filters.is_empty() {
        SetSubLiveOp::RemoveExisting
    } else {
        SetSubLiveOp::ModifyExisting
    }
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

fn subscribe_live(
    pool: &mut Outbox<'_>,
    account_read_relays: &HashSet<NormRelayUrl>,
    spec: &SubConfig,
) -> Option<OutboxSubId> {
    if spec.filters.is_empty() {
        return None;
    }

    let relays = resolve_relays(account_read_relays, &spec.relays);
    let mut relay_pkgs = RelayUrlPkgs::new(relays);
    relay_pkgs.use_transparent = spec.use_transparent;
    Some(pool.subscribe(spec.filters.clone(), relay_pkgs))
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
