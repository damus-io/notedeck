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
