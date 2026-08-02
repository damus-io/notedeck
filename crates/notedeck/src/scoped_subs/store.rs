use enostr::Pubkey;
use hashbrown::{HashMap, HashSet};

use super::config::{ResolvedSubScope, ScopedSubKey, SubConfig, SubOwnerKey};
use super::owner_declarations::ScopedSubOwnerDeclarations;

/// Result of removing one owner from a retained scoped subscription.
pub(super) enum ScopedSubStoreRelease {
    NotFound,
    StillInUse {
        previous_config: Option<SubConfig>,
        next_config: Option<SubConfig>,
    },
    Cleared {
        removed_config: Option<SubConfig>,
    },
}

/// Effective desired config transition after one owner declaration changes.
pub(super) struct ScopedSubStoreDesiredChange {
    pub(super) previous: Option<SubConfig>,
    pub(super) next: Option<SubConfig>,
}

/// Desired config removed while purging one resolved scope.
pub(super) struct ScopedSubStoreScopeRemoval {
    pub(super) scoped: ScopedSubKey,
    pub(super) removed_config: Option<SubConfig>,
}

/// Desired and owner state removed while purging one resolved scope.
pub(super) struct ScopedSubStoreScopePurge {
    pub(super) removed: Vec<ScopedSubStoreScopeRemoval>,
}

/// Retained scoped-subscription declarations plus their effective desired state.
///
/// `owners` is the source of truth for app declarations and ownership.
/// `desired` is the derived effective config map consumed by planning and
/// realization. Keeping them separate prevents lifecycle cleanup from depending
/// on how owner declarations are ordered or merged.
#[derive(Default)]
pub(super) struct ScopedSubStore {
    owners: ScopedSubOwnerDeclarations,
    desired: ScopedSubDesiredStore,
}

impl ScopedSubStore {
    /// Register that `owner` currently owns `scoped`.
    pub(super) fn register_ownership(&mut self, owner: SubOwnerKey, scoped: &ScopedSubKey) {
        self.owners.register(owner, scoped);
    }

    /// Return retained desired config for one scoped key.
    pub(super) fn desired(&self, scoped: &ScopedSubKey) -> Option<&SubConfig> {
        self.desired.get(scoped)
    }

    /// Return whether desired state exists for one scoped key.
    pub(super) fn contains_desired(&self, scoped: &ScopedSubKey) -> bool {
        self.desired.contains(scoped)
    }

    /// Return whether one owner owns a scoped key.
    pub(super) fn owner_owns(&self, owner: SubOwnerKey, scoped: &ScopedSubKey) -> bool {
        self.owners.owner_owns(owner, scoped)
    }

    /// Retain one owner's declaration and derive the effective desired config.
    pub(super) fn set_owner_config(
        &mut self,
        scoped: ScopedSubKey,
        owner: SubOwnerKey,
        config: SubConfig,
    ) -> ScopedSubStoreDesiredChange {
        let previous = self.desired.get(&scoped).cloned();
        self.owners.set_declaration(scoped.clone(), owner, config);
        let next = self.rebuild_desired_from_declarations(&scoped, previous.clone());
        ScopedSubStoreDesiredChange { previous, next }
    }

    /// Return scoped keys with desired state and at least one active owner.
    pub(super) fn owned_desired_keys_for_scope(
        &self,
        scope: &ResolvedSubScope,
    ) -> Vec<ScopedSubKey> {
        self.desired
            .keys()
            .filter(|key| key.scope == *scope && self.owners.has_owners(key))
            .cloned()
            .collect()
    }

    /// Return selected-account or global desired keys with at least one owner.
    pub(super) fn owned_desired_keys_for_selected_or_global(
        &self,
        selected_account_pubkey: Pubkey,
    ) -> Vec<ScopedSubKey> {
        self.desired
            .keys()
            .filter(|scoped| {
                scoped.is_active_for_account(selected_account_pubkey)
                    && self.owners.has_owners(scoped)
            })
            .cloned()
            .collect()
    }

    /// Remove one `(owner, scoped)` binding and retained desired state if it was last owner.
    pub(super) fn clear_owner_binding(
        &mut self,
        owner: SubOwnerKey,
        scoped: &ScopedSubKey,
    ) -> ScopedSubStoreRelease {
        if !self.owners.remove_owner_membership(owner, scoped) {
            return ScopedSubStoreRelease::NotFound;
        }

        self.release_owner(owner, scoped)
    }

    /// Remove and return every scoped key owned by an owner.
    pub(super) fn take_owner(&mut self, owner: SubOwnerKey) -> Option<HashSet<ScopedSubKey>> {
        self.owners.take_owner(owner)
    }

    /// Remove every desired and owner binding under one resolved account scope.
    pub(super) fn purge_scope(&mut self, scope: &ResolvedSubScope) -> ScopedSubStoreScopePurge {
        let scoped_keys = self
            .desired
            .keys()
            .chain(self.owners.scoped_keys())
            .filter(|scoped| scoped.scope == *scope)
            .cloned()
            .collect::<HashSet<_>>();
        let mut removed = Vec::new();

        for scoped in scoped_keys {
            let removed_config = self.desired.remove(&scoped);
            self.owners.remove_scoped(&scoped);
            removed.push(ScopedSubStoreScopeRemoval {
                scoped,
                removed_config,
            });
        }

        ScopedSubStoreScopePurge { removed }
    }

