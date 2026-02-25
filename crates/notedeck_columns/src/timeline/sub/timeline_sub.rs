use notedeck::{filter::HybridFilter, UnifiedSubscription};

use enostr::Pubkey;
use enostr::RelayPool;
use hashbrown::HashMap;
use nostrdb::{Ndb, Subscription};

use crate::{subscriptions, timeline::sub::ndb_sub};

fn unsubscribe_local(ndb: &mut Ndb, local: Subscription, context: &str) -> bool {
    if let Err(e) = ndb.unsubscribe(local) {
        tracing::error!("{context}: failed to unsubscribe from ndb: {e}");
        return false;
    }

    true
}

/// Per-account timeline subscription state with ref-counting.
///
/// This still manages legacy relay-pool remote subscriptions for now; scoped-sub
/// remote ownership is migrated in a follow-up refactor.
#[derive(Debug, Default)]
pub struct TimelineSub {
    filter: Option<HybridFilter>,
    by_account: HashMap<Pubkey, SubState>,
}

#[derive(Debug, Clone)]
enum SubState {
    NoSub {
        dependers: usize,
    },
    LocalOnly {
        local: Subscription,
        dependers: usize,
    },
    RemoteOnly {
        remote: String,
        dependers: usize,
    },
    Unified {
        unified: UnifiedSubscription,
        dependers: usize,
    },
}

impl Default for SubState {
    fn default() -> Self {
        Self::NoSub { dependers: 0 }
    }
}

impl TimelineSub {
    fn state_for_account(&self, account_pk: &Pubkey) -> SubState {
        self.by_account.get(account_pk).cloned().unwrap_or_default()
    }

    fn set_state_for_account(&mut self, account_pk: Pubkey, state: SubState) {
        if matches!(state, SubState::NoSub { dependers: 0 }) {
            self.by_account.remove(&account_pk);
            return;
        }

        self.by_account.insert(account_pk, state);
    }

