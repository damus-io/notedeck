use crate::{Error, Pubkey, Result};
use hex;
use serde_derive::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

/// Event is the struct used to represent a Nostr event
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    /// 32-bytes sha256 of the the serialized event data
    pub id: NoteId,
    /// 32-bytes hex-encoded public key of the event creator
    #[serde(rename = "pubkey")]
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
            id: id.try_into()?,
            pubkey: pubkey.to_string().into(),
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

#[derive(Serialize, Debug, Eq, PartialEq, Clone, Hash)]
pub struct NoteId([u8; 32]);

// Implement `Deserialize` for `NoteId`.
impl<'de> serde::Deserialize<'de> for NoteId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize the JSON string
        let s = String::deserialize(deserializer)?;

        // Convert the hex string to bytes
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;

        // Check that the length is exactly 32
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("Expected exactly 32 bytes"));
        }

        // Convert the Vec<u8> to [u8; 32]
        let mut array = [0; 32];
        array.copy_from_slice(&bytes);

        Ok(NoteId(array))
    }
}

impl TryFrom<String> for NoteId {
    type Error = hex::FromHexError;

    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        let s: &str = &s;
        NoteId::try_from(s)
    }
}

impl From<[u8; 32]> for NoteId {
    fn from(s: [u8; 32]) -> Self {
        NoteId(s)
    }
}

impl TryFrom<&str> for NoteId {
    type Error = hex::FromHexError;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        let decoded = hex::decode(s)?;
        match decoded.try_into() {
            Ok(bs) => Ok(NoteId(bs)),
            Err(_) => Err(hex::FromHexError::InvalidStringLength),
        }
    }
}
