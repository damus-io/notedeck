use enostr::Pubkey;

use super::{
    declaration_cache::{DeclarationCacheTransition, ScopedSubDeclarationCache},
    ClearSubResult, EnsureSubResult, ScopedSubCommand, ScopedSubIdentity, SetSubResult, SubConfig,
    SubKey, SubOwnerKey, SubScope,
};
use crate::remote_data::{RemoteIntent, RemoteIntentBatchBuilder};

/// UI-thread scoped-sub declaration state plus bridge command delivery.
///
/// This is synchronous from the app-facing API's perspective: it updates the
/// local declaration cache, emits a bridge command only for real declaration
/// transitions, and reports whether the temporary UI-owned runtime path should
/// still run.
#[derive(Default)]
pub(super) struct ScopedSubDeclarations {
    cache: ScopedSubDeclarationCache,
}

impl ScopedSubDeclarations {
    pub(super) fn scoped_key(
        selected_account_pubkey: Pubkey,
        scope: SubScope,
        key: SubKey,
    ) -> super::config::ScopedSubKey {
        ScopedSubDeclarationCache::scoped_key(selected_account_pubkey, scope, key)
    }

    pub(super) fn owner_owns(
        &self,
        owner: SubOwnerKey,
        scoped: &super::config::ScopedSubKey,
    ) -> bool {
        self.cache.owner_owns(owner, scoped)
    }

    pub(super) fn set_sub(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
        config: &SubConfig,
    ) -> DeclarationCacheTransition<SetSubResult> {
        let scoped = Self::scoped_key(account_pubkey, identity.scope, identity.key);
        let transition = self.cache.set_sub(scoped, identity.owner, config.clone());
        if transition.forward_to_runtime {
            self.queue_scoped_command(
                batch,
                ScopedSubCommand::set_owner_config(
                    account_pubkey,
                    identity.owner,
                    identity.scope,
                    identity.key,
                    config.clone(),
                ),
            );
        }
        transition
    }

    pub(super) fn set_sub_for_account(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        config: &SubConfig,
    ) -> DeclarationCacheTransition<SetSubResult> {
        let scoped = Self::scoped_key(account_pubkey, SubScope::Account, key);
        let transition = self.cache.set_sub(scoped, owner, config.clone());
        if transition.forward_to_runtime {
            self.queue_scoped_command(
                batch,
                ScopedSubCommand::set_owner_config(
                    account_pubkey,
                    owner,
                    SubScope::Account,
                    key,
                    config.clone(),
                ),
            );
        }
        transition
    }

    pub(super) fn ensure_sub(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
        config: &SubConfig,
    ) -> DeclarationCacheTransition<EnsureSubResult> {
        let scoped = Self::scoped_key(account_pubkey, identity.scope, identity.key);
        let transition = self.cache.ensure_sub(scoped, identity.owner);
        self.send_ensure_transition_command(batch, account_pubkey, identity, config, &transition);
        transition
    }

    pub(super) fn ensure_sub_for_account(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        key: SubKey,
        config: &SubConfig,
    ) -> DeclarationCacheTransition<EnsureSubResult> {
        let scoped = Self::scoped_key(account_pubkey, SubScope::Account, key);
        let transition = self.cache.ensure_sub(scoped, owner);
        let identity = ScopedSubIdentity::account(owner, key);
        self.send_ensure_transition_command(batch, account_pubkey, identity, config, &transition);
        transition
    }

    pub(super) fn clear_sub(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
    ) -> ClearSubResult {
        let scoped = Self::scoped_key(account_pubkey, identity.scope, identity.key);
        let result = self.cache.clear_sub(&scoped, identity.owner);
        if !matches!(result, ClearSubResult::NotFound) {
            self.queue_scoped_command(
                batch,
                ScopedSubCommand::ClearOwnerConfig {
                    account_pubkey,
                    owner: identity.owner,
                    scope: identity.scope,
                    key: identity.key,
                },
            );
        }
        result
    }

    pub(super) fn drop_owner(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        owner: SubOwnerKey,
    ) -> bool {
        if !self.cache.drop_owner(owner) {
            return false;
        }

        self.queue_scoped_command(batch, ScopedSubCommand::DropOwner { owner });
        true
    }

    pub(super) fn purge_account_scope(
        &mut self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
    ) {
        self.cache
            .purge_scope(&super::config::ResolvedSubScope::Account(account_pubkey));
        self.queue_scoped_command(batch, ScopedSubCommand::PurgeAccount { account_pubkey });
    }

    fn queue_scoped_command(
        &self,
        batch: &mut RemoteIntentBatchBuilder,
        command: ScopedSubCommand,
    ) {
        batch.push(RemoteIntent::ScopedSub(command));
    }

    fn send_ensure_transition_command(
        &self,
        batch: &mut RemoteIntentBatchBuilder,
        account_pubkey: Pubkey,
        identity: ScopedSubIdentity,
        config: &SubConfig,
        transition: &DeclarationCacheTransition<EnsureSubResult>,
    ) {
        if !transition.forward_to_runtime {
            return;
        }

        self.queue_scoped_command(
            batch,
            ScopedSubCommand::ensure_owner_config(
                account_pubkey,
                identity.owner,
                identity.scope,
                identity.key,
                config.clone(),
            ),
        );
    }
}
