use crate::{Error, Pubkey};
use log::debug;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::hash::{Hash, Hasher};

/// Event is the struct used to represent a Nostr event
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    /// 32-bytes sha256 of the the serialized event data
    pub id: EventId,
    /// 32-bytes hex-encoded public key of the event creator
    pub pubkey: Pubkey,
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
        self.id.0.hash(state);
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Event {}

impl Event {
    pub fn from_json(s: &str) -> Result<Self, Error> {
        serde_json::from_str(s).map_err(Into::into)
    }

    pub fn verify(&self) -> Result<Self, Error> {
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
    ) -> Result<Self, Error> {
        Ok(Event {
            id: EventId::from_hex(id)?,
            pubkey: Pubkey::from_hex(pubkey)?,
            created_at,
            kind,
            tags,
            content: content.to_string(),
            sig: sig.to_string(),
        })
    }
}

impl std::str::FromStr for Event {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Event::from_json(s)
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct EventId([u8; 32]);

impl EventId {
    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        let evid = EventId(hex::decode(hex_str)?.as_slice().try_into().unwrap());
        Ok(evid)
    }
}

// Custom serialize function for Pubkey
impl Serialize for EventId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.hex())
    }
}

// Custom deserialize function for Pubkey
impl<'de> Deserialize<'de> for EventId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        EventId::from_hex(&s).map_err(serde::de::Error::custom)
    }
}
