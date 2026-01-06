use crate::relay::{setup_multicast_relay, MulticastRelay, Relay, RelayStatus};
use crate::{ClientMessage, Error, Result};
use nostrdb::Filter;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use url::Url;

use ewebsock::{WsEvent, WsMessage};
use tracing::{debug, error, trace};

use super::subs_debug::SubsDebug;

#[derive(Debug)]
pub struct PoolEvent<'a> {
    pub relay: &'a str,
    pub event: ewebsock::WsEvent,
}

impl PoolEvent<'_> {
    pub fn into_owned(self) -> PoolEventBuf {
        PoolEventBuf {
            relay: self.relay.to_owned(),
            event: self.event,
        }
    }
}

pub struct PoolEventBuf {
    pub relay: String,
    pub event: ewebsock::WsEvent,
}

pub enum PoolRelay {
    Websocket(WebsocketRelay),
    Multicast(MulticastRelay),
}

pub struct WebsocketRelay {
    pub relay: Relay,
    pub last_ping: Instant,
    pub last_connect_attempt: Instant,
    pub retry_connect_after: Duration,
}

impl PoolRelay {
    pub fn url(&self) -> &str {
        match self {
            Self::Websocket(wsr) => wsr.relay.url.as_str(),
            Self::Multicast(_wsr) => "multicast",
        }
    }

    pub fn set_status(&mut self, status: RelayStatus) {
        match self {
            Self::Websocket(wsr) => {
                wsr.relay.status = status;
            }
            Self::Multicast(_mcr) => {}
        }
    }

    pub fn try_recv(&self) -> Option<WsEvent> {
        match self {
            Self::Websocket(recvr) => recvr.relay.receiver.try_recv(),
            Self::Multicast(recvr) => recvr.try_recv(),
        }
    }

    pub fn status(&self) -> RelayStatus {
        match self {
            Self::Websocket(wsr) => wsr.relay.status,
            Self::Multicast(mcr) => mcr.status,
        }
    }

    pub fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        match self {
            Self::Websocket(wsr) => {
                wsr.relay.send(msg);
                Ok(())
            }

            Self::Multicast(mcr) => {
                // we only send event client messages at the moment
                if let ClientMessage::Event(ecm) = msg {
                    mcr.send(ecm)?;
                }
                Ok(())
            }
        }
    }

    pub fn subscribe(&mut self, subid: String, filter: Vec<Filter>) -> Result<()> {
        self.send(&ClientMessage::req(subid, filter))
    }

    pub fn websocket(relay: Relay) -> Self {
        Self::Websocket(WebsocketRelay::new(relay))
    }

    pub fn multicast(wakeup: impl Fn() + Send + Sync + Clone + 'static) -> Result<Self> {
        Ok(Self::Multicast(setup_multicast_relay(wakeup)?))
    }
}

impl WebsocketRelay {
    pub fn new(relay: Relay) -> Self {
        Self {
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
    pub ping_rate: Duration,
    pub debug: Option<SubsDebug>,
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
            ping_rate: Duration::from_secs(45),
            debug: None,
        }
    }

    pub fn add_multicast_relay(
        &mut self,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        let multicast_relay = PoolRelay::multicast(wakeup)?;
        self.relays.push(multicast_relay);
        Ok(())
    }

    pub fn use_debug(&mut self) {
        self.debug = Some(SubsDebug::default());
    }

    pub fn ping_rate(&mut self, duration: Duration) -> &mut Self {
        self.ping_rate = duration;
        self
    }

    pub fn has(&self, url: &str) -> bool {
        for relay in &self.relays {
            if relay.url() == url {
                return true;
            }
        }

        false
    }

    pub fn urls(&self) -> BTreeSet<String> {
        self.relays
            .iter()
            .map(|pool_relay| pool_relay.url().to_string())
            .collect()
    }

    /// Check if a relay URL is in the pool and return its connection status.
    ///
    /// Returns Some(RelayStatus) if the relay is in the pool, None otherwise.
    /// Used for grace period logic when multiple relay hints are provided.
    pub fn relay_status(&self, url: &str) -> Option<RelayStatus> {
        let normalized = Self::canonicalize_url(url.to_string());
        self.relays
            .iter()
            .find(|r| Self::canonicalize_url(r.url().to_string()) == normalized)
            .map(|r| r.status())
    }

    /// Check if any of the given relay URLs are currently connecting (not yet connected).
    ///
    /// Returns true if at least one relay is in Connecting state, which indicates
    /// we should wait before querying to give it time to establish connection.
    pub fn has_connecting_relays<I, S>(&self, urls: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        urls.into_iter().any(|url| {
            matches!(self.relay_status(url.as_ref()), Some(RelayStatus::Connecting))
        })
    }

