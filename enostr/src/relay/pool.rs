use crate::relay::message::RelayEvent;
use crate::relay::Relay;
use crate::Result;
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

    pub fn try_recv(&self) -> Option<PoolEvent<'_>> {
        for relay in &self.relays {
            if let Some(msg) = relay.receiver.try_recv() {
                match msg.try_into() {
                    Ok(event) => {
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

    pub fn connect() {}
}
