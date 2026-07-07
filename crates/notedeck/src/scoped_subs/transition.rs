use enostr::Pubkey;

use super::config::{ScopedSubKey, SubConfig};

/// Active effective config that should be planned and realized.
pub(super) struct ActiveSubTransition<'a> {
    pub(super) scoped: ScopedSubKey,
    pub(super) previous: Option<&'a SubConfig>,
    pub(super) next: &'a SubConfig,
}

/// Effective scoped-sub transition after owner declarations have been merged.
pub(super) enum EffectiveSubTransition<'a> {
    Removed {
        scoped: ScopedSubKey,
        previous: Option<&'a SubConfig>,
    },
    Inactive {
        scoped: ScopedSubKey,
        previous: Option<&'a SubConfig>,
        next: &'a SubConfig,
    },
    Active(ActiveSubTransition<'a>),
}

/// Classify one effective config transition against the selected account.
pub(super) fn effective_sub_transition<'a>(
    selected_account_pubkey: Pubkey,
    scoped: ScopedSubKey,
    previous: Option<&'a SubConfig>,
    next: Option<&'a SubConfig>,
) -> EffectiveSubTransition<'a> {
    let Some(next) = next else {
        return EffectiveSubTransition::Removed { scoped, previous };
    };

    if !scoped.is_active_for_account(selected_account_pubkey) {
        return EffectiveSubTransition::Inactive {
            scoped,
            previous,
            next,
        };
    }

    EffectiveSubTransition::Active(ActiveSubTransition {
        scoped,
        previous,
        next,
    })
}
