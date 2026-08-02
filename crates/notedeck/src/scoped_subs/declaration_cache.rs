use enostr::Pubkey;
use hashbrown::{HashMap, HashSet};

use super::config::{
    ClearSubResult, EnsureSubResult, ResolvedSubScope, ScopedSubKey, SetSubResult, SubConfig,
    SubKey, SubOwnerKey, SubScope,
};

/// UI-thread declaration cache for cheap per-frame scoped-sub declarations.
///
/// This cache tracks local owner membership and exact-repeat `set_sub(...)`
/// commands only. It does not compute effective desired config; runtime/store
/// state owns that transition.
#[derive(Default)]
pub(super) struct ScopedSubDeclarationCache {
    owners_by_sub: HashMap<ScopedSubKey, HashSet<SubOwnerKey>>,
    set_configs: HashMap<ScopedSubKey, HashMap<SubOwnerKey, SubConfig>>,
}

/// Result of applying one declaration-cache transition.
pub(super) struct DeclarationCacheTransition<R> {
    pub(super) result: R,
    pub(super) forward_to_runtime: bool,
}

impl ScopedSubDeclarationCache {
    /// Resolve an app-facing scope/key for the current selected account.
    pub(super) fn scoped_key(
        selected_account_pubkey: Pubkey,
        scope: SubScope,
        key: SubKey,
    ) -> ScopedSubKey {
        ScopedSubKey {
            scope: match scope {
                SubScope::Account => ResolvedSubScope::Account(selected_account_pubkey),
                SubScope::Global => ResolvedSubScope::Global,
            },
            key,
        }
    }

    /// Record a create-if-absent declaration and report whether runtime work is needed.
    pub(super) fn ensure_sub(
        &mut self,
        scoped: ScopedSubKey,
        owner: SubOwnerKey,
    ) -> DeclarationCacheTransition<EnsureSubResult> {
        let had_owners = self.has_owners(&scoped);
        let was_owned = self.owner_owns(owner, &scoped);
        self.register_owner(owner, &scoped);

        DeclarationCacheTransition {
            result: if had_owners {
                EnsureSubResult::AlreadyExists
            } else {
                EnsureSubResult::Created
            },
            forward_to_runtime: !was_owned,
        }
    }

    /// Return whether one owner is locally declared for one scoped key.
    pub(super) fn owner_owns(&self, owner: SubOwnerKey, scoped: &ScopedSubKey) -> bool {
        self.owners_by_sub
            .get(scoped)
            .is_some_and(|owners| owners.contains(&owner))
    }

    /// Record an upsert declaration.
    pub(super) fn set_sub(
        &mut self,
        scoped: ScopedSubKey,
        owner: SubOwnerKey,
        config: SubConfig,
    ) -> DeclarationCacheTransition<SetSubResult> {
        let had_owners = self.has_owners(&scoped);
        let was_owned = self.owner_owns(owner, &scoped);
        let owner_config_unchanged = self
            .set_configs
            .get(&scoped)
            .and_then(|configs| configs.get(&owner))
            == Some(&config);
        if was_owned && owner_config_unchanged {
            return DeclarationCacheTransition {
                result: SetSubResult::Unchanged,
                forward_to_runtime: false,
            };
        }

        let result = if had_owners {
            SetSubResult::Updated
        } else {
            SetSubResult::Created
        };
        self.register_owner(owner, &scoped);
        self.set_configs
            .entry(scoped)
            .or_default()
            .insert(owner, config);
        DeclarationCacheTransition {
            result,
            forward_to_runtime: true,
        }
    }

    /// Clear one cached declaration and report the local lifecycle result.
    pub(super) fn clear_sub(
        &mut self,
        scoped: &ScopedSubKey,
        owner: SubOwnerKey,
    ) -> ClearSubResult {
        if !self.remove_owner(owner, scoped) {
            return ClearSubResult::NotFound;
        }

        if self.has_owners(scoped) {
            ClearSubResult::StillInUse
        } else {
            ClearSubResult::Cleared
        }
    }

    /// Drop every cached declaration owned by one owner.
    pub(super) fn drop_owner(&mut self, owner: SubOwnerKey) -> bool {
        let scoped_keys = self
            .owners_by_sub
            .iter()
            .filter(|(_, owners)| owners.contains(&owner))
            .map(|(scoped, _)| scoped.clone())
            .collect::<Vec<_>>();
        if scoped_keys.is_empty() {
            return false;
        }

        for scoped in scoped_keys {
            self.remove_owner_from_scoped(owner, &scoped);
        }
        true
    }

    /// Remove every cached declaration and owner binding under one account scope.
    pub(super) fn purge_scope(&mut self, scope: &ResolvedSubScope) {
        let scoped_keys = self
            .owners_by_sub
            .keys()
            .filter(|scoped| scoped.scope == *scope)
            .cloned()
            .collect::<HashSet<_>>();

        for scoped in scoped_keys {
            self.remove_scoped(&scoped);
        }
    }

    fn register_owner(&mut self, owner: SubOwnerKey, scoped: &ScopedSubKey) {
        self.owners_by_sub
            .entry(scoped.clone())
            .or_default()
            .insert(owner);
    }