    pub fn send(&mut self, cmd: &ClientMessage) {
        for relay in &mut self.relays {
            if let Some(debug) = &mut self.debug {
                debug.send_cmd(relay.url().to_owned(), cmd);
            }
            if let Err(err) = relay.send(cmd) {
                error!("error sending {:?} to {}: {err}", cmd, relay.url());
            }
        }
    }

    pub fn unsubscribe(&mut self, subid: String) {
        for relay in &mut self.relays {
            let cmd = ClientMessage::close(subid.clone());
            if let Some(debug) = &mut self.debug {
                debug.send_cmd(relay.url().to_owned(), &cmd);
            }
            if let Err(err) = relay.send(&cmd) {
                error!(
                    "error unsubscribing from {} on {}: {err}",
                    &subid,
                    relay.url()
                );
            }
        }
    }

    pub fn subscribe(&mut self, subid: String, filter: Vec<Filter>) {
        for relay in &mut self.relays {
            if let Some(debug) = &mut self.debug {
                debug.send_cmd(
                    relay.url().to_owned(),
                    &ClientMessage::req(subid.clone(), filter.clone()),
                );
            }

            if let Err(err) = relay.send(&ClientMessage::req(subid.clone(), filter.clone())) {
                error!("error subscribing to {}: {err}", relay.url());
            }
        }
    }

