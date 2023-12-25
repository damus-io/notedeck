use crate::relay::message::{RelayEvent, RelayMessage};
use crate::relay::{Relay, RelayStatus};
use crate::{ClientMessage, Result};

use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use ewebsock::{WsEvent, WsMessage};

#[cfg(not(target_arch = "wasm32"))]
use tracing::debug;

use tracing::error;

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
            relay: relay,
            last_ping: Instant::now(),
            last_connect_attempt: Instant::now(),
            retry_connect_after: Self::initial_reconnect_duration(),
        }
    }

    pub fn initial_reconnect_duration() -> Duration {
        Duration::from_secs(2)
    }
}

pub struct RelayPool {
    pub relays: Vec<PoolRelay>,
    pub ping_rate: Duration,
}

impl RelayPool {
    // Constructs a new, empty RelayPool.
    pub fn new() -> RelayPool {
        RelayPool {
            relays: vec![],
            ping_rate: Duration::from_secs(25),
        }
    }

    pub fn ping_rate(&mut self, duration: Duration) -> &mut Self {
        self.ping_rate = duration;
        self
    }

    pub fn has(&self, url: &str) -> bool {
        for relay in &self.relays {
            if &relay.relay.url == url {
                return true;
            }
        }
        return false;
    }

    pub fn send(&mut self, cmd: &ClientMessage) {
        for relay in &mut self.relays {
            relay.relay.send(cmd);
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
                        relay.relay.connect(wakeup.clone());
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

    // Adds a websocket url to the RelayPool.
    pub fn add_url(
        &mut self,
        url: String,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        let relay = Relay::new(url, wakeup)?;
        let pool_relay = PoolRelay::new(relay);

        self.relays.push(pool_relay);

        Ok(())
    }

    /// Attempts to receive a pool event from a list of relays. The
    /// function searches each relay in the list in order, attempting to
    /// receive a message from each. If a message is received, return it.
    /// If no message is received from any relays, None is returned.
    pub fn try_recv<'a>(&'a mut self) -> Option<PoolEvent<'a>> {
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
                        match &ev {
                            WsMessage::Ping(ref bs) => {
                                debug!("pong {}", &relay.url);
                                relay.sender.send(WsMessage::Pong(bs.to_owned()));
                            }
                            _ => {}
                        }
                        return Some(PoolEvent {
                            event,
                            relay: &relay.url,
                        });
                    }
                }
            }
        }

        None
    }
}
