use crate::relay::message::RelayEvent;
use crate::relay::Relay;
use crate::{ClientMessage, Result};

#[cfg(not(target_arch = "wasm32"))]
use ewebsock::WsMessage;

#[cfg(not(target_arch = "wasm32"))]
use tracing::debug;

use tracing::error;

#[derive(Debug)]
pub struct PoolEvent<'a> {
    pub relay: &'a str,
    pub event: RelayEvent,
}

pub struct RelayPool {
    pub relays: Vec<Relay>,
}

impl Default for RelayPool {
    fn default() -> RelayPool {
        RelayPool { relays: Vec::new() }
    }
}

impl RelayPool {
    // Constructs a new, empty RelayPool.
    pub fn new() -> RelayPool {
        RelayPool { relays: vec![] }
    }

    pub fn has(&self, url: &str) -> bool {
        for relay in &self.relays {
            if &relay.url == url {
                return true;
            }
        }
        return false;
    }

    pub fn send(&mut self, cmd: &ClientMessage) {
        for relay in &mut self.relays {
            relay.send(cmd);
        }
    }

    pub fn send_to(&mut self, cmd: &ClientMessage, relay_url: &str) {
        for relay in &mut self.relays {
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
        wakeup: impl Fn() + Send + Sync + 'static,
    ) -> Result<()> {
        let relay = Relay::new(url, wakeup)?;

        self.relays.push(relay);

        Ok(())
    }

    /// Attempts to receive a pool event from a list of relays. The function searches each relay in the list in order, attempting to receive a message from each. If a message is received, return it. If no message is received from any relays, None is returned.
    pub fn try_recv(&mut self) -> Option<PoolEvent<'_>> {
        for relay in &mut self.relays {
            if let Some(msg) = relay.receiver.try_recv() {
                match msg.try_into() {
                    Ok(event) => {
                        // let's just handle pongs here.
                        // We only need to do this natively.
                        #[cfg(not(target_arch = "wasm32"))]
                        match event {
                            RelayEvent::Other(WsMessage::Ping(ref bs)) => {
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

                    Err(e) => {
                        error!("{:?}", e);
                        continue;
                    }
                }
            }
        }

        None
    }
}