    fn has_owners(&self, scoped: &ScopedSubKey) -> bool {
        self.owners_by_sub
            .get(scoped)
            .is_some_and(|owners| !owners.is_empty())
    }

    fn remove_owner(&mut self, owner: SubOwnerKey, scoped: &ScopedSubKey) -> bool {
        if !self.owner_owns(owner, scoped) {
            return false;
        }
        self.remove_owner_from_scoped(owner, scoped);
        true
    }

    fn remove_owner_from_scoped(&mut self, owner: SubOwnerKey, scoped: &ScopedSubKey) {
        if let Some(owners) = self.owners_by_sub.get_mut(scoped) {
            owners.remove(&owner);
            if owners.is_empty() {
                self.owners_by_sub.remove(scoped);
            }
        }

        if let Some(configs) = self.set_configs.get_mut(scoped) {
            configs.remove(&owner);
            if configs.is_empty() {
                self.set_configs.remove(scoped);
            }
        }
    }

    fn remove_scoped(&mut self, scoped: &ScopedSubKey) {
        self.owners_by_sub.remove(scoped);
        self.set_configs.remove(scoped);
    }
}

#[cfg(test)]
mod tests {
    use enostr::{Pubkey, RelayDemandPriority, RelayRoutingPreference};
    use nostrdb::Filter;

    use super::*;
    use crate::SubRelayPolicy;

    fn test_pubkey(tag: u8) -> Pubkey {
        Pubkey::new([tag; 32])
    }

    fn owner(tag: &'static str) -> SubOwnerKey {
        SubOwnerKey::new(("owner", tag))
    }

    fn scoped(label: &'static str) -> ScopedSubKey {
        ScopedSubDeclarationCache::scoped_key(
            test_pubkey(0xA1),
            SubScope::Account,
            SubKey::new(("sub", label)),
        )
    }

    fn config(kind: u64) -> SubConfig {
        SubConfig::builder(vec![Filter::new().kinds([kind]).build()])
            .accounts_read(SubRelayPolicy::new(
                RelayDemandPriority::Important,
                RelayRoutingPreference::default(),
            ))
            .build()
    }

    #[test]
    fn repeated_ensure_for_same_owner_requires_runtime_once() {
        let scoped = scoped("same-owner");
        let owner = owner("a");
        let mut cache = ScopedSubDeclarationCache::default();

        let first = cache.ensure_sub(scoped.clone(), owner);
        let second = cache.ensure_sub(scoped, owner);

        assert_eq!(first.result, EnsureSubResult::Created);
        assert!(first.forward_to_runtime);
        assert_eq!(second.result, EnsureSubResult::AlreadyExists);
        assert!(!second.forward_to_runtime);
    }

    #[test]
    fn ensure_for_new_owner_on_existing_scoped_key_still_requires_runtime() {
        let scoped = scoped("new-owner");
        let mut cache = ScopedSubDeclarationCache::default();

        let first = cache.ensure_sub(scoped.clone(), owner("a"));
        let second = cache.ensure_sub(scoped, owner("b"));

        assert_eq!(first.result, EnsureSubResult::Created);
        assert!(first.forward_to_runtime);
        assert_eq!(second.result, EnsureSubResult::AlreadyExists);
        assert!(second.forward_to_runtime);
    }

    #[test]
    fn clearing_owner_keeps_remaining_membership_owner() {
        let scoped = scoped("membership-owner");
        let mut cache = ScopedSubDeclarationCache::default();

        let _ = cache.ensure_sub(scoped.clone(), owner("a"));
        let _ = cache.ensure_sub(scoped.clone(), owner("b"));

        assert_eq!(
            cache.clear_sub(&scoped, owner("a")),
            ClearSubResult::StillInUse
        );
        let repeat = cache.ensure_sub(scoped, owner("b"));

        assert_eq!(repeat.result, EnsureSubResult::AlreadyExists);
        assert!(!repeat.forward_to_runtime);
    }

    #[test]
    fn repeated_set_for_same_owner_and_config_does_not_forward() {
        let scoped = scoped("same-set");
        let config = config(1);
        let mut cache = ScopedSubDeclarationCache::default();

        let first = cache.set_sub(scoped.clone(), owner("a"), config.clone());
        let second = cache.set_sub(scoped, owner("a"), config);

        assert_eq!(first.result, SetSubResult::Created);
        assert!(first.forward_to_runtime);
        assert_eq!(second.result, SetSubResult::Unchanged);
        assert!(!second.forward_to_runtime);
    }

    #[test]
    fn set_after_ensure_for_same_owner_still_forwards_explicit_set() {
        let scoped = scoped("ensure-then-set");
        let owner = owner("a");
        let config = config(1);
        let mut cache = ScopedSubDeclarationCache::default();

        let ensured = cache.ensure_sub(scoped.clone(), owner);
        let set = cache.set_sub(scoped, owner, config);

        assert_eq!(ensured.result, EnsureSubResult::Created);
        assert!(ensured.forward_to_runtime);
        assert_eq!(set.result, SetSubResult::Updated);
        assert!(set.forward_to_runtime);
    }
}
