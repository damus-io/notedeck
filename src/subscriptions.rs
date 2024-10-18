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

/// Subscriptions that need to be tracked at various stages. Sometimes we
/// need to do A, then B, then C. Tracking requests at various stages by
/// mapping uuid subids to explicit states happens here.
#[derive(Default)]
pub struct RemoteSubscriptions {
    pub subs: HashMap<String, SubKind>,
}

pub struct Subscriptions<'a> {
    pub remote: Option<&'a str>,
    pub local: Option<Subscription>,
}

pub struct SubscriptionsBuf {
    pub remote: Option<String>,
    pub local: Option<Subscription>,
}

impl SubscriptionsBuf {
    pub fn new(local: Option<Subscription>, remote: Option<&str>) -> Self {
        let remote = remote.map(|x| x.to_owned());
        Self { remote, local }
    }

    pub fn borrow<'a>(&'a self) -> Subscriptions<'a> {
        Subscriptions::new(self.local, self.remote.as_deref())
    }
}

impl<'a> Subscriptions<'a> {
    pub fn new(local: Option<Subscription>, remote: Option<&'a str>) -> Self {
        Subscriptions {
            remote, local
        }
    }

    pub fn to_owned(&self) -> SubscriptionsBuf {
        SubscriptionsBuf::new(self.remote, self.local)
    }

    pub fn unsubscribe(ndb: &Ndb, pool: &mut RelayPool) -> {
        if let Some(r) = self.remote {
            pool.unsubscribe(r)
        }

        if let Some(local) = self.local {
            ndb.unsubscribe(local)
        }
    }
}

