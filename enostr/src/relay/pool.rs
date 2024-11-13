use crate::relay::{Relay, RelayStatus};
use crate::{ClientMessage, Result};
use nostrdb::Filter;

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use ewebsock::{WsEvent, WsMessage};

#[cfg(not(target_arch = "wasm32"))]
use tracing::{debug, error, info};

#[derive(Debug)]
pub struct PoolEvent<'a> {
    pub relay: &'a str,
    pub event: ewebsock::WsEvent,
}

pub struct PoolRelay {
    pub relay: Relay,
    pub last_ping: Instant,
    pub last_connect_attempt: Instant,
    pub retry_connect_after: Duration,
}

impl PoolRelay {
    pub fn new(relay: Relay) -> PoolRelay {
        PoolRelay {
            relay,
            last_ping: Instant::now(),
            last_connect_attempt: Instant::now(),
            retry_connect_after: Self::initial_reconnect_duration(),
        }
    }

    pub fn initial_reconnect_duration() -> Duration {
        Duration::from_secs(5)
    }
}

pub struct RelayPool {
    pub relays: Vec<PoolRelay>,
    pub subs: HashMap<String, Vec<Filter>>,
    pub ping_rate: Duration,
    /// Used when there are no others
    pub bootstrapping_relays: BTreeSet<String>,
    /// Locally specified relays
    pub local_relays: BTreeSet<String>,
    /// NIP-65 specified relays
    pub advertised_relays: BTreeSet<String>,
    /// If non-empty force the relay pool to use exactly this set
    pub forced_relays: BTreeSet<String>,
}

impl Default for RelayPool {
    fn default() -> Self {
        RelayPool::new()
    }
}

impl RelayPool {
    // Constructs a new, empty RelayPool.
    pub fn new() -> RelayPool {
        RelayPool {
            relays: vec![],
            subs: HashMap::new(),
            ping_rate: Duration::from_secs(25),
            bootstrapping_relays: BTreeSet::new(),
            local_relays: BTreeSet::new(),
            advertised_relays: BTreeSet::new(),
            forced_relays: BTreeSet::new(),
        }
    }

    pub fn ping_rate(&mut self, duration: Duration) -> &mut Self {
        self.ping_rate = duration;
        self
    }

    pub fn has(&self, url: &str) -> bool {
        for relay in &self.relays {
            if relay.relay.url == url {
                return true;
            }
        }

        false
    }

    pub fn send(&mut self, cmd: &ClientMessage) {
        for relay in &mut self.relays {
            relay.relay.send(cmd);
        }
    }

    pub fn unsubscribe(&mut self, subid: String) {
        for relay in &mut self.relays {
            relay.relay.send(&ClientMessage::close(subid.clone()));
        }
        self.subs.remove(&subid);
    }

    pub fn subscribe(&mut self, subid: String, filter: Vec<Filter>) {
        self.subs.insert(subid.clone(), filter.clone());
        for relay in &mut self.relays {
            relay.relay.subscribe(subid.clone(), filter.clone());
        }
    }

    /// Keep relay connectiongs alive by pinging relays that haven't been
    /// pinged in awhile. Adjust ping rate with [`ping_rate`].
    pub fn keepalive_ping(&mut self, wakeup: impl Fn() + Send + Sync + Clone + 'static) {
        for relay in &mut self.relays {
            let now = std::time::Instant::now();

            match relay.relay.status {
                RelayStatus::Disconnected => {
                    let reconnect_at = relay.last_connect_attempt + relay.retry_connect_after;
                    if now > reconnect_at {
                        relay.last_connect_attempt = now;
                        let next_duration = Duration::from_millis(
                            ((relay.retry_connect_after.as_millis() as f64) * 1.5) as u64,
                        );
                        debug!(
                            "bumping reconnect duration from {:?} to {:?} and retrying connect",
                            relay.retry_connect_after, next_duration
                        );
                        relay.retry_connect_after = next_duration;
                        if let Err(err) = relay.relay.connect(wakeup.clone()) {
                            error!("error connecting to relay: {}", err);
                        }
                    } else {
                        // let's wait a bit before we try again
                    }
                }

                RelayStatus::Connected => {
                    relay.retry_connect_after = PoolRelay::initial_reconnect_duration();

                    let should_ping = now - relay.last_ping > self.ping_rate;
                    if should_ping {
                        debug!("pinging {}", relay.relay.url);
                        relay.relay.ping();
                        relay.last_ping = Instant::now();
                    }
                }

                RelayStatus::Connecting => {
                    // cool story bro
                }
            }
        }
    }

