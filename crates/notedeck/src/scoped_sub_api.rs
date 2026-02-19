use enostr::Pubkey;

use crate::scoped_sub_owners::ScopedSubOwners;
use crate::scoped_subs::ScopedSubRuntime;
use crate::{
    Accounts, ClearSubResult, EnsureSubResult, Outbox, ScopedSubEoseStatus, ScopedSubIdentity,
    SetSubResult, SubConfig, SubOwnerKey,
};

/// App-facing facade over scoped subscription owner/runtime operations.
///
/// This bundles host resources that are commonly passed together to avoid
/// argument plumbing through app-layer helper functions.
pub struct ScopedSubApi<'o, 'a> {
    pool: &'o mut Outbox<'a>,
    accounts: &'o Accounts,
    owners: &'o mut ScopedSubOwners,
    runtime: &'o mut ScopedSubRuntime,
}

impl<'o, 'a> ScopedSubApi<'o, 'a> {
    pub(crate) fn new(
        pool: &'o mut Outbox<'a>,
        accounts: &'o Accounts,
        owners: &'o mut ScopedSubOwners,
        runtime: &'o mut ScopedSubRuntime,
    ) -> Self {
        Self {
            pool,
            accounts,
            owners,
            runtime,
        }
    }

    pub fn selected_account_pubkey(&self) -> Pubkey {
        *self.accounts.selected_account_pubkey()
    }

    /// Create or update one scoped remote subscription declaration.
    ///
    /// Thread example (recommended mental model):
    /// - `identity.owner` = one thread view lifecycle (for example one open thread pane)
    /// - `identity.key` = `replies-by-root(root_id)`
    /// - `identity.scope` = `SubScope::Account`
    ///
    /// If two thread views open the same root on the same account, they should use:
    /// - different `owner`
    /// - same `key`
    /// - same `scope`
    ///
    /// The runtime shares one live outbox subscription for that resolved `(scope, key)`.
    ///
    /// `set_sub(...)` is an upsert for the resolved `(scope, key)`:
    /// - first call creates desired state
    /// - repeated calls update/replace desired state and may modify the live outbox sub
    ///
    /// Use this when the remote config can change (filters and/or relays).
    /// For thread reply subscriptions, prefer [`Self::ensure_sub`] unless the thread's
    /// remote filters actually change.
    ///
    /// Account-scoped behavior (`SubScope::Account`):
    /// - On switch away, the live outbox subscription is unsubscribed.
    /// - Desired state is retained while owners still exist.
    /// - On switch back, the live outbox subscription is restored from desired state.
    /// - If owners are dropped while away, nothing is restored.
    pub fn set_sub(&mut self, identity: ScopedSubIdentity, config: SubConfig) -> SetSubResult {
        self.owners
            .set_sub(self.runtime, self.pool, self.accounts, identity, config)
    }

    /// Create a scoped remote subscription declaration only if it is absent.
    ///
    /// Thread open path example:
    /// - build `identity = { owner: thread-view, key: replies-by-root(root_id), scope: Account }`
    /// - call `ensure_sub(identity, config)` when opening the thread
    ///
    /// Repeated calls with the same resolved `(scope, key)`:
    /// - keep ownership attached
    /// - do not modify desired state
    /// - do not modify the live outbox subscription
    ///
    /// This is the preferred API for stable thread reply subscriptions because it is
    /// idempotent and avoids unnecessary outbox subscription updates on repeats.
    ///
    /// Account-switch behavior matches [`Self::set_sub`].
    pub fn ensure_sub(
        &mut self,
        identity: ScopedSubIdentity,
        config: SubConfig,
    ) -> EnsureSubResult {
        self.owners
            .ensure_sub(self.runtime, self.pool, self.accounts, identity, config)
    }

    /// Clear one scoped subscription declaration while keeping the owner alive.
    ///
    /// Thread example:
    /// - This is less common than [`Self::drop_owner`].
    /// - Use this only if a thread owner remains alive but should stop declaring one
    ///   specific thread remote sub key.
    ///
    /// Outbox behavior:
    /// - If other owners still declare the same resolved `(scope, key)`, the shared live
    ///   outbox subscription remains active.
    /// - If this was the last owner for that `(scope, key)`, the live outbox subscription
    ///   is unsubscribed (if active) and desired state is removed.
    pub fn clear_sub(&mut self, identity: ScopedSubIdentity) -> ClearSubResult {
        self.owners
            .clear_sub(self.runtime, self.pool, self.accounts, identity)
    }

    /// Clear one account-scoped declaration for an explicit account (host-only).
    ///
    /// This exists for host cleanup paths (for example deleting a non-selected account's
    /// retained scoped declarations). App code should use [`Self::clear_sub`], which resolves
    /// account scope from the currently selected account.
    pub(crate) fn clear_sub_for_account(
        &mut self,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
    ) -> ClearSubResult {
        self.owners
            .clear_sub_for_account(self.runtime, self.pool, account_pubkey, identity)
    }

    /// Query aggregate EOSE state for one scoped subscription declaration.
    ///
    /// Thread example:
    /// - query the status of `{ owner: thread-view, key: replies-by-root(root_id), scope: Account }`
    /// - the lookup uses the current selected account to resolve `SubScope::Account`
    ///
    /// If the same thread root is open in multiple thread views, each owner can query the same
    /// shared outbox subscription status through its own identity.
    ///
    /// Account-switch behavior:
    /// - Switch away: status typically becomes [`ScopedSubEoseStatus::Inactive`] because the
    ///   live outbox subscription is removed while desired state is retained.
    /// - Switch back: status may return to `Live(...)` after restore.
    pub fn sub_eose_status(&self, identity: ScopedSubIdentity) -> ScopedSubEoseStatus {
        self.owners
            .sub_eose_status(self.runtime, self.pool, self.accounts, identity)
    }

    /// Drop one owner lifecycle and release all scoped subscriptions declared by it.
    ///
    /// Thread example:
    /// - `owner` is one thread view lifecycle token.
    /// - If two thread views opened the same `replies-by-root(root_id)` on the same account,
    ///   dropping one owner keeps the shared live outbox subscription active.
    /// - Dropping the last owner unsubscribes the live outbox subscription for that thread key
    ///   (if active) and removes the retained desired declaration.
    ///
    /// Account-scoped behavior:
    /// - If the thread owner is dropped while switched away, there may be no live outbox sub to
    ///   unsubscribe, but the retained declaration is removed so nothing is restored on switch-back.
    pub fn drop_owner(&mut self, owner: SubOwnerKey) -> bool {
        self.owners.drop_owner(self.runtime, self.pool, owner)
    }
}