    /// Keep relay connectiongs alive by pinging relays that haven't been
    /// pinged in awhile. Adjust ping rate with [`ping_rate`].
    pub fn keepalive_ping(&mut self, wakeup: impl Fn() + Send + Sync + Clone + 'static) {
        for relay in &mut self.relays {
            let now = std::time::Instant::now();

            match relay {
                PoolRelay::Multicast(_) => {}
                PoolRelay::Websocket(relay) => {
                    match relay.relay.status {
                        RelayStatus::Disconnected => {
                            let reconnect_at =
                                relay.last_connect_attempt + relay.retry_connect_after;
                            if now > reconnect_at {
                                relay.last_connect_attempt = now;
                                let next_duration = Duration::from_millis(3000);
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
                            relay.retry_connect_after =
                                WebsocketRelay::initial_reconnect_duration();

                            let should_ping = now - relay.last_ping > self.ping_rate;
                            if should_ping {
                                trace!("pinging {}", relay.relay.url);
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
        }
    }

    pub fn send_to(&mut self, cmd: &ClientMessage, relay_url: &str) {
        for relay in &mut self.relays {
            if relay.url() == relay_url {
                if let Some(debug) = &mut self.debug {
                    debug.send_cmd(relay.url().to_owned(), cmd);
                }
                if let Err(err) = relay.send(cmd) {
                    error!("send_to err: {err}");
                }
                return;
            }
        }
    }

    /// Subscribe to specific relays by URL.
    ///
    /// This enables hint-based routing where subscriptions are sent only to
    /// relays that are likely to have the requested events (e.g., from NIP-19
    /// relay hints). Relays not in the pool are silently skipped.
    ///
    /// URLs are normalized before comparison (trailing slashes, etc.) to handle
    /// minor formatting differences between hint sources and pool URLs.
    pub fn subscribe_to<I, S>(&mut self, subid: String, filter: Vec<Filter>, relay_urls: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Normalize all input URLs for comparison
        let urls: std::collections::HashSet<String> = relay_urls
            .into_iter()
            .map(|s| Self::canonicalize_url(s.as_ref().to_string()))
            .collect();

        for relay in &mut self.relays {
            // Pool URLs are already canonicalized via add_url
            if !urls.contains(relay.url()) {
                continue;
            }

            let cmd = ClientMessage::req(subid.clone(), filter.clone());
            if let Some(debug) = &mut self.debug {
                debug.send_cmd(relay.url().to_owned(), &cmd);
            }

            if let Err(err) = relay.send(&cmd) {
                error!("subscribe_to error for {}: {err}", relay.url());
            }
        }
    }

    /// check whether a relay url is valid to add
    pub fn is_valid_url(&self, url: &str) -> bool {
        if url.is_empty() {
            return false;
        }
        let url = match Url::parse(url) {
            Ok(parsed_url) => parsed_url.to_string(),
            Err(_err) => {
                // debug!("bad relay url \"{}\": {:?}", url, err);
                return false;
            }
        };
        if self.has(&url) {
            return false;
        }
        true
    }

    // Adds a websocket url to the RelayPool.
    pub fn add_url(
        &mut self,
        url: String,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        let url = Self::canonicalize_url(url);
        // Check if the URL already exists in the pool.
        if self.has(&url) {
            return Ok(());
        }
        let relay = Relay::new(
            nostr::RelayUrl::parse(url).map_err(|_| Error::InvalidRelayUrl)?,
            wakeup,
        )?;
        let pool_relay = PoolRelay::websocket(relay);

        self.relays.push(pool_relay);

        Ok(())
    }

    pub fn add_urls(
        &mut self,
        urls: BTreeSet<String>,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        for url in urls {
            self.add_url(url, wakeup.clone())?;
        }
        Ok(())
    }

    pub fn remove_urls(&mut self, urls: &BTreeSet<String>) {
        self.relays
            .retain(|pool_relay| !urls.contains(pool_relay.url()));
    }

    /// Standardize the format of relay URLs (e.g., trailing slashes).
    ///
    /// This ensures consistent URL comparison by normalizing formatting
    /// differences. Uses the url crate's parsing to canonicalize.
    pub fn canonicalize_url(url: String) -> String {
        match Url::parse(&url) {
            Ok(parsed_url) => parsed_url.to_string(),
            Err(_) => url, // If parsing fails, return the original URL.
        }
    }

    /// Attempts to receive a pool event from a list of relays. The
    /// function searches each relay in the list in order, attempting to
    /// receive a message from each. If a message is received, return it.
    /// If no message is received from any relays, None is returned.
    pub fn try_recv(&mut self) -> Option<PoolEvent<'_>> {
        for relay in &mut self.relays {
            if let PoolRelay::Multicast(mcr) = relay {
                // try rejoin on multicast
                if mcr.should_rejoin() {
                    if let Err(err) = mcr.rejoin() {
                        error!("multicast: rejoin error: {err}");
                    }
                }
            }

            if let Some(event) = relay.try_recv() {
                match &event {
                    WsEvent::Opened => {
                        relay.set_status(RelayStatus::Connected);
                    }
                    WsEvent::Closed => {
                        relay.set_status(RelayStatus::Disconnected);
                    }
                    WsEvent::Error(err) => {
                        error!("{:?}", err);
                        relay.set_status(RelayStatus::Disconnected);
                    }
                    WsEvent::Message(ev) => {
                        // let's just handle pongs here.
                        // We only need to do this natively.
                        #[cfg(not(target_arch = "wasm32"))]
                        if let WsMessage::Ping(ref bs) = ev {
                            trace!("pong {}", relay.url());
                            match relay {
                                PoolRelay::Websocket(wsr) => {
                                    wsr.relay.sender.send(WsMessage::Pong(bs.to_owned()));
                                }
                                PoolRelay::Multicast(_mcr) => {}
                            }
                        }
                    }
                }

                if let Some(debug) = &mut self.debug {
                    debug.receive_cmd(relay.url().to_owned(), (&event).into());
                }

                let pool_event = PoolEvent {
                    event,
                    relay: relay.url(),
                };

                return Some(pool_event);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test URL normalization handles trailing slashes consistently.
    #[test]
    fn test_canonicalize_url_trailing_slash() {
        // URLs should be normalized to include trailing slash
        let with_slash = RelayPool::canonicalize_url("wss://relay.damus.io/".to_string());
        let without_slash = RelayPool::canonicalize_url("wss://relay.damus.io".to_string());

        assert_eq!(with_slash, without_slash);
    }

    /// Test URL normalization handles different cases.
    #[test]
    fn test_canonicalize_url_various() {
        // Standard websocket URL
        let url1 = RelayPool::canonicalize_url("wss://nos.lol".to_string());
        assert!(url1.starts_with("wss://"));

        // URL with path
        let url2 = RelayPool::canonicalize_url("wss://relay.example.com/nostr".to_string());
        assert!(url2.contains("/nostr"));

        // Invalid URL should return as-is
        let invalid = RelayPool::canonicalize_url("not-a-url".to_string());
        assert_eq!(invalid, "not-a-url");
    }

    /// Test that subscribe_to normalizes URLs before matching.
    #[test]
    fn test_subscribe_to_url_normalization() {
        // This is a unit test for the normalization logic in subscribe_to
        // We can't easily test the full subscribe_to without mocking relays,
        // but we can verify the URL set is built correctly

        let urls: std::collections::HashSet<String> = ["wss://relay.damus.io"]
            .into_iter()
            .map(|s| RelayPool::canonicalize_url(s.to_string()))
            .collect();

        // Both with and without trailing slash should match after normalization
        let normalized = RelayPool::canonicalize_url("wss://relay.damus.io/".to_string());
        assert!(urls.contains(&normalized));
    }
}
