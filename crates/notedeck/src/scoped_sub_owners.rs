use enostr::Pubkey;
use hashbrown::HashMap;

use crate::{
    scoped_subs::{ScopedSubRuntime, SubOwnerKey, SubSlotId},
    Accounts, ClearSubResult, EnsureSubResult, Outbox, ScopedSubEoseStatus, ScopedSubIdentity,
    SetSubResult, SubConfig, SubKeyBuilder, SubScope,
};

/// Incremental builder for stable owner keys.
pub type SubOwnerKeyBuilder = SubKeyBuilder;

/// Host-owned mapping from owner lifecycles to runtime slots.
///
/// This is intended to be held by host/container code (not app feature state)
/// so slot ids are never stored in app modules.
#[derive(Default)]
pub(crate) struct ScopedSubOwners {
    slots_by_owner: HashMap<SubOwnerKey, SubSlotId>,
}

impl ScopedSubOwners {
    /// Ensure one runtime slot exists for this owner key.
    pub(crate) fn ensure_slot(
        &mut self,
        runtime: &mut ScopedSubRuntime,
        owner: SubOwnerKey,
    ) -> SubSlotId {
        if let Some(slot) = self.slots_by_owner.get(&owner).copied() {
            return slot;
        }

        let slot = runtime.create_slot();
        self.slots_by_owner.insert(owner, slot);
        slot
    }

    /// Forward an upsert scoped-sub request for an owner lifecycle.
    pub fn set_sub(
        &mut self,
        runtime: &mut ScopedSubRuntime,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
        identity: ScopedSubIdentity,
        config: SubConfig,
    ) -> SetSubResult {
        let slot = self.ensure_slot(runtime, identity.owner);
        runtime.set_sub(pool, accounts, slot, identity.scope, identity.key, config)
    }

    /// Forward a create-if-absent scoped-sub request for an owner lifecycle.
    pub fn ensure_sub(
        &mut self,
        runtime: &mut ScopedSubRuntime,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
        identity: ScopedSubIdentity,
        config: SubConfig,
    ) -> EnsureSubResult {
        let slot = self.ensure_slot(runtime, identity.owner);
        runtime.ensure_sub(pool, accounts, slot, identity.scope, identity.key, config)
    }

    /// Clear one scoped subscription binding from an owner lifecycle.
    pub fn clear_sub(
        &mut self,
        runtime: &mut ScopedSubRuntime,
        pool: &mut Outbox<'_>,
        accounts: &Accounts,
        identity: ScopedSubIdentity,
    ) -> ClearSubResult {
        let Some(slot) = self.slots_by_owner.get(&identity.owner).copied() else {
            return ClearSubResult::NotFound;
        };

        runtime.clear_sub(pool, accounts, slot, identity.key, identity.scope)
    }

    /// Clear one account-scoped subscription binding from an owner lifecycle for an explicit account.
    pub fn clear_sub_for_account(
        &mut self,
        runtime: &mut ScopedSubRuntime,
        pool: &mut Outbox<'_>,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
    ) -> ClearSubResult {
        debug_assert!(matches!(identity.scope, SubScope::Account));
        let Some(slot) = self.slots_by_owner.get(&identity.owner).copied() else {
            return ClearSubResult::NotFound;
        };

        runtime.clear_sub_with_selected(pool, account_pubkey, slot, identity.key, identity.scope)
    }

    /// Query aggregate EOSE state for one scoped subscription binding owned by `owner`.
    pub fn sub_eose_status(
        &self,
        runtime: &ScopedSubRuntime,
        pool: &Outbox<'_>,
        accounts: &Accounts,
        identity: ScopedSubIdentity,
    ) -> ScopedSubEoseStatus {
        let Some(slot) = self.slots_by_owner.get(&identity.owner).copied() else {
            return ScopedSubEoseStatus::Missing;
        };

        runtime.sub_eose_status(pool, accounts, slot, identity.key, identity.scope)
    }

    /// Drop one owner lifecycle and release all its scoped subscriptions.
    pub fn drop_owner(
        &mut self,
        runtime: &mut ScopedSubRuntime,
        pool: &mut Outbox<'_>,
        owner: SubOwnerKey,
    ) -> bool {
        let Some(slot) = self.slots_by_owner.remove(&owner) else {
            return false;
        };

        let _ = runtime.drop_slot(pool, slot);
        true
    }

    /// Number of tracked owner lifecycles.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.slots_by_owner.len()
    }

    /// Returns true if no owner lifecycles are tracked.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.slots_by_owner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EguiWakeup;
    use enostr::OutboxPool;

    /// Verifies the same owner key always resolves to the same runtime slot.
    #[test]
    fn ensure_slot_is_stable_for_owner() {
        let mut owners = ScopedSubOwners::default();
        let mut runtime = ScopedSubRuntime::default();

        let owner = SubOwnerKey::builder("threads").with(42u64).finish();
        let a = owners.ensure_slot(&mut runtime, owner);
        let b = owners.ensure_slot(&mut runtime, owner);

        assert_eq!(a, b);
        assert_eq!(owners.len(), 1);
    }

    /// Ensures different owner keys allocate distinct runtime slots.
    #[test]
    fn ensure_slot_distinguishes_different_owners() {
        let mut owners = ScopedSubOwners::default();
        let mut runtime = ScopedSubRuntime::default();

        let owner_a = SubOwnerKey::builder("threads").with(1u64).finish();
        let owner_b = SubOwnerKey::builder("threads").with(2u64).finish();

        let slot_a = owners.ensure_slot(&mut runtime, owner_a);
        let slot_b = owners.ensure_slot(&mut runtime, owner_b);

        assert_ne!(slot_a, slot_b);
        assert_eq!(owners.len(), 2);
    }

    /// Verifies dropping an owner removes its slot mapping and is idempotent.
    #[test]
    fn drop_owner_removes_mapping_and_runtime_slot() {
        let mut owners = ScopedSubOwners::default();
        let mut runtime = ScopedSubRuntime::default();
        let mut pool = OutboxPool::default();
        let mut outbox =
            enostr::OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));

        let owner = SubOwnerKey::builder("onboarding").finish();
        let _slot = owners.ensure_slot(&mut runtime, owner);

        assert!(owners.drop_owner(&mut runtime, &mut outbox, owner));
        assert!(!owners.slots_by_owner.contains_key(&owner));
        assert!(owners.is_empty());

        assert!(!owners.drop_owner(&mut runtime, &mut outbox, owner));
    }
}
