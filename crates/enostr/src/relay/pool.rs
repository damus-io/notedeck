use crate::relay::{setup_multicast_relay, MulticastRelay, Relay, RelayStatus};
use crate::{ClientMessage, Result};
use nostrdb::Filter;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use ewebsock::{WsEvent, WsMessage};

#[cfg(not(target_arch = "wasm32"))]
use tracing::{debug, error};

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
            Self::Websocket(wsr) => &wsr.relay.url,
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
            ping_rate: Duration::from_secs(25),
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
        let relay = Relay::new(url, wakeup)?;
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

    // standardize the format (ie, trailing slashes)
    fn canonicalize_url(url: String) -> String {
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
                            debug!("pong {}", relay.url());
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
