use egui_nav::ReturnType;
use enostr::{Filter, NoteId, RelayPool};
use hashbrown::HashMap;
use nostrdb::{Ndb, Subscription};
use notedeck::{UnifiedSubscription, filter::HybridFilter};
use uuid::Uuid;

use crate::{subscriptions, timeline::ThreadSelection};

type RootNoteId = NoteId;

#[derive(Default)]
pub struct ThreadSubs {
    pub remotes: HashMap<RootNoteId, Remote>,
    scopes: HashMap<MetaId, Vec<Scope>>,
}

// column id
type MetaId = usize;

pub struct Remote {
    pub filter: Vec<Filter>,
    subid: String,
    dependers: usize,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("subid", &self.subid)
            .field("dependers", &self.dependers)
            .finish()
    }
}

struct Scope {
    pub root_id: NoteId,
    stack: Vec<Sub>,
}

pub struct Sub {
    pub selected_id: NoteId,
    pub sub: Subscription,
    pub filter: Vec<Filter>,
}

impl ThreadSubs {
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        meta_id: usize,
        id: &ThreadSelection,
        local_sub_filter: Vec<Filter>,
        new_scope: bool,
        remote_sub_filter: impl FnOnce() -> Vec<Filter>,
    ) {
        let cur_scopes = self.scopes.entry(meta_id).or_default();

        let new_subs = if new_scope || cur_scopes.is_empty() {
            local_sub_new_scope(ndb, id, local_sub_filter, cur_scopes)
        } else {
            let cur_scope = cur_scopes.last_mut().expect("can't be empty");
            sub_current_scope(ndb, id, local_sub_filter, cur_scope)
        };

        let remote = match self.remotes.raw_entry_mut().from_key(&id.root_id.bytes()) {
            hashbrown::hash_map::RawEntryMut::Occupied(entry) => entry.into_mut(),
            hashbrown::hash_map::RawEntryMut::Vacant(entry) => {
                let (_, res) = entry.insert(
                    NoteId::new(*id.root_id.bytes()),
                    sub_remote(pool, remote_sub_filter, id),
                );

                res
            }
        };

        remote.dependers = remote.dependers.saturating_add_signed(new_subs);
        let num_dependers = remote.dependers;
        tracing::debug!(
            "Sub stats: num remotes: {}, num locals: {}, num remote dependers: {:?}",
            self.remotes.len(),
            self.scopes.len(),
            num_dependers,
        );
    }

    pub fn unsubscribe(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        meta_id: usize,
        id: &ThreadSelection,
        return_type: ReturnType,
    ) {
        let Some(scopes) = self.scopes.get_mut(&meta_id) else {
            return;
        };

        let Some(remote) = self.remotes.get_mut(&id.root_id.bytes()) else {
            tracing::error!("somehow we're unsubscribing but we don't have a remote");
            return;
        };

        match return_type {
            ReturnType::Drag => {
                if let Some(scope) = scopes.last_mut() {
                    let Some(cur_sub) = scope.stack.pop() else {
                        tracing::error!("expected a scope to be left");
                        return;
                    };

                    if scope.root_id.bytes() != id.root_id.bytes() {
                        tracing::error!(
                            "Somehow the current scope's root is not equal to the selected note's root. scope's root: {:?}, thread's root: {:?}",
                            scope.root_id.hex(),
                            id.root_id.bytes()
                        );
                    }

                    if ndb_unsub(ndb, cur_sub.sub, id) {
                        remote.dependers = remote.dependers.saturating_sub(1);
                    }

                    if scope.stack.is_empty() {
                        scopes.pop();
                    }
                }
            }
            ReturnType::Click => {
                let Some(scope) = scopes.pop() else {
                    tracing::error!("called unsubscribe but there aren't any scopes left");
                    return;
                };

                if scope.root_id.bytes() != id.root_id.bytes() {
                    tracing::error!(
                        "Somehow the current scope's root is not equal to the selected note's root. scope's root: {:?}, thread's root: {:?}",
                        scope.root_id.hex(),
                        id.root_id.bytes()
                    );
                }
                for sub in scope.stack {
                    if ndb_unsub(ndb, sub.sub, id) {
                        remote.dependers = remote.dependers.saturating_sub(1);
                    }
                }
            }
        }

        if scopes.is_empty() {
            self.scopes.remove(&meta_id);
        }

        let num_dependers = remote.dependers;

        if remote.dependers == 0 {
            let remote = self
                .remotes
                .remove(&id.root_id.bytes())
                .expect("code above should guarentee existence");
            tracing::debug!("Remotely unsubscribed: {}", remote.subid);
            pool.unsubscribe(remote.subid);
        }

        tracing::debug!(
            "unsub stats: num remotes: {}, num locals: {}, num remote dependers: {:?}",
            self.remotes.len(),
            self.scopes.len(),
            num_dependers,
        );
    }

    pub fn get_local(&self, meta_id: usize) -> Option<&Sub> {
        self.scopes
            .get(&meta_id)
            .as_ref()
            .and_then(|s| s.last())
            .and_then(|s| s.stack.last())
    }
}

