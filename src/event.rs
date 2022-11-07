use serde_derive::{Deserialize, Serialize};

/// Event is the struct used to represent a Nostr event
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    /// 32-bytes sha256 of the the serialized event data
    pub id: String,
    /// 32-bytes hex-encoded public key of the event creator
    #[serde(rename = "pubkey")]
    pub pub_key: String,
    /// unix timestamp in seconds
    pub created_at: u64,
    /// integer
    /// 0: NostrEvent
    pub kind: u8,
    /// Tags
    pub tags: Vec<Vec<String>>,
    /// arbitrary string
    pub content: String,
    /// 64-bytes signature of the sha256 hash of the serialized event data, which is the same as the "id" field
    pub sig: String,
}
