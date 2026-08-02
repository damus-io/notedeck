use enostr::Pubkey;
use hashbrown::HashMap;
use nostrdb::{Ndb, NoteKey, Subscription};
use notedeck::filter::HybridFilter;

use crate::timeline::sub::ndb_sub;

/// Per-account local timeline subscription state with ref-counting.
///
/// Remote timeline relay subscriptions are managed by scoped subs; this type
/// only tracks local NostrDB subscriptions and active dependers.
#[derive(Debug, Default)]
pub struct TimelineSub {
    by_account: HashMap<Pubkey, AccountSubState>,
}

/// Tracks whether the remote relay subscription has been registered with
/// `ScopedSubApi` for this (account, timeline) pair. The remote sub itself
/// lives in the scoped-subs system; this is just an "already asked" marker
/// so `is_timeline_ready` doesn't re-register the relay sub every frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RemoteSubStatus {
    #[default]
    Pending,
    Registered,
}

#[derive(Debug, Clone, Default)]
struct AccountSubState {
    local: Option<LocalSubState>,
    dependers: usize,
    remote_sub_status: RemoteSubStatus,
}

#[derive(Debug, Clone)]
struct LocalSubState {
    sub: Subscription,
    /// Note keys consumed from the local NDB subscription queue before their
    /// notes were visible to the insertion transaction.
    pending_polled_note_keys: Vec<NoteKey>,
}

impl LocalSubState {
    fn new(sub: Subscription) -> Self {
        Self {
            sub,
            pending_polled_note_keys: Vec::new(),
        }
    }
}

fn should_remove_account_state(state: &AccountSubState) -> bool {
    state.dependers == 0 && state.local.is_none()
}

fn unsubscribe_local_with_rollback(
    ndb: &mut Ndb,
    local: &mut Option<LocalSubState>,
    context: &str,
) {
    let Some(local_state) = local.take() else {
        return;
    };

    if let Err(e) = ndb.unsubscribe(local_state.sub) {
        tracing::error!("{context}: ndb unsubscribe failed: {e}");
        *local = Some(local_state);
    }
}

impl TimelineSub {
    fn state_for_account(&self, account_pk: &Pubkey) -> Option<&AccountSubState> {
        self.by_account.get(account_pk)
    }

    fn state_for_account_mut(&mut self, account_pk: Pubkey) -> &mut AccountSubState {
        self.by_account.entry(account_pk).or_default()
    }

    /// Reset one account's local subscription state while preserving its depender count.
    pub fn reset_for_account(&mut self, account_pk: Pubkey, ndb: &mut Ndb) {
        let mut remove_account_state = false;

        if let Some(state) = self.by_account.get_mut(&account_pk) {
            unsubscribe_local_with_rollback(
                ndb,
                &mut state.local,
                "TimelineSub::reset_for_account",
            );
            remove_account_state = should_remove_account_state(state);
        }

        if remove_account_state {
            self.by_account.remove(&account_pk);
        }
    }

    pub fn try_add_local(&mut self, account_pk: Pubkey, ndb: &Ndb, filter: &HybridFilter) {
        let state = self.state_for_account_mut(account_pk);
        if state.local.is_some() {
            return;
        }

        if let Some(sub) = ndb_sub(ndb, &filter.local().combined(), "") {
            state.local = Some(LocalSubState::new(sub));
        }
    }

    pub fn increment(&mut self, account_pk: Pubkey) {
        self.state_for_account_mut(account_pk).dependers += 1;
    }

    pub fn is_remote_registered(&self, account_pk: &Pubkey) -> bool {
        self.state_for_account(account_pk)
            .is_some_and(|state| state.remote_sub_status == RemoteSubStatus::Registered)
    }

    pub fn mark_remote_registered(&mut self, account_pk: Pubkey) {
        self.state_for_account_mut(account_pk).remote_sub_status = RemoteSubStatus::Registered;
    }

    pub fn mark_remote_pending(&mut self, account_pk: Pubkey) {
        if let Some(state) = self.by_account.get_mut(&account_pk) {
            state.remote_sub_status = RemoteSubStatus::Pending;
        }
    }

    pub fn get_local(&self, account_pk: &Pubkey) -> Option<Subscription> {
        self.state_for_account(account_pk)
            .and_then(|state| state.local.as_ref().map(|local| local.sub))
    }

    pub fn clear_pending_polled_note_keys(&mut self) {
        for state in self.by_account.values_mut() {
            if let Some(local) = &mut state.local {
                local.pending_polled_note_keys.clear();
            }
        }
    }

    pub fn take_pending_or_poll(
        &mut self,
        account_pk: &Pubkey,
        ndb: &Ndb,
        limit: u32,
    ) -> Option<Vec<NoteKey>> {
        let state = self.by_account.get_mut(account_pk)?;
        let local = state.local.as_mut()?;
        if !local.pending_polled_note_keys.is_empty() {
            return Some(std::mem::take(&mut local.pending_polled_note_keys));
        }

        Some(ndb.poll_for_notes(local.sub, limit))
    }

    pub fn push_pending_polled_note_keys(
        &mut self,
        account_pk: Pubkey,
        note_keys: impl IntoIterator<Item = NoteKey>,
    ) {
        let Some(state) = self.by_account.get_mut(&account_pk) else {
            return;
        };
        let Some(local) = &mut state.local else {
            return;
        };
        local.pending_polled_note_keys.extend(note_keys);
    }

    pub fn unsubscribe_or_decrement(&mut self, account_pk: Pubkey, ndb: &mut Ndb) {
        let mut remove_account_state = false;
        if let Some(state) = self.by_account.get_mut(&account_pk) {
            if state.dependers > 1 {
                state.dependers = state.dependers.saturating_sub(1);
                return;
            }

            state.dependers = state.dependers.saturating_sub(1);
            state.remote_sub_status = RemoteSubStatus::Pending;
            unsubscribe_local_with_rollback(
                ndb,
                &mut state.local,
                "TimelineSub::unsubscribe_or_decrement",
            );
            remove_account_state = should_remove_account_state(state);
        }

        if remove_account_state {
            self.by_account.remove(&account_pk);
        }
    }

    pub fn no_sub(&self, account_pk: &Pubkey) -> bool {
        self.state_for_account(account_pk)
            .is_none_or(|state| state.dependers == 0)
    }

    pub fn has_any_subs(&self) -> bool {
        !self.by_account.is_empty()
    }

    pub fn dependers(&self, account_pk: &Pubkey) -> usize {
        self.state_for_account(account_pk)
            .map_or(0, |state| state.dependers)
    }

    pub fn accounts_with_dependers(&self) -> Vec<Pubkey> {
        self.by_account
            .iter()
            .filter_map(|(account_pk, state)| (state.dependers > 0).then_some(*account_pk))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb =
            Ndb::new(tmp.path().to_str().expect("path"), &nostrdb::Config::new()).expect("ndb");
        (tmp, ndb)
    }

    #[test]
    fn accounts_with_dependers_omits_unsubscribed_account() {
        let account_a = Pubkey::new([0xA1; 32]);
        let account_b = Pubkey::new([0xB2; 32]);
        let mut sub = TimelineSub::default();
        let (_tmp, mut ndb) = new_ndb();

        sub.increment(account_a);
        sub.increment(account_b);
        sub.mark_remote_registered(account_b);
        sub.unsubscribe_or_decrement(account_b, &mut ndb);

        assert_eq!(sub.accounts_with_dependers(), vec![account_a]);
        assert!(!sub.is_remote_registered(&account_b));
    }
}
