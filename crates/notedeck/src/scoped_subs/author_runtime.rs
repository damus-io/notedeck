use enostr::Pubkey;
use hashbrown::HashSet;

use crate::author_outbox::filter_author_pubkeys;

use super::author_index::AuthorOutboxDemandIndex;
use super::config::{ScopedSubKey, SubConfig};

/// Scoped-sub author-outbox author membership state.
#[derive(Default)]
pub(super) struct ScopedAuthorOutboxRuntime {
    index: AuthorOutboxDemandIndex,
}

impl ScopedAuthorOutboxRuntime {
    /// Apply the selected account scope for derived author-outbox demand.
    pub(super) fn apply_selected_account(&mut self, selected_account_pubkey: Pubkey) {
        self.index.apply_account_scope(selected_account_pubkey);
    }

    /// Reconcile retained author-outbox demand for one effective scoped-sub transition.
    pub(super) fn retain_transition(
        &mut self,
        scoped: ScopedSubKey,
        previous: Option<&SubConfig>,
        next: Option<&SubConfig>,
        active: bool,
    ) {
        let Some(next) = next else {
            self.release_transition(&scoped, previous, active);
            return;
        };

        self.retain_config_authors(
            scoped,
            previous,
            next,
            sub_config_author_pubkeys(next),
            active,
        );
    }

    /// Remove retained author demand when the owning desired config is released.
    pub(super) fn release_transition(
        &mut self,
        scoped: &ScopedSubKey,
        removed: Option<&SubConfig>,
        active: bool,
    ) {
        if removed.is_some_and(SubConfig::uses_author_outbox) {
            self.index.remove_scoped(scoped, active);
        }
    }

    /// Return active author demand for scoped-sub tests.
    #[cfg(test)]
    pub(super) fn active_authors_for_test(&self) -> HashSet<Pubkey> {
        self.index.active_authors()
    }

    fn retain_config_authors(
        &mut self,
        scoped: ScopedSubKey,
        previous: Option<&SubConfig>,
        next: &SubConfig,
        next_authors: HashSet<Pubkey>,
        active: bool,
    ) {
        if previous.is_some_and(SubConfig::uses_author_outbox) && !next.uses_author_outbox() {
            self.index.remove_scoped(&scoped, active);
        }

        if next.uses_author_outbox() {
            self.index.upsert_scoped(scoped, next_authors, active);
        }
    }
}

pub(super) fn sub_config_author_pubkeys(config: &SubConfig) -> HashSet<Pubkey> {
    let live_authors = config
        .filters()
        .iter()
        .flat_map(|filter| filter_author_pubkeys(filter.as_filter()))
        .collect::<HashSet<_>>();
    let Some(full_history) = config.full_history_config() else {
        return live_authors;
    };

    live_authors
        .into_iter()
        .chain(
            full_history
                .filters()
                .iter()
                .flat_map(|filter| filter_author_pubkeys(filter.as_filter())),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        scoped_subs::config::ResolvedSubScope, RelayRoutingPreference, SubKey, SubRelayPolicy,
    };
    use enostr::RelayDemandPriority;
    use nostrdb::Filter;

    fn author_outbox_config(author: Pubkey) -> SubConfig {
        let baseline = SubRelayPolicy::new(
            RelayDemandPriority::Important,
            RelayRoutingPreference::default(),
        );
        let author_outbox = SubRelayPolicy::new(
            RelayDemandPriority::Opportunistic,
            RelayRoutingPreference::NoPreference,
        );

        SubConfig::builder(vec![Filter::new()
            .authors([author.bytes()])
            .kinds([1])
            .limit(20)
            .build()])
        .accounts_read(baseline)
        .with_author_outbox(author_outbox)
        .build()
    }

    fn scoped_key(account: Pubkey, key: &str) -> ScopedSubKey {
        ScopedSubKey {
            scope: ResolvedSubScope::Account(account),
            key: SubKey::new(key),
        }
    }

    #[test]
    fn inactive_account_demand_does_not_affect_selected_authors() {
        let selected = Pubkey::new([0x01; 32]);
        let inactive = Pubkey::new([0x02; 32]);
        let author = Pubkey::new([0xA1; 32]);
        let scoped = scoped_key(inactive, "inactive-author");
        let config = author_outbox_config(author);
        let mut runtime = ScopedAuthorOutboxRuntime::default();

        runtime.apply_selected_account(selected);
        runtime.retain_transition(scoped.clone(), None, Some(&config), false);

        assert_eq!(runtime.active_authors_for_test(), HashSet::new());
    }
}