fn sub_current_scope(
    ndb: &mut Ndb,
    selection: &ThreadSelection,
    local_sub_filter: Vec<Filter>,
    cur_scope: &mut Scope,
) -> isize {
    let mut new_subs = 0;

    if selection.root_id.bytes() != cur_scope.root_id.bytes() {
        tracing::error!(
            "Somehow the current scope's root is not equal to the selected note's root"
        );
    }

    if let Some(sub) = ndb_sub(ndb, &local_sub_filter, selection) {
        cur_scope.stack.push(Sub {
            selected_id: NoteId::new(*selection.selected_or_root()),
            sub,
            filter: local_sub_filter,
        });
        new_subs += 1;
    }

    new_subs
}

fn ndb_sub(ndb: &Ndb, filter: &[Filter], id: impl std::fmt::Debug) -> Option<Subscription> {
    match ndb.subscribe(filter) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!("Failed to get subscription for {:?}: {e}", id);
            None
        }
    }
}

fn ndb_unsub(ndb: &mut Ndb, sub: Subscription, id: impl std::fmt::Debug) -> bool {
    match ndb.unsubscribe(sub) {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("Failed to unsub {:?}: {e}", id);
            false
        }
    }
}

fn sub_remote(
    pool: &mut RelayPool,
    remote_sub_filter: impl FnOnce() -> Vec<Filter>,
    id: impl std::fmt::Debug,
) -> Remote {
    let subid = Uuid::new_v4().to_string();

    let filter = remote_sub_filter();

    let remote = Remote {
        filter: filter.clone(),
        subid: subid.clone(),
        dependers: 0,
    };

    tracing::debug!("Remote subscribe for {:?}", id);

    pool.subscribe(subid, filter);

    remote
}

fn local_sub_new_scope(
    ndb: &mut Ndb,
    id: &ThreadSelection,
    local_sub_filter: Vec<Filter>,
    scopes: &mut Vec<Scope>,
) -> isize {
    let Some(sub) = ndb_sub(ndb, &local_sub_filter, id) else {
        return 0;
    };

    scopes.push(Scope {
        root_id: id.root_id.to_note_id(),
        stack: vec![Sub {
            selected_id: NoteId::new(*id.selected_or_root()),
            sub,
            filter: local_sub_filter,
        }],
    });

    1
}

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
    pub fn try_add_local(&mut self, ndb: &Ndb, filter: &HybridFilter) {
        let before = self.state.clone();
        match &mut self.state {
            SubState::NoSub { dependers } => {
                let Some(sub) = ndb_sub(ndb, filter.local(), "") else {
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
                let Some(local) = ndb_sub(ndb, filter.local(), "") else {
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
                SubState::NoSub { dependers } => {
                    *dependers -= 1;
                }
                SubState::LocalOnly { local, dependers } => {
                    if *dependers > 1 {
                        *dependers -= 1;
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
                        *dependers -= 1;
                        break 's;
                    }

                    pool.unsubscribe(remote.to_owned());

                    self.state = SubState::NoSub { dependers: 0 };
                }
                SubState::Unified { unified, dependers } => {
                    if *dependers > 1 {
                        *dependers -= 1;
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
}