    pub fn send_to(&mut self, cmd: &ClientMessage, relay_url: &str) {
        for relay in &mut self.relays {
            let relay = &mut relay.relay;
            if relay.url == relay_url {
                relay.send(cmd);
                return;
            }
        }
    }

    pub fn configure_relays(
        &mut self,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        let urls = if !self.forced_relays.is_empty() {
            debug!("using forced relays");
            self.forced_relays.iter().cloned().collect::<Vec<_>>()
        } else {
            let mut combined_relays = self
                .local_relays
                .union(&self.advertised_relays)
                .cloned()
                .collect::<BTreeSet<_>>();

            // If the combined set is empty, use `bootstrapping_relays`.
            if combined_relays.is_empty() {
                debug!("using bootstrapping relays");
                combined_relays = self.bootstrapping_relays.clone();
            } else {
                debug!("using local+advertised relays");
            }

            // Collect the resulting set into a vector.
            combined_relays.into_iter().collect::<Vec<_>>()
        };

        self.set_relays(&urls, wakeup)
    }

    // Adds a websocket url to the RelayPool.
    fn add_url(
        &mut self,
        url: String,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        let url = Self::canonicalize_url(&url);
        // Check if the URL already exists in the pool.
        if self.has(&url) {
            return Ok(());
        }
        let relay = Relay::new(url, wakeup)?;
        let mut pool_relay = PoolRelay::new(relay);

        // Add all of the existing subscriptions to the new relay
        for (subid, filters) in &self.subs {
            pool_relay.relay.subscribe(subid.clone(), filters.clone());
        }

        self.relays.push(pool_relay);

        Ok(())
    }

    // Add and remove relays to match the provided list
    pub fn set_relays(
        &mut self,
        urls: &Vec<String>,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        // Canonicalize the new URLs.
        let new_urls = urls
            .iter()
            .map(|u| Self::canonicalize_url(u))
            .collect::<HashSet<_>>();

        // Get the old URLs from the existing relays.
        let old_urls = self
            .relays
            .iter()
            .map(|pr| pr.relay.url.clone())
            .collect::<HashSet<_>>();

        debug!("old relays: {:?}", old_urls);
        debug!("new relays: {:?}", new_urls);

        if new_urls.len() == 0 {
            info!("bootstrapping, not clearing the relay list ...");
            return Ok(());
        }

        // Remove the relays that are in old_urls but not in new_urls.
        let to_remove: HashSet<_> = old_urls.difference(&new_urls).cloned().collect();
        for url in &to_remove {
            debug!("removing relay {}", url);
        }
        self.relays.retain(|pr| !to_remove.contains(&pr.relay.url));

        // FIXME - how do we close connections the removed relays?

        // Add the relays that are in new_urls but not in old_urls.
        let to_add: HashSet<_> = new_urls.difference(&old_urls).cloned().collect();
        for url in to_add {
            debug!("adding relay {}", url);
            if let Err(e) = self.add_url(url.clone(), wakeup.clone()) {
                error!("Failed to add relay with URL {}: {:?}", url, e);
            }
        }

        Ok(())
    }

    // standardize the format (ie, trailing slashes) to avoid dups
    pub fn canonicalize_url(url: &String) -> String {
        match Url::parse(&url) {
            Ok(parsed_url) => parsed_url.to_string(),
            Err(_) => url.clone(), // If parsing fails, return the original URL.
        }
    }

    /// Attempts to receive a pool event from a list of relays. The
    /// function searches each relay in the list in order, attempting to
    /// receive a message from each. If a message is received, return it.
    /// If no message is received from any relays, None is returned.
    pub fn try_recv(&mut self) -> Option<PoolEvent<'_>> {
        for relay in &mut self.relays {
            let relay = &mut relay.relay;
            if let Some(event) = relay.receiver.try_recv() {
                match &event {
                    WsEvent::Opened => {
                        relay.status = RelayStatus::Connected;
                    }
                    WsEvent::Closed => {
                        relay.status = RelayStatus::Disconnected;
                    }
                    WsEvent::Error(err) => {
                        error!("{:?}", err);
                        relay.status = RelayStatus::Disconnected;
                    }
                    WsEvent::Message(ev) => {
                        // let's just handle pongs here.
                        // We only need to do this natively.
                        #[cfg(not(target_arch = "wasm32"))]
                        if let WsMessage::Ping(ref bs) = ev {
                            debug!("pong {}", &relay.url);
                            relay.sender.send(WsMessage::Pong(bs.to_owned()));
                        }
                    }
                }
                return Some(PoolEvent {
                    event,
                    relay: &relay.url,
                });
            }
        }

        None
    }
}
