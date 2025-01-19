use enostr::{Filter, RelayPool};
use nostrdb::Ndb;
use tracing::{error, info};
use uuid::Uuid;

use notedeck::UnifiedSubscription;

pub struct MultiSubscriber {
    filters: Vec<Filter>,
    pub sub: Option<UnifiedSubscription>,
    subscribers: u32,
}

impl MultiSubscriber {
    pub fn new(filters: Vec<Filter>) -> Self {
        Self {
            filters,
            sub: None,
            subscribers: 0,
        }
    }

    fn real_subscribe(
        ndb: &Ndb,
        pool: &mut RelayPool,
        filters: Vec<Filter>,
    ) -> Option<UnifiedSubscription> {
        let subid = Uuid::new_v4().to_string();
        let sub = ndb.subscribe(&filters).ok()?;

        pool.subscribe(subid.clone(), filters);

        Some(UnifiedSubscription {
            local: sub,
            remote: subid,
        })
    }

    pub fn unsubscribe(&mut self, ndb: &mut Ndb, pool: &mut RelayPool) {
        if self.subscribers == 0 {
            error!("No subscribers to unsubscribe from");
            return;
        }

        self.subscribers -= 1;
        if self.subscribers == 0 {
            let sub = match self.sub {
                Some(ref sub) => sub,
                None => {
                    error!("No remote subscription to unsubscribe from");
                    return;
                }
            };
            let local_sub = &sub.local;
            if let Err(e) = ndb.unsubscribe(*local_sub) {
                error!(
                    "failed to unsubscribe from object: {e}, subid:{}, {} active subscriptions",
                    local_sub.id(),
                    ndb.subscription_count()
                );
            } else {
                info!(
                    "Unsubscribed from object subid:{}. {} active subscriptions",
                    local_sub.id(),
                    ndb.subscription_count()
                );
            }

            // unsub from remote
            pool.unsubscribe(sub.remote.clone());
            self.sub = None;
        } else {
            info!(
                "Locally unsubscribing. {} active ndb subscriptions. {} active subscriptions for this object",
                ndb.subscription_count(),
                self.subscribers,
            );
        }
    }

    pub fn subscribe(&mut self, ndb: &Ndb, pool: &mut RelayPool) {
        self.subscribers += 1;
        if self.subscribers == 1 {
            if self.sub.is_some() {
                error!("Object is first subscriber, but it already had remote subscription");
                return;
            }

            self.sub = Self::real_subscribe(ndb, pool, self.filters.clone());
            info!(
                "Remotely subscribing to object. {} total active subscriptions, {} on this object",
                ndb.subscription_count(),
                self.subscribers,
            );

            if self.sub.is_none() {
                error!("Error subscribing remotely to object");
            }
        } else {
            info!(
                "Locally subscribing. {} total active subscriptions, {} for this object",
                ndb.subscription_count(),
                self.subscribers,
            )
        }
    }
}
