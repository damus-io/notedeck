use crate::{Error, Result};
use serde_derive::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// Event is the struct used to represent a Nostr event
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    /// 32-bytes sha256 of the the serialized event data
    pub id: String,
    /// 32-bytes hex-encoded public key of the event creator
    #[serde(rename = "pubkey")]
    pub pubkey: String,
    /// unix timestamp in seconds
    pub created_at: u64,
    /// integer
    /// 0: NostrEvent
    pub kind: u64,
    /// Tags
    pub tags: Vec<Vec<String>>,
    /// arbitrary string
    pub content: String,
    /// 64-bytes signature of the sha256 hash of the serialized event data, which is the same as the "id" field
    pub sig: String,
}

// Implement Hash trait
impl Hash for Event {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Event {}

impl Event {
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(Into::into)
    }

    pub fn verify(&self) -> Result<Self> {
        return Err(Error::InvalidSignature);
    }

    /// This is just for serde sanity checking
    #[allow(dead_code)]
    pub(crate) fn new_dummy(
        id: &str,
        pubkey: &str,
        created_at: u64,
        kind: u64,
        tags: Vec<Vec<String>>,
        content: &str,
        sig: &str,
    ) -> Result<Self> {
        let event = Event {
            id: id.to_string(),
            pubkey: pubkey.to_string(),
            created_at,
            kind,
            tags,
            content: content.to_string(),
            sig: sig.to_string(),
        };

        event.verify()
    }
}

impl std::str::FromStr for Event {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Event::from_json(s)
    }
}
