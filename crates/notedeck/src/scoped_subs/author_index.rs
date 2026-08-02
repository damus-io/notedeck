use enostr::Pubkey;
use hashbrown::{HashMap, HashSet};

use super::config::ScopedSubKey;

/// Derived active membership for scoped subscriptions with author-outbox routing.
///
/// `ScopedSubRuntime::desired` remains the authoritative `SubConfig` store. This
/// index only keeps author membership needed to avoid rebuilding author-outbox
/// demand from `desired` every frame.
#[derive(Clone, Debug, Default)]
pub(super) struct AuthorOutboxDemandIndex {
    applied_account_scope: Option<Pubkey>,
    authors_by_scoped: HashMap<ScopedSubKey, HashSet<Pubkey>>,
    active_author_counts: HashMap<Pubkey, usize>,
}

impl AuthorOutboxDemandIndex {
    /// Apply the account scope represented by this derived author-demand view.
    pub(super) fn apply_account_scope(&mut self, account_scope: Pubkey) {
        if self.applied_account_scope == Some(account_scope) {
            return;
        }

        self.applied_account_scope = Some(account_scope);
        self.rebuild_active_authors(account_scope);
    }

    fn rebuild_active_authors(&mut self, account_scope: Pubkey) {
        let mut active_author_counts = HashMap::new();
        for (scoped, authors) in &self.authors_by_scoped {
            if !scoped.is_active_for_account(account_scope) {
                continue;
            }

            for author in authors.iter().copied() {
                *active_author_counts.entry(author).or_insert(0) += 1;
            }
        }
        self.active_author_counts = active_author_counts;
    }

    /// Upsert derived authors for one author-outbox scoped key.
    pub(super) fn upsert_scoped(
        &mut self,
        scoped: ScopedSubKey,
        next_authors: HashSet<Pubkey>,
        active: bool,
    ) {
        let previous_authors = self
            .authors_by_scoped
            .insert(scoped.clone(), next_authors.clone())
            .unwrap_or_default();

        if !active {
            return;
        }

        for author in previous_authors.difference(&next_authors).copied() {
            self.decrement_author(author);
        }

        for author in next_authors.difference(&previous_authors).copied() {
            self.increment_author(author);
        }
    }

    /// Remove all derived demand for one scoped key.
    pub(super) fn remove_scoped(&mut self, scoped: &ScopedSubKey, active: bool) {
        let previous_authors = self.authors_by_scoped.remove(scoped).unwrap_or_default();

        if !active {
            return;
        }

        for author in previous_authors {
            self.decrement_author(author);
        }
    }

    /// Return the active author set without scanning retained `SubConfig`s.
    #[cfg(test)]
    pub(super) fn active_authors(&self) -> HashSet<Pubkey> {
        self.active_author_counts.keys().copied().collect()
    }

    fn increment_author(&mut self, author: Pubkey) {
        *self.active_author_counts.entry(author).or_insert(0) += 1;
    }

    fn decrement_author(&mut self, author: Pubkey) {
        let Some(count) = self.active_author_counts.get_mut(&author) else {
            return;
        };

        if *count > 1 {
            *count -= 1;
            return;
        }

        self.active_author_counts.remove(&author);
    }
}
