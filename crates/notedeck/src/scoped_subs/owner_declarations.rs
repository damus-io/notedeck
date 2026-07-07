use hashbrown::{HashMap, HashSet};

use super::config::{ScopedSubKey, SubConfig, SubOwnerKey};

/// Owner membership plus ordered per-owner declarations for retained scoped subs.
///
/// Runtime storage uses this to rebuild one effective desired config from
/// retained owner declarations. UI declaration caching must not depend on this
/// merge behavior.
#[derive(Default)]
pub(super) struct ScopedSubOwnerDeclarations {
    configs: HashMap<ScopedSubKey, HashMap<SubOwnerKey, SubConfig>>,
    declaration_order: HashMap<ScopedSubKey, Vec<SubOwnerKey>>,
    owners_by_sub: HashMap<ScopedSubKey, HashSet<SubOwnerKey>>,
    subs_by_owner: HashMap<SubOwnerKey, HashSet<ScopedSubKey>>,
}

impl ScopedSubOwnerDeclarations {
    pub(super) fn register(&mut self, owner: SubOwnerKey, scoped: &ScopedSubKey) {
        self.subs_by_owner
            .entry(owner)
            .or_default()
            .insert(scoped.clone());
        self.owners_by_sub
            .entry(scoped.clone())
            .or_default()
            .insert(owner);
    }

    pub(super) fn owner_owns(&self, owner: SubOwnerKey, scoped: &ScopedSubKey) -> bool {
        self.subs_by_owner
            .get(&owner)
            .is_some_and(|owned| owned.contains(scoped))
    }

    pub(super) fn set_declaration(
        &mut self,
        scoped: ScopedSubKey,
        owner: SubOwnerKey,
        config: SubConfig,
    ) {
        self.configs
            .entry(scoped.clone())
            .or_default()
            .insert(owner, config);

        let order = self.declaration_order.entry(scoped).or_default();
        order.retain(|candidate| *candidate != owner);
        order.push(owner);
    }

    pub(super) fn merged_declaration(&self, scoped: &ScopedSubKey) -> Option<SubConfig> {
        let configs = self
            .declaration_order
            .get(scoped)?
            .iter()
            .filter_map(|owner| {
                self.configs
                    .get(scoped)
                    .and_then(|configs| configs.get(owner))
            })
            .collect::<Vec<_>>();
        SubConfig::merged_owner_configs(&configs)
    }

    pub(super) fn remove_owner_membership(
        &mut self,
        owner: SubOwnerKey,
        scoped: &ScopedSubKey,
    ) -> bool {
        let Some(owner_entries) = self.subs_by_owner.get_mut(&owner) else {
            return false;
        };

        if !owner_entries.remove(scoped) {
            return false;
        }

        if owner_entries.is_empty() {
            self.subs_by_owner.remove(&owner);
        }
        true
    }

    pub(super) fn take_owner(&mut self, owner: SubOwnerKey) -> Option<HashSet<ScopedSubKey>> {
        self.subs_by_owner.remove(&owner)
    }

    pub(super) fn remove_scoped_owner(
        &mut self,
        owner: SubOwnerKey,
        scoped: &ScopedSubKey,
    ) -> bool {
        let Some(owners) = self.owners_by_sub.get_mut(scoped) else {
            return false;
        };

        owners.remove(&owner)
    }

    pub(super) fn remove_declaration(&mut self, owner: SubOwnerKey, scoped: &ScopedSubKey) {
        if let Some(configs) = self.configs.get_mut(scoped) {
            configs.remove(&owner);
            if configs.is_empty() {
                self.configs.remove(scoped);
            }
        }

        if let Some(order) = self.declaration_order.get_mut(scoped) {
            order.retain(|candidate| *candidate != owner);
            if order.is_empty() {
                self.declaration_order.remove(scoped);
            }
        }
    }

    pub(super) fn remove_scoped(&mut self, scoped: &ScopedSubKey) {
        self.configs.remove(scoped);
        self.declaration_order.remove(scoped);

        if let Some(owners) = self.owners_by_sub.remove(scoped) {
            for owner in owners {
                if let Some(owner_entries) = self.subs_by_owner.get_mut(&owner) {
                    owner_entries.remove(scoped);
                    if owner_entries.is_empty() {
                        self.subs_by_owner.remove(&owner);
                    }
                }
            }
        }
    }

    pub(super) fn scoped_keys(&self) -> impl Iterator<Item = &ScopedSubKey> {
        self.owners_by_sub.keys()
    }

    pub(super) fn has_owners(&self, scoped: &ScopedSubKey) -> bool {
        self.owners_by_sub
            .get(scoped)
            .is_some_and(|owners| !owners.is_empty())
    }

    #[cfg(test)]
    pub(super) fn owner_len(&self) -> usize {
        self.subs_by_owner.len()
    }
}

#[cfg(test)]
mod tests {
    use enostr::{Pubkey, RelayDemandPriority, RelayRoutingPreference};
    use nostrdb::Filter;

    use super::*;
    use crate::{SubKey, SubRelayPolicy};

    use crate::scoped_subs::config::ResolvedSubScope;

    fn scoped(label: &'static str) -> ScopedSubKey {
        ScopedSubKey {
            scope: ResolvedSubScope::Account(Pubkey::new([0xA1; 32])),
            key: SubKey::new(("shared-owner-declarations", label)),
        }
    }

    fn owner(label: &'static str) -> SubOwnerKey {
        SubOwnerKey::new(("shared-owner", label))
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
    fn replacing_declaration_moves_owner_to_latest_merge_position() {
        let scoped = scoped("replacement-order");
        let owner_a = owner("a");
        let owner_b = owner("b");
        let config_a1 = config(1);
        let config_b = config(2);
        let config_a2 = config(3);
        let mut declarations = ScopedSubOwnerDeclarations::default();

        declarations.register(owner_a, &scoped);
        declarations.set_declaration(scoped.clone(), owner_a, config_a1);
        declarations.register(owner_b, &scoped);
        declarations.set_declaration(scoped.clone(), owner_b, config_b.clone());
        assert_eq!(declarations.merged_declaration(&scoped), Some(config_b));

        declarations.set_declaration(scoped.clone(), owner_a, config_a2.clone());

        assert_eq!(declarations.merged_declaration(&scoped), Some(config_a2));
    }

    #[test]
    fn removing_latest_declaration_restores_remaining_owner_config() {
        let scoped = scoped("drop-latest");
        let owner_a = owner("a");
        let owner_b = owner("b");
        let config_a = config(1);
        let config_b = config(2);
        let mut declarations = ScopedSubOwnerDeclarations::default();

        declarations.register(owner_a, &scoped);
        declarations.set_declaration(scoped.clone(), owner_a, config_a.clone());
        declarations.register(owner_b, &scoped);
        declarations.set_declaration(scoped.clone(), owner_b, config_b);

        assert!(declarations.remove_owner_membership(owner_b, &scoped));
        assert!(declarations.remove_scoped_owner(owner_b, &scoped));
        declarations.remove_declaration(owner_b, &scoped);

        assert!(declarations.has_owners(&scoped));
        assert_eq!(declarations.merged_declaration(&scoped), Some(config_a));
    }
}
