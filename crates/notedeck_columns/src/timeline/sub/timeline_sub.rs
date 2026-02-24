use notedeck::{filter::HybridFilter, UnifiedSubscription};

use enostr::RelayPool;
use nostrdb::{Ndb, Subscription};

use crate::{subscriptions, timeline::sub::ndb_sub};

#[derive(Debug)]
pub struct TimelineSub {
    filter: Option<HybridFilter>,
    state: SubState,
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

impl Default for TimelineSub {
    fn default() -> Self {
        Self {
            state: SubState::NoSub { dependers: 0 },
            filter: None,
        }
    }
}

impl TimelineSub {
    /// Reset the subscription state, properly unsubscribing from ndb and
    /// relay pool before clearing.
    ///
    /// Used when the contact list changes and we need to rebuild the
    /// timeline with a new filter. Preserves the depender count so that
    /// shared subscription reference counting remains correct.
    pub fn reset(&mut self, ndb: &mut Ndb, pool: &mut RelayPool) {
        let before = self.state.clone();

        let dependers = match &self.state {
            SubState::NoSub { dependers } => *dependers,

            SubState::LocalOnly { local, dependers } => {
                if let Err(e) = ndb.unsubscribe(*local) {
                    tracing::error!("TimelineSub::reset: failed to unsubscribe from ndb: {e}");
                }
                *dependers
            }

            SubState::RemoteOnly { remote, dependers } => {
                pool.unsubscribe(remote.to_owned());
                *dependers
            }

            SubState::Unified { unified, dependers } => {
                pool.unsubscribe(unified.remote.to_owned());
                if let Err(e) = ndb.unsubscribe(unified.local) {
                    tracing::error!("TimelineSub::reset: failed to unsubscribe from ndb: {e}");
                }
                *dependers
            }
        };

        self.state = SubState::NoSub { dependers };
        self.filter = None;

        tracing::debug!("TimelineSub::reset: {:?} => {:?}", before, self.state);
    }

    pub fn try_add_local(&mut self, ndb: &Ndb, filter: &HybridFilter) {
        let before = self.state.clone();
        match &mut self.state {
            SubState::NoSub { dependers } => {
                let Some(sub) = ndb_sub(ndb, &filter.local().combined(), "") else {
                    return;
                };

                self.filter = Some(filter.to_owned());
                self.state = SubState::LocalOnly {
                    local: sub,
                    dependers: *dependers,
                }
            }
            SubState::LocalOnly {
                local: _,
                dependers: _,
            } => {}
            SubState::RemoteOnly { remote, dependers } => {
                let Some(local) = ndb_sub(ndb, &filter.local().combined(), "") else {
                    return;
                };
                self.state = SubState::Unified {
                    unified: UnifiedSubscription {
                        local,
                        remote: remote.to_owned(),
                    },
                    dependers: *dependers,
                };
            }
            SubState::Unified {
                unified: _,
                dependers: _,
            } => {}
        }
        tracing::debug!(
            "TimelineSub::try_add_local: {:?} => {:?}",
            before,
            self.state
        );
    }

    pub fn force_add_remote(&mut self, subid: String) {
        let before = self.state.clone();
        match &mut self.state {
            SubState::NoSub { dependers } => {
                self.state = SubState::RemoteOnly {
                    remote: subid,
                    dependers: *dependers,
                }
            }
            SubState::LocalOnly { local, dependers } => {
                self.state = SubState::Unified {
                    unified: UnifiedSubscription {
                        local: *local,
                        remote: subid,
                    },
                    dependers: *dependers,
                }
            }
            SubState::RemoteOnly {
                remote: _,
                dependers: _,
            } => {}
            SubState::Unified {
                unified: _,
                dependers: _,
            } => {}
        }
        tracing::debug!(
            "TimelineSub::force_add_remote: {:?} => {:?}",
            before,
            self.state
        );
    }

