use enostr::{Filter, NoteId, RelayPool};
use hashbrown::HashMap;
use nostrdb::{Ndb, Subscription};
use tracing::{error, info};
use uuid::Uuid;

use crate::timeline::ThreadSelection;

#[derive(Debug)]
pub struct MultiSubscriber {
    pub filters: Vec<Filter>,
    pub local_subid: Option<Subscription>,
    pub remote_subid: Option<String>,
    local_subscribers: u32,
    remote_subscribers: u32,
}

impl MultiSubscriber {
    /// Create a MultiSubscriber with an initial local subscription.
    pub fn with_initial_local_sub(sub: Subscription, filters: Vec<Filter>) -> Self {
        let mut msub = MultiSubscriber::new(filters);
        msub.local_subid = Some(sub);
        msub.local_subscribers = 1;
        msub
    }

    pub fn new(filters: Vec<Filter>) -> Self {
        Self {
            filters,
            local_subid: None,
            remote_subid: None,
            local_subscribers: 0,
            remote_subscribers: 0,
        }
    }

    fn unsubscribe_remote(&mut self, ndb: &Ndb, pool: &mut RelayPool) {
        let remote_subid = if let Some(remote_subid) = &self.remote_subid {
            remote_subid
        } else {
            self.err_log(ndb, "unsubscribe_remote: nothing to unsubscribe from?");
            return;
        };

        pool.unsubscribe(remote_subid.clone());

        self.remote_subid = None;
    }

    /// Locally unsubscribe if we have one
    fn unsubscribe_local(&mut self, ndb: &mut Ndb) {
        let local_sub = if let Some(local_sub) = self.local_subid {
            local_sub
        } else {
            self.err_log(ndb, "unsubscribe_local: nothing to unsubscribe from?");
            return;
        };

        match ndb.unsubscribe(local_sub) {
            Err(e) => {
                self.err_log(ndb, &format!("Failed to unsubscribe: {e}"));
            }
            Ok(_) => {
                self.local_subid = None;
            }
        }
    }

    pub fn unsubscribe(&mut self, ndb: &mut Ndb, pool: &mut RelayPool) -> bool {
        if self.local_subscribers == 0 && self.remote_subscribers == 0 {
            self.err_log(
                ndb,
                "Called multi_subscriber unsubscribe when both sub counts are 0",
            );
            return false;
        }

        self.local_subscribers = self.local_subscribers.saturating_sub(1);
        self.remote_subscribers = self.remote_subscribers.saturating_sub(1);

        if self.local_subscribers == 0 && self.remote_subscribers == 0 {
            self.info_log(ndb, "Locally unsubscribing");
            self.unsubscribe_local(ndb);
            self.unsubscribe_remote(ndb, pool);
            self.local_subscribers = 0;
            self.remote_subscribers = 0;
            true
        } else {
            false
        }
    }

    fn info_log(&self, ndb: &Ndb, msg: &str) {
        info!(
            "{msg}. {}/{}/{} active ndb/local/remote subscriptions.",
            ndb.subscription_count(),
            self.local_subscribers,
            self.remote_subscribers,
        );
    }

    fn err_log(&self, ndb: &Ndb, msg: &str) {
        error!(
            "{msg}. {}/{}/{} active ndb/local/remote subscriptions.",
            ndb.subscription_count(),
            self.local_subscribers,
            self.remote_subscribers,
        );
    }

    pub fn subscribe(&mut self, ndb: &Ndb, pool: &mut RelayPool) {
        self.local_subscribers += 1;
        self.remote_subscribers += 1;

        if self.remote_subscribers == 1 {
            if self.remote_subid.is_some() {
                self.err_log(
                    ndb,
                    "Object is first subscriber, but it already had a subscription",
                );
                return;
            } else {
                let subid = Uuid::new_v4().to_string();
                pool.subscribe(subid.clone(), self.filters.clone());
                self.info_log(ndb, "First remote subscription");
                self.remote_subid = Some(subid);
            }
        }

        if self.local_subscribers == 1 {
            if self.local_subid.is_some() {
                self.err_log(ndb, "Should not have a local subscription already");
                return;
            }

            match ndb.subscribe(&self.filters) {
                Ok(sub) => {
                    self.info_log(ndb, "First local subscription");
                    self.local_subid = Some(sub);
                }

                Err(err) => {
                    error!("multi_subscriber: error subscribing locally: '{err}'")
                }
            }
        }
    }
}

