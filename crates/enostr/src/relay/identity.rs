use std::{
    borrow::Borrow,
    fmt::{self, Display},
};

use hashbrown::HashSet;
use nostr::types::RelayUrl;
use url::Url;
use uuid::Uuid;

use crate::Error;

#[derive(Eq, PartialEq, Hash, Clone, Debug)]
pub enum RelayId {
    Websocket(NormRelayUrl),
    Multicast,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct OutboxSubId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RelayReqStatus {
    InitialQuery,
    Eose,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelayReqId(pub String);

impl RelayReqId {
    pub fn byte_len() -> usize {
        uuid::fmt::Hyphenated::LENGTH
    }
}

impl Default for RelayReqId {
    fn default() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl From<String> for RelayReqId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<RelayReqId> for String {
    fn from(value: RelayReqId) -> Self {
        value.0
    }
}

impl From<&str> for RelayReqId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<Uuid> for RelayReqId {
    fn from(value: Uuid) -> Self {
        RelayReqId(value.to_string())
    }
}

impl std::fmt::Display for RelayReqId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Borrow<str> for RelayReqId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, PartialOrd, Ord)]
pub struct NormRelayUrl {
    url: RelayUrl,
}

impl NormRelayUrl {
    pub fn new(url: &str) -> Result<Self, Error> {
        Ok(Self {
            url: nostr::RelayUrl::parse(canonicalize_url(url.to_owned()))
                .map_err(|_| Error::InvalidRelayUrl)?,
        })
    }
}

impl Display for NormRelayUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

impl From<NormRelayUrl> for RelayUrl {
    fn from(value: NormRelayUrl) -> Self {
        value.url
    }
}

impl From<RelayUrl> for NormRelayUrl {
    fn from(url: RelayUrl) -> Self {
        Self { url }
    }
}

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub enum RelayType {
    Compaction,
    Transparent,
}

#[derive(Default, Clone, Debug)]
pub struct RelayUrlPkgs {
    pub urls: HashSet<NormRelayUrl>,
    pub use_transparent: bool,
}

impl RelayUrlPkgs {
    pub fn iter(&self) -> impl Iterator<Item = &NormRelayUrl> {
        self.urls.iter()
    }

    pub fn new(urls: HashSet<NormRelayUrl>) -> Self {
        Self {
            urls,
            use_transparent: false,
        }
    }
}

// standardize the format (ie, trailing slashes)
fn canonicalize_url(url: String) -> String {
    match Url::parse(&url) {
        Ok(parsed_url) => parsed_url.to_string(),
        Err(_) => url, // If parsing fails, return the original URL.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== NormRelayUrl tests ====================

    #[test]
    fn norm_relay_url_creates_valid_url() {
        let url = NormRelayUrl::new("wss://relay.example.com");
        assert!(url.is_ok());
    }

    #[test]
    fn norm_relay_url_handles_trailing_slash() {
        let url1 = NormRelayUrl::new("wss://relay.example.com/").unwrap();
        let url2 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        // Both should canonicalize to the same thing
        assert_eq!(url1.to_string(), url2.to_string());
    }

    #[test]
    fn norm_relay_url_rejects_invalid() {
        assert!(NormRelayUrl::new("not-a-url").is_err());
    }

    #[test]
    fn norm_relay_url_rejects_http() {
        // nostr relay URLs must be ws:// or wss://
        assert!(NormRelayUrl::new("http://relay.example.com").is_err());
    }

    #[test]
    fn norm_relay_url_equality() {
        let url1 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        let url2 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        assert_eq!(url1, url2);
    }

    #[test]
    fn norm_relay_url_hash_consistency() {
        use std::collections::HashSet;

        let url1 = NormRelayUrl::new("wss://relay.example.com").unwrap();
        let url2 = NormRelayUrl::new("wss://relay.example.com").unwrap();

        let mut set = HashSet::new();
        set.insert(url1);
        assert!(set.contains(&url2));
    }

    // ==================== RelayUrlPkgs tests ====================

    #[test]
    fn relay_url_pkgs_default_not_transparent() {
        let pkgs = RelayUrlPkgs::default();
        assert!(!pkgs.use_transparent);
        assert!(pkgs.urls.is_empty());
    }

    #[test]
    fn relay_url_pkgs_new_sets_urls() {
        let mut urls = HashSet::new();
        urls.insert(NormRelayUrl::new("wss://relay1.example.com").unwrap());
        urls.insert(NormRelayUrl::new("wss://relay2.example.com").unwrap());

        let pkgs = RelayUrlPkgs::new(urls);
        assert_eq!(pkgs.urls.len(), 2);
        assert!(!pkgs.use_transparent);
    }

    #[test]
    fn relay_url_pkgs_iter() {
        let mut urls = HashSet::new();
        urls.insert(NormRelayUrl::new("wss://relay1.example.com").unwrap());

        let pkgs = RelayUrlPkgs::new(urls);
        assert_eq!(pkgs.iter().count(), 1);
    }

    // ==================== RelayREQId tests ====================

    #[test]
    fn relay_req_id_default_generates_uuid() {
        let id1 = RelayReqId::default();
        let id2 = RelayReqId::default();
        // Each default should generate a unique UUID
        assert_ne!(id1, id2);
    }

    // ==================== SubRequestId tests ====================

    #[test]
    fn sub_request_id_equality() {
        let id1 = OutboxSubId(42);
        let id2 = OutboxSubId(42);
        let id3 = OutboxSubId(43);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn sub_request_id_ordering() {
        let id1 = OutboxSubId(1);
        let id2 = OutboxSubId(2);

        assert!(id1 < id2);
    }
}
