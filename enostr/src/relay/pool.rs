use crate::relay::message::RelayEvent;
use crate::relay::Relay;
use crate::Result;

#[derive(Debug)]
pub struct PoolMessage<'a> {
    relay: &'a str,
    event: RelayEvent,
}

pub struct RelayPool {
    relays: Vec<Relay>,
}

impl Default for RelayPool {
    fn default() -> RelayPool {
        RelayPool { relays: Vec::new() }
    }
}

impl RelayPool {
    // Constructs a new, empty RelayPool.
    pub fn new(relays: Vec<Relay>) -> RelayPool {
        RelayPool { relays: relays }
    }

    pub fn has(&self, url: &str) -> bool {
        for relay in &self.relays {
            if &relay.url == url {
                return true;
            }
        }
        return false;
    }

    // Adds a websocket url to the RelayPool.
    pub fn add_url(&mut self, url: String) -> Result<()> {
        let relay = Relay::new(url)?;

        self.relays.push(relay);

        Ok(())
    }

    pub fn try_recv(&self) -> Option<PoolMessage<'_>> {
        for relay in &self.relays {
            if let Some(msg) = relay.receiver.try_recv() {
                if let Ok(event) = msg.try_into() {
                    let pmsg = PoolMessage {
                        event,
                        relay: &relay.url,
                    };
                    return Some(pmsg);
                }
            }
        }

        None
    }

    pub fn connect() {}
}
