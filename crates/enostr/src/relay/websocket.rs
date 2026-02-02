use crate::{relay::RelayStatus, ClientMessage, Result};

use std::{
    fmt,
    hash::{Hash, Hasher},
};

use ewebsock::{Options, WsMessage, WsReceiver, WsSender};
use tracing::{debug, error};

pub struct Relay {
    pub url: nostr::RelayUrl,
    pub status: RelayStatus,
    pub sender: WsSender,
    pub receiver: WsReceiver,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("url", &self.url)
            .field("status", &self.status)
            .finish()
    }
}

impl Hash for Relay {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hashes the Relay by hashing the URL
        self.url.hash(state);
    }
}

impl PartialEq for Relay {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
    }
}

impl Eq for Relay {}

impl Relay {
    pub fn new(url: nostr::RelayUrl, wakeup: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let status = RelayStatus::Connecting;
        let (sender, receiver) =
            ewebsock::connect_with_wakeup(url.as_str(), Options::default(), wakeup)?;

        Ok(Self {
            url,
            sender,
            receiver,
            status,
        })
    }

    pub fn send(&mut self, msg: &ClientMessage) {
        let json = match msg.to_json() {
            Ok(json) => {
                debug!("sending {} to {}", json, self.url);
                json
            }
            Err(e) => {
                error!("error serializing json for filter: {e}");
                return;
            }
        };

        let txt = WsMessage::Text(json);
        self.sender.send(txt);
    }

    pub fn connect(&mut self, wakeup: impl Fn() + Send + Sync + 'static) -> Result<()> {
        let (sender, receiver) =
            ewebsock::connect_with_wakeup(self.url.as_str(), Options::default(), wakeup)?;
        self.status = RelayStatus::Connecting;
        self.sender = sender;
        self.receiver = receiver;
        Ok(())
    }

    pub fn ping(&mut self) {
        let msg = WsMessage::Ping(vec![]);
        self.sender.send(msg);
    }
}