    /// Remove one owner from a scoped key after owner membership was already removed.
    pub(super) fn release_owner(
        &mut self,
        owner: SubOwnerKey,
        scoped: &ScopedSubKey,
    ) -> ScopedSubStoreRelease {
        if !self.owners.remove_scoped_owner(owner, scoped) {
            return ScopedSubStoreRelease::NotFound;
        }

        let still_in_use = self.owners.has_owners(scoped);
        let previous_config = self.desired.get(scoped).cloned();
        self.owners.remove_declaration(owner, scoped);

        if still_in_use {
            let next_config =
                self.rebuild_desired_from_declarations(scoped, previous_config.clone());
            return ScopedSubStoreRelease::StillInUse {
                previous_config,
                next_config,
            };
        }

        self.owners.remove_scoped(scoped);
        ScopedSubStoreRelease::Cleared {
            removed_config: self.desired.remove(scoped),
        }
    }

    #[cfg(test)]
    pub(super) fn desired_len(&self) -> usize {
        self.desired.len()
    }

    #[cfg(test)]
    pub(super) fn desired_for_test(&self, scoped: &ScopedSubKey) -> Option<&SubConfig> {
        self.desired.get(scoped)
    }

    #[cfg(test)]
    pub(super) fn owner_len(&self) -> usize {
        self.owners.owner_len()
    }

    fn rebuild_desired_from_declarations(
        &mut self,
        scoped: &ScopedSubKey,
        fallback: Option<SubConfig>,
    ) -> Option<SubConfig> {
        let next = self
            .owners
            .merged_declaration(scoped)
            .or_else(|| fallback.clone());
        if let Some(next) = &next {
            self.desired.set(scoped.clone(), next.clone());
        } else {
            self.desired.remove(scoped);
        }
        next
    }
}

/// Effective desired configs derived from owner declarations.
#[derive(Default)]
struct ScopedSubDesiredStore {
    configs: HashMap<ScopedSubKey, SubConfig>,
}

impl ScopedSubDesiredStore {
    fn get(&self, scoped: &ScopedSubKey) -> Option<&SubConfig> {
        self.configs.get(scoped)
    }

    fn contains(&self, scoped: &ScopedSubKey) -> bool {
        self.configs.contains_key(scoped)
    }

    fn set(&mut self, scoped: ScopedSubKey, config: SubConfig) {
        self.configs.insert(scoped, config);
    }

    fn remove(&mut self, scoped: &ScopedSubKey) -> Option<SubConfig> {
        self.configs.remove(scoped)
    }

    fn keys(&self) -> impl Iterator<Item = &ScopedSubKey> {
        self.configs.keys()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.configs.len()
    }
}

#[cfg(test)]
mod tests {
    use enostr::{NormRelayUrl, Pubkey, RelayDemandPriority, RelayRoutingPreference};
    use nostrdb::Filter;

    use super::*;
    use crate::scoped_subs::config::SubKey;
    use crate::SubRelayPolicy;

    fn owner(tag: &'static str) -> SubOwnerKey {
        SubOwnerKey::new(("store-owner", tag))
    }

    fn scoped(label: &'static str) -> ScopedSubKey {
        ScopedSubKey {
            scope: ResolvedSubScope::Account(Pubkey::new([0xA1; 32])),
            key: SubKey::new(("store-merge", label)),
        }
    }

    fn additive_config(relays: impl IntoIterator<Item = &'static str>) -> SubConfig {
        let relays = relays
            .into_iter()
            .map(|url| NormRelayUrl::new(url).unwrap())
            .collect::<HashSet<_>>();
        SubConfig::builder(vec![Filter::new().kinds([1]).build()])
            .accounts_read(SubRelayPolicy::new(
                RelayDemandPriority::Important,
                RelayRoutingPreference::default(),
            ))
            .with_explicit_relays(
                relays,
                SubRelayPolicy::new(
                    RelayDemandPriority::Important,
                    RelayRoutingPreference::default(),
                ),
            )
            .build()
    }

    #[test]
    fn dropping_latest_owner_restores_remaining_store_declaration() {
        let scoped = scoped("owner-drop");
        let owner_a = owner("a");
        let owner_b = owner("b");
        let config_a = additive_config(["wss://owner-a.example.com"]);
        let config_b = additive_config(["wss://owner-b.example.com"]);
        let merged = additive_config(["wss://owner-a.example.com", "wss://owner-b.example.com"]);
        let mut store = ScopedSubStore::default();

        store.register_ownership(owner_a, &scoped);
        let _ = store.set_owner_config(scoped.clone(), owner_a, config_a.clone());
        store.register_ownership(owner_b, &scoped);
        let _ = store.set_owner_config(scoped.clone(), owner_b, config_b);
        assert_eq!(store.desired_for_test(&scoped), Some(&merged));

        let release = store.clear_owner_binding(owner_b, &scoped);

        match release {
            ScopedSubStoreRelease::StillInUse {
                previous_config,
                next_config,
            } => {
                assert_eq!(previous_config, Some(merged));
                assert_eq!(next_config, Some(config_a.clone()));
            }
            ScopedSubStoreRelease::NotFound | ScopedSubStoreRelease::Cleared { .. } => {
                panic!("expected remaining owner declaration")
            }
        }
        assert_eq!(store.desired_for_test(&scoped), Some(&config_a));
    }
}
