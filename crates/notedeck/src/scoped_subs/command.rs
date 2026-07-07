use enostr::Pubkey;

use super::config::{SubConfig, SubKey, SubOwnerKey, SubScope};

/// UI-to-bridge scoped-sub state command.
///
/// Variants carry owner/config intent. Selected-account state is sent through
/// the top-level remote bridge account command.
pub(crate) enum ScopedSubCommand {
    SetOwnerConfig {
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    },
    EnsureOwnerConfig {
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    },
    ClearOwnerConfig {
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
    },
    DropOwner {
        owner: SubOwnerKey,
    },
    PurgeAccount {
        account_pubkey: Pubkey,
    },
}

impl ScopedSubCommand {
    pub(crate) fn set_owner_config(
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> Self {
        Self::SetOwnerConfig {
            account_pubkey,
            owner,
            scope,
            key,
            config,
        }
    }

    pub(crate) fn ensure_owner_config(
        account_pubkey: Pubkey,
        owner: SubOwnerKey,
        scope: SubScope,
        key: SubKey,
        config: SubConfig,
    ) -> Self {
        Self::EnsureOwnerConfig {
            account_pubkey,
            owner,
            scope,
            key,
            config,
        }
    }
}
