use ewebsock::{WsReceiver, WsSender};

use crate::Result;
use std::fmt;
use std::hash::{Hash, Hasher};

pub mod message;
pub mod pool;

#[derive(Debug)]
pub enum RelayStatus {
    Connected,
    Connecting,
    Disconnected,
}

pub struct Relay {
    pub url: String,
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
    pub fn new(url: String) -> Result<Self> {
        let status = RelayStatus::Connecting;
        let (sender, receiver) = ewebsock::connect(&url)?;

        Ok(Self {
            url,
            sender,
            receiver,
            status,
        })
    }
}
