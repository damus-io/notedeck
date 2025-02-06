use enostr::{Filter, RelayPool};
use nostrdb::{Ndb, Subscription};
use tracing::{error, info};
use uuid::Uuid;

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