    /// Reset one account's subscription state while preserving its depender count.
    pub fn reset_for_account(&mut self, account_pk: Pubkey, ndb: &mut Ndb, pool: &mut RelayPool) {
        let before = self.state_for_account(&account_pk);

        let next = match before.clone() {
            SubState::NoSub { dependers } => SubState::NoSub { dependers },
            SubState::LocalOnly { local, dependers } => {
                if !unsubscribe_local(ndb, local, "TimelineSub::reset_for_account") {
                    return;
                }
                SubState::NoSub { dependers }
            }
            SubState::RemoteOnly { remote, dependers } => {
                pool.unsubscribe(remote);
                SubState::NoSub { dependers }
            }
            SubState::Unified { unified, dependers } => {
                pool.unsubscribe(unified.remote.clone());
                if !unsubscribe_local(ndb, unified.local, "TimelineSub::reset_for_account") {
                    self.set_state_for_account(
                        account_pk,
                        SubState::LocalOnly {
                            local: unified.local,
                            dependers,
                        },
                    );
                    return;
                }
                SubState::NoSub { dependers }
            }
        };

        self.set_state_for_account(account_pk, next);
        self.filter = None;

        tracing::debug!(
            "TimelineSub::reset_for_account({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    pub fn try_add_local(&mut self, account_pk: Pubkey, ndb: &Ndb, filter: &HybridFilter) {
        let before = self.state_for_account(&account_pk);

        let Some(next) = (match before.clone() {
            SubState::NoSub { dependers } => {
                let Some(sub) = ndb_sub(ndb, &filter.local().combined(), "") else {
                    return;
                };
                self.filter = Some(filter.to_owned());
                Some(SubState::LocalOnly {
                    local: sub,
                    dependers,
                })
            }
            SubState::LocalOnly { .. } => None,
            SubState::RemoteOnly { remote, dependers } => {
                let Some(local) = ndb_sub(ndb, &filter.local().combined(), "") else {
                    return;
                };
                Some(SubState::Unified {
                    unified: UnifiedSubscription { local, remote },
                    dependers,
                })
            }
            SubState::Unified { .. } => None,
        }) else {
            return;
        };

        self.set_state_for_account(account_pk, next);

        tracing::debug!(
            "TimelineSub::try_add_local({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    pub fn force_add_remote(&mut self, account_pk: Pubkey, subid: String) {
        let before = self.state_for_account(&account_pk);

        let next = match before.clone() {
            SubState::NoSub { dependers } => SubState::RemoteOnly {
                remote: subid,
                dependers,
            },
            SubState::LocalOnly { local, dependers } => SubState::Unified {
                unified: UnifiedSubscription {
                    local,
                    remote: subid,
                },
                dependers,
            },
            SubState::RemoteOnly { .. } | SubState::Unified { .. } => return,
        };

        self.set_state_for_account(account_pk, next);

        tracing::debug!(
            "TimelineSub::force_add_remote({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    pub fn try_add_remote(
        &mut self,
        account_pk: Pubkey,
        pool: &mut RelayPool,
        filter: &HybridFilter,
    ) {
        let before = self.state_for_account(&account_pk);

        let next = match before.clone() {
            SubState::NoSub { dependers } => {
                let subid = subscriptions::new_sub_id();
                pool.subscribe(subid.clone(), filter.remote().to_vec());
                self.filter = Some(filter.to_owned());
                SubState::RemoteOnly {
                    remote: subid,
                    dependers,
                }
            }
            SubState::LocalOnly { local, dependers } => {
                let subid = subscriptions::new_sub_id();
                pool.subscribe(subid.clone(), filter.remote().to_vec());
                self.filter = Some(filter.to_owned());
                SubState::Unified {
                    unified: UnifiedSubscription {
                        local,
                        remote: subid,
                    },
                    dependers,
                }
            }
            SubState::RemoteOnly { .. } | SubState::Unified { .. } => return,
        };

        self.set_state_for_account(account_pk, next);

        tracing::debug!(
            "TimelineSub::try_add_remote({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    pub fn increment(&mut self, account_pk: Pubkey) {
        let before = self.state_for_account(&account_pk);

        let next = match before.clone() {
            SubState::NoSub { dependers } => SubState::NoSub {
                dependers: dependers + 1,
            },
            SubState::LocalOnly { local, dependers } => SubState::LocalOnly {
                local,
                dependers: dependers + 1,
            },
            SubState::RemoteOnly { remote, dependers } => SubState::RemoteOnly {
                remote,
                dependers: dependers + 1,
            },
            SubState::Unified { unified, dependers } => SubState::Unified {
                unified,
                dependers: dependers + 1,
            },
        };

        self.set_state_for_account(account_pk, next);

        tracing::debug!(
            "TimelineSub::increment({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    pub fn get_local(&self, account_pk: &Pubkey) -> Option<Subscription> {
        match self.state_for_account(account_pk) {
            SubState::NoSub { dependers: _ } => None,
            SubState::LocalOnly {
                local,
                dependers: _,
            } => Some(local),
            SubState::RemoteOnly {
                remote: _,
                dependers: _,
            } => None,
            SubState::Unified {
                unified,
                dependers: _,
            } => Some(unified.local),
        }
    }

    pub fn unsubscribe_or_decrement(
        &mut self,
        account_pk: Pubkey,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
    ) {
        let before = self.state_for_account(&account_pk);

        let next = match before.clone() {
            SubState::NoSub { dependers } => SubState::NoSub {
                dependers: dependers.saturating_sub(1),
            },
            SubState::LocalOnly { local, dependers } => {
                if dependers > 1 {
                    return self.set_and_log_after_decrement(
                        account_pk,
                        before,
                        SubState::LocalOnly {
                            local,
                            dependers: dependers.saturating_sub(1),
                        },
                    );
                }

                // Keep local state intact if NDB unsubscribe fails.
                if !unsubscribe_local(ndb, local, "TimelineSub::unsubscribe_or_decrement") {
                    return;
                }

                SubState::NoSub { dependers: 0 }
            }
            SubState::RemoteOnly { remote, dependers } => {
                if dependers > 1 {
                    return self.set_and_log_after_decrement(
                        account_pk,
                        before,
                        SubState::RemoteOnly {
                            remote,
                            dependers: dependers.saturating_sub(1),
                        },
                    );
                }

                pool.unsubscribe(remote);
                SubState::NoSub { dependers: 0 }
            }
            SubState::Unified { unified, dependers } => {
                if dependers > 1 {
                    return self.set_and_log_after_decrement(
                        account_pk,
                        before,
                        SubState::Unified {
                            unified,
                            dependers: dependers.saturating_sub(1),
                        },
                    );
                }

                pool.unsubscribe(unified.remote.clone());

                // Remote is already gone above; fall back to local-only on NDB failure.
                if !unsubscribe_local(ndb, unified.local, "TimelineSub::unsubscribe_or_decrement") {
                    SubState::LocalOnly {
                        local: unified.local,
                        dependers,
                    }
                } else {
                    SubState::NoSub { dependers: 0 }
                }
            }
        };

        self.set_state_for_account(account_pk, next);
        tracing::debug!(
            "TimelineSub::unsubscribe_or_decrement({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    fn set_and_log_after_decrement(
        &mut self,
        account_pk: Pubkey,
        before: SubState,
        next: SubState,
    ) {
        self.set_state_for_account(account_pk, next);
        tracing::debug!(
            "TimelineSub::unsubscribe_or_decrement({account_pk:?}): {:?} => {:?}",
            before,
            self.state_for_account(&account_pk)
        );
    }

    pub fn get_filter(&self) -> Option<&HybridFilter> {
        self.filter.as_ref()
    }

    pub fn no_sub(&self, account_pk: &Pubkey) -> bool {
        matches!(
            self.state_for_account(account_pk),
            SubState::NoSub { dependers: _ }
        )
    }

    pub fn has_any_subs(&self) -> bool {
        !self.by_account.is_empty()
    }

    pub fn dependers(&self, account_pk: &Pubkey) -> usize {
        match self.state_for_account(account_pk) {
            SubState::NoSub { dependers } => dependers,
            SubState::LocalOnly {
                local: _,
                dependers,
            } => dependers,
            SubState::RemoteOnly {
                remote: _,
                dependers,
            } => dependers,
            SubState::Unified {
                unified: _,
                dependers,
            } => dependers,
        }
    }
}