    pub fn try_add_remote(&mut self, pool: &mut RelayPool, filter: &HybridFilter) {
        let before = self.state.clone();
        match &mut self.state {
            SubState::NoSub { dependers } => {
                let subid = subscriptions::new_sub_id();
                pool.subscribe(subid.clone(), filter.remote().to_vec());
                self.filter = Some(filter.to_owned());
                self.state = SubState::RemoteOnly {
                    remote: subid,
                    dependers: *dependers,
                };
            }
            SubState::LocalOnly { local, dependers } => {
                let subid = subscriptions::new_sub_id();
                pool.subscribe(subid.clone(), filter.remote().to_vec());
                self.filter = Some(filter.to_owned());
                self.state = SubState::Unified {
                    unified: UnifiedSubscription {
                        local: *local,
                        remote: subid,
                    },
                    dependers: *dependers,
                }
            }
            SubState::RemoteOnly {
                remote: _,
                dependers: _,
            } => {}
            SubState::Unified {
                unified: _,
                dependers: _,
            } => {}
        }
        tracing::debug!(
            "TimelineSub::try_add_remote: {:?} => {:?}",
            before,
            self.state
        );
    }

    pub fn increment(&mut self) {
        let before = self.state.clone();
        match &mut self.state {
            SubState::NoSub { dependers } => {
                *dependers += 1;
            }
            SubState::LocalOnly {
                local: _,
                dependers,
            } => {
                *dependers += 1;
            }
            SubState::RemoteOnly {
                remote: _,
                dependers,
            } => {
                *dependers += 1;
            }
            SubState::Unified {
                unified: _,
                dependers,
            } => {
                *dependers += 1;
            }
        }

        tracing::debug!("TimelineSub::increment: {:?} => {:?}", before, self.state);
    }

    pub fn get_local(&self) -> Option<Subscription> {
        match &self.state {
            SubState::NoSub { dependers: _ } => None,
            SubState::LocalOnly {
                local,
                dependers: _,
            } => Some(*local),
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

    pub fn unsubscribe_or_decrement(&mut self, ndb: &mut Ndb, pool: &mut RelayPool) {
        let before = self.state.clone();
        's: {
            match &mut self.state {
                SubState::NoSub { dependers } => *dependers = dependers.saturating_sub(1),
                SubState::LocalOnly { local, dependers } => {
                    if *dependers > 1 {
                        *dependers = dependers.saturating_sub(1);
                        break 's;
                    }

                    if let Err(e) = ndb.unsubscribe(*local) {
                        tracing::error!("Could not unsub ndb: {e}");
                        break 's;
                    }

                    self.state = SubState::NoSub { dependers: 0 };
                }
                SubState::RemoteOnly { remote, dependers } => {
                    if *dependers > 1 {
                        *dependers = dependers.saturating_sub(1);
                        break 's;
                    }

                    pool.unsubscribe(remote.to_owned());

                    self.state = SubState::NoSub { dependers: 0 };
                }
                SubState::Unified { unified, dependers } => {
                    if *dependers > 1 {
                        *dependers = dependers.saturating_sub(1);
                        break 's;
                    }

                    pool.unsubscribe(unified.remote.to_owned());

                    if let Err(e) = ndb.unsubscribe(unified.local) {
                        tracing::error!("could not unsub ndb: {e}");
                        self.state = SubState::LocalOnly {
                            local: unified.local,
                            dependers: *dependers,
                        }
                    } else {
                        self.state = SubState::NoSub { dependers: 0 };
                    }
                }
            }
        }
        tracing::debug!(
            "TimelineSub::unsubscribe_or_decrement: {:?} => {:?}",
            before,
            self.state
        );
    }

    pub fn get_filter(&self) -> Option<&HybridFilter> {
        self.filter.as_ref()
    }

    pub fn no_sub(&self) -> bool {
        matches!(self.state, SubState::NoSub { dependers: _ })
    }

    pub fn dependers(&self) -> usize {
        match &self.state {
            SubState::NoSub { dependers } => *dependers,
            SubState::LocalOnly {
                local: _,
                dependers,
            } => *dependers,
            SubState::RemoteOnly {
                remote: _,
                dependers,
            } => *dependers,
            SubState::Unified {
                unified: _,
                dependers,
            } => *dependers,
        }
    }
}