type RootNoteId = NoteId;

#[derive(Default)]
pub struct ThreadSubs {
    pub remotes: HashMap<RootNoteId, Remote>,

    // each 'scope' represents a thread with the same root id. Navigating to a different root id means we need
    // a new scope so we can retain the old subscription. Navigating to a note within the same root id replaces the
    // local subscription in that scope
    scopes: HashMap<MetaId, Vec<ScopedSub>>,
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

pub struct ScopedSub {
    pub selection: ThreadSelection,
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
            replace_local_sub(ndb, id, local_sub_filter, cur_scope)
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
        tracing::info!(
            "Sub stats: num remotes: {}, num locals: {}",
            self.remotes.len(),
            self.scopes.len()
        );
    }

    pub fn unsubscribe(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        meta_id: usize,
        id: &ThreadSelection,
    ) {
        let Some(scopes) = self.scopes.get_mut(&meta_id) else {
            return;
        };

        let Some(scope) = scopes.pop() else {
            tracing::error!("called unsubscribe but there aren't any scopes left");
            return;
        };
        ndb_unsub(ndb, scope.sub, id);

        if scopes.is_empty() {
            self.scopes.remove(&meta_id);
        }

        let Some(remote) = self.remotes.get_mut(&id.root_id.bytes()) else {
            tracing::error!("somehow we're unsubscribing but we don't have a remote");
            return;
        };

        remote.dependers = remote.dependers.saturating_sub(1);

        if remote.dependers == 0 {
            let remote = self
                .remotes
                .remove(&id.root_id.bytes())
                .expect("code above should guarentee existence");
            tracing::info!("Remotely unsubscribed: {}", remote.subid);
            pool.unsubscribe(remote.subid);
        }

        tracing::info!(
            "Unsub status num remotes: {}, num locals: {}",
            self.remotes.len(),
            self.scopes.len()
        );
    }

    pub fn get_local(&self, meta_id: usize) -> Option<&ScopedSub> {
        self.scopes.get(&meta_id).as_ref().and_then(|s| s.last())
    }
}

fn replace_local_sub(
    ndb: &mut Ndb,
    selection: &ThreadSelection,
    local_sub_filter: Vec<Filter>,
    old_sub: &mut ScopedSub,
) -> isize {
    if old_sub.selection == *selection {
        return 0;
    }

    let mut new_subs = 0;

    if ndb_unsub(ndb, old_sub.sub, selection) {
        new_subs -= 1;
    }

    if let Some(sub) = ndb_sub(ndb, &local_sub_filter, selection) {
        *old_sub = ScopedSub {
            selection: selection.clone(),
            sub,
            filter: local_sub_filter,
        };
        new_subs += 1;
    }

    new_subs
}

fn ndb_sub(ndb: &Ndb, filter: &[Filter], id: impl std::fmt::Debug) -> Option<Subscription> {
    match ndb.subscribe(filter) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::info!("Failed to get subscription for {:?}: {e}", id);
            None
        }
    }
}

fn ndb_unsub(ndb: &mut Ndb, sub: Subscription, id: impl std::fmt::Debug) -> bool {
    match ndb.unsubscribe(sub) {
        Ok(_) => true,
        Err(e) => {
            tracing::info!("Failed to unsub {:?}: {e}", id);
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

    tracing::info!("Remote subscribe for {:?}", id);

    pool.subscribe(subid, filter);

    remote
}

fn local_sub_new_scope(
    ndb: &mut Ndb,
    id: &ThreadSelection,
    local_sub_filter: Vec<Filter>,
    scopes: &mut Vec<ScopedSub>,
) -> isize {
    let Some(sub) = ndb_sub(ndb, &local_sub_filter, id) else {
        return 0;
    };

    scopes.push(ScopedSub {
        selection: id.clone(),
        sub,
        filter: local_sub_filter,
    });

    1
}
