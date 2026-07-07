use enostr::Pubkey;

use super::declarations::ScopedSubDeclarations;
use super::{config::ScopedSubKey, state::ScopedSubReadModel};
use super::{
    ClearSubResult, EnsureSubResult, ScopedSubIdentity, ScopedSubReadiness, SetSubResult,
    SubConfig, SubKey, SubOwnerKey,
};
use crate::{remote_data::RemoteIntentBatchBuilder, Accounts};

/// App-facing facade over scoped subscription owner/runtime operations.
///
/// This bundles host resources that are commonly passed together to avoid
/// argument plumbing through app-layer helper functions.
pub struct ScopedSubApi<'o> {
    accounts: &'o Accounts,
    declarations: &'o mut ScopedSubDeclarations,
    read_model: &'o ScopedSubReadModel,
    batch: &'o mut RemoteIntentBatchBuilder,
}

impl<'o> ScopedSubApi<'o> {
    pub(super) fn new(
        accounts: &'o Accounts,
        declarations: &'o mut ScopedSubDeclarations,
        read_model: &'o ScopedSubReadModel,
        batch: &'o mut RemoteIntentBatchBuilder,
    ) -> Self {
        Self {
            accounts,
            declarations,
            read_model,
            batch,
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
    /// `set_sub(...)` upserts this owner's declaration for the resolved
    /// `(scope, key)`. The runtime derives one effective config for the shared
    /// key; compatible additive relay coverage is merged across owners.
    ///
    /// The transition is:
    /// - first call creates desired state
    /// - repeated calls with a canonically unchanged `SubConfig` return
    ///   `SetSubResult::Unchanged` without refreshing live/full-history state
    /// - repeated calls with a changed `SubConfig` return `SetSubResult::Updated`
    ///   after replacing desired state and updating the live outbox sub
    ///
    /// Use [`Self::ensure_sub`] when an existing declaration must never be
    /// mutated by a later caller.
    ///
    /// Account-scoped behavior (`SubScope::Account`):
    /// - On switch away, the live outbox subscription is unsubscribed.
    /// - Desired state is retained while owners still exist.
    /// - On switch back, the live outbox subscription is restored from desired state.
    /// - If owners are dropped while away, nothing is restored.
    pub fn set_sub(&mut self, identity: ScopedSubIdentity, config: SubConfig) -> SetSubResult {
        let transition = self.declarations.set_sub(
            self.batch,
            self.selected_account_pubkey(),
            identity,
            &config,
        );
        transition.result
    }

    /// Create or update one account-scoped declaration for an explicit account.
    ///
    /// This is for retained app state whose account may not currently be
    /// selected. Selected-account calls realize live relay state normally;
    /// inactive-account calls update retained desired state only.
    pub fn set_sub_for_account(
        &mut self,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        config: SubConfig,
    ) -> SetSubResult {
        let transition =
            self.declarations
                .set_sub_for_account(self.batch, account_pubkey, owner, key, &config);
        transition.result
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
        let transition = self.declarations.ensure_sub(
            self.batch,
            self.selected_account_pubkey(),
            identity,
            &config,
        );
        transition.result
    }

    /// Create one account-scoped declaration for an explicit account if absent.
    ///
    /// Inactive-account calls retain desired state for later account-switch
    /// restore without opening relay subscriptions for the inactive account.
    pub fn ensure_sub_for_account(
        &mut self,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        config: SubConfig,
    ) -> EnsureSubResult {
        let transition = self.declarations.ensure_sub_for_account(
            self.batch,
            account_pubkey,
            owner,
            key,
            &config,
        );
        transition.result
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
        self.declarations
            .clear_sub(self.batch, self.selected_account_pubkey(), identity)
    }

    /// Query readiness for one scoped subscription declaration.
    ///
    /// Thread example:
    /// - query the status of `{ owner: thread-view, key: replies-by-root(root_id), scope: Account }`
    /// - the lookup uses the current selected account to resolve `SubScope::Account`
    ///
    /// If the same thread root is open in multiple thread views, each owner can query the same
    /// shared outbox subscription status through its own identity.
    ///
    /// Account-switch behavior:
    /// - Switch away: readiness typically becomes [`ScopedSubReadiness::Inactive`] because the
    ///   live outbox subscription is removed while desired state is retained.
    /// - Switch back: readiness may return to `Live(...)` after restore.
    pub fn sub_readiness(&self, identity: ScopedSubIdentity) -> ScopedSubReadiness {
        self.sub_readiness_for_account(self.selected_account_pubkey(), identity)
    }

    /// Query readiness for one declaration resolved against an explicit account.
    ///
    /// This mirrors [`Self::set_sub_for_account`] and
    /// [`Self::ensure_sub_for_account`]: `SubScope::Account` resolves to
    /// `account_pubkey`, while `SubScope::Global` remains global.
    pub fn sub_readiness_for_account(
        &self,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
    ) -> ScopedSubReadiness {
        let scoped = self.scoped_key_for_account(account_pubkey, identity);
        if !self.declarations.owner_owns(identity.owner, &scoped) {
            return ScopedSubReadiness::Missing;
        }
        if let Some(readiness) = self.read_model.readiness(&scoped) {
            return readiness;
        }

        ScopedSubReadiness::Inactive
    }

    fn scoped_key_for_account(
        &self,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
    ) -> ScopedSubKey {
        ScopedSubDeclarations::scoped_key(account_pubkey, identity.scope, identity.key)
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
        self.declarations.drop_owner(self.batch, owner)
    }

    /// Permanently clear all retained scoped subscriptions for a deleted account.
    pub(crate) fn purge_account_scope(&mut self, account_pk: Pubkey) {
        self.declarations
            .purge_account_scope(self.batch, account_pk);
    }
}
