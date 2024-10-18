use crate::timeline::{TimelineId, TimelineKind};
use std::collections::HashMap;
use nostrdb::{Subscription, Ndb};
use enostr::RelayPool;

#[derive(Debug, Clone)]
pub enum SubKind {
    /// Initial subscription. This is the first time we do a remote subscription
    /// for a timeline
    Initial,

    /// One shot requests, we can just close after we receive EOSE
    OneShot,

    Timeline(TimelineKind),

    /// We are fetching a contact list so that we can use it for our follows
    /// Filter.
    // TODO: generalize this to any list?
    FetchingContactList(TimelineId),
}

pub struct ActiveSub {
    /// Remote subscription id
    id: String,

    /// Number of subscribers. There can be multiple views using the same
    /// subscription. This tracks that. When we reach 0, then we unsubscribe.
    subscribers: i32,

    /// Do we have a corresponding local subscription?
    local: Option<Subscription>,

    /// What kind of subscription is this?
    kind: SubKind
}

/// Subscriptions that need to be tracked at various stages. Sometimes we
/// need to do A, then B, then C. Tracking requests at various stages by
/// mapping uuid subids to explicit states happens here.
#[derive(Default)]
pub struct Subscriptions {
    pub active: Vec<RemoteSub>
}

impl Subscriptions {
    /// Find a remote subscription given a local subscription id. This
    /// is for subscriptions that have a one-to-one subscription mapping, which
    /// may not always be the case
    pub fn find_remote_sub(&self, sub_id: Subscription) -> Option<&RemoteSub> {
        for sub in &self.active {
            if let Some(local) = sub.local {
                if local == sub_id {
                    return Some(sub);
                }
            }
        }

        None
    }

    pub fn subscribe() {
    }
}

/// References to the remote and local parts of a subscription
pub struct SubRefs<'a> {
    pub remote: Option<&'a str>,
    pub local: Option<Subscription>,
}

/// Owned version of SubRefs
pub struct SubRefsBuf {
    pub remote: Option<String>,
    pub local: Option<Subscription>,
}

impl SubRefsBuf {
    pub fn new(local: Option<Subscription>, remote: Option<&str>) -> Self {
        let remote = remote.map(|x| x.to_owned());
        Self { remote, local }
    }

    pub fn borrow<'a>(&'a self) -> SubRefs<'a> {
        SubRefs::new(self.local, self.remote.as_deref())
    }
}

impl<'a> SubRefs<'a> {
    pub fn new(local: Option<Subscription>, remote: Option<&'a str>) -> Self {
        Self {
            remote, local
        }
    }

    pub fn to_owned(&self) -> SubRefsBuf {
        SubRefsBuf::new(self.local, self.remote)
    }

    pub fn unsubscribe(&self, ndb: &Ndb, pool: &mut RelayPool) {
        if let Some(r) = self.remote {
            pool.unsubscribe(r.to_string());
        }

        if let Some(local) = self.local {
            ndb.unsubscribe(local);
        }
    }
}

