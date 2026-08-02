use nostrdb::{Filter, Ndb, Subscription};

mod thread_sub;
mod timeline_remote;
mod timeline_sub;

pub use thread_sub::ThreadSubs;
pub use timeline_remote::RemoteSubscriptionPolicy;
pub(crate) use timeline_remote::{
    drop_timeline_remote_owner, ensure_remote_timeline_subscription,
    update_remote_timeline_subscription, update_remote_timeline_subscription_for_account,
};
#[cfg(test)]
pub(super) use timeline_remote::{
    timeline_remote_sub_declaration, timeline_remote_sub_key, TimelineScopedSub,
};
pub use timeline_sub::TimelineSub;

pub fn ndb_sub(ndb: &Ndb, filter: &[Filter], id: impl std::fmt::Debug) -> Option<Subscription> {
    match ndb.subscribe(filter) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!("Failed to get subscription for {:?}: {e}", id);
            None
        }
    }
}

pub fn ndb_unsub(ndb: &mut Ndb, sub: Subscription, id: impl std::fmt::Debug) -> bool {
    match ndb.unsubscribe(sub) {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("Failed to unsub {:?}: {e}", id);
            false
        }
    }
}
