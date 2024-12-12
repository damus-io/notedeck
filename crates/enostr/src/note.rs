use crate::{Error, Pubkey};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct NoteId([u8; 32]);

impl fmt::Debug for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hex())
    }
}

static HRP_NOTE: bech32::Hrp = bech32::Hrp::parse_unchecked("note");

impl NoteId {
    pub fn new(bytes: [u8; 32]) -> Self {
        NoteId(bytes)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, Error> {
        let evid = NoteId(hex::decode(hex_str)?.as_slice().try_into().unwrap());
        Ok(evid)
    }

    pub fn to_bech(&self) -> Option<String> {
        bech32::encode::<bech32::Bech32>(HRP_NOTE, &self.0).ok()
    }
}

/// Event is the struct used to represent a Nostr event
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Note {
    /// 32-bytes sha256 of the the serialized event data
    pub id: NoteId,
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
impl Hash for Note {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.0.hash(state);
    }
}

impl PartialEq for Note {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Note {}

impl Note {
    pub fn from_json(s: &str) -> Result<Self, Error> {
        serde_json::from_str(s).map_err(Into::into)
    }

    pub fn verify(&self) -> Result<Self, Error> {
        Err(Error::InvalidSignature)
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
        Ok(Note {
            id: NoteId::from_hex(id)?,
            pubkey: Pubkey::from_hex(pubkey)?,
            created_at,
            kind,
            tags,
            content: content.to_string(),
            sig: sig.to_string(),
        })
    }
}

impl std::str::FromStr for Note {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Note::from_json(s)
    }
}

// Custom serialize function for Pubkey
impl Serialize for NoteId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.hex())
    }
}

// Custom deserialize function for Pubkey
impl<'de> Deserialize<'de> for NoteId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NoteId::from_hex(&s).map_err(serde::de::Error::custom)
    }
}
