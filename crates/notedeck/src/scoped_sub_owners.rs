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
    pub fn len(&self) -> usize {
        self.slots_by_owner.len()
    }

    /// Returns true if no owner lifecycles are tracked.
    pub fn is_empty(&self) -> bool {
        self.slots_by_owner.is_empty()
    }
}
