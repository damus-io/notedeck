//! NIP-11 Relay Information Document support
//!
//! Fetches and caches relay metadata including server limitations.

use http_body_util::{BodyExt, Empty, Limited};
use hyper::{
    body::Bytes,
    header::{self, HeaderValue},
    Request, Uri,
};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Default max_limit when relay limits are unknown (conservative, works everywhere)
pub const DEFAULT_MAX_LIMIT: usize = 500;

/// Default for max_event_tags when not specified (conservative for filter size)
pub const DEFAULT_MAX_EVENT_TAGS: usize = 100;

/// Default max subscriptions when not specified
pub const DEFAULT_MAX_SUBSCRIPTIONS: usize = 20;

/// How long to cache relay info before re-fetching
const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour

/// Maximum body size for NIP-11 response
const MAX_BODY_BYTES: usize = 64 * 1024; // 64KB should be plenty for relay info

/// NIP-11 Relay Information Document
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RelayInfo {
    pub name: Option<String>,
    pub description: Option<String>,
    pub pubkey: Option<String>,
    pub contact: Option<String>,
    pub supported_nips: Option<Vec<u32>>,
    pub software: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub limitation: RelayLimitation,
}

/// Server limitations from NIP-11
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RelayLimitation {
    pub max_message_length: Option<usize>,
    pub max_subscriptions: Option<usize>,
    pub max_limit: Option<usize>,
    pub max_subid_length: Option<usize>,
    pub max_event_tags: Option<usize>,
    pub max_content_length: Option<usize>,
    pub min_pow_difficulty: Option<u32>,
    pub auth_required: Option<bool>,
    pub payment_required: Option<bool>,
}

impl RelayInfo {
    /// Get the maximum event tags allowed, falling back to default
    pub fn max_event_tags(&self) -> usize {
        self.limitation
            .max_event_tags
            .unwrap_or(DEFAULT_MAX_EVENT_TAGS)
    }

    /// Get the maximum limit for queries, falling back to default
    pub fn max_limit(&self) -> usize {
        self.limitation.max_limit.unwrap_or(DEFAULT_MAX_LIMIT)
    }

    /// Get the maximum subscriptions allowed, falling back to default
    pub fn max_subscriptions(&self) -> usize {
        self.limitation
            .max_subscriptions
            .unwrap_or(DEFAULT_MAX_SUBSCRIPTIONS)
    }
}

/// Cached relay info entry
struct CacheEntry {
    info: RelayInfo,
    fetched_at: Instant,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.fetched_at.elapsed() > CACHE_TTL
    }
}

/// Cache for relay information documents
#[derive(Default, Clone)]
pub struct RelayInfoCache {
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
    /// Track URLs currently being fetched to avoid duplicate requests
    in_flight: Arc<RwLock<HashSet<String>>>,
}

impl RelayInfoCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get cached relay info if available and not expired
    pub fn get(&self, relay_url: &str) -> Option<RelayInfo> {
        let normalized = normalize_relay_url(relay_url);
        let cache = self.cache.read().ok()?;
        let entry = cache.get(&normalized)?;
        if entry.is_expired() {
            None
        } else {
            Some(entry.info.clone())
        }
    }

    /// Store relay info in cache
    pub fn insert(&self, relay_url: &str, info: RelayInfo) {
        let normalized = normalize_relay_url(relay_url);
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(
                normalized,
                CacheEntry {
                    info,
                    fetched_at: Instant::now(),
                },
            );
        }
    }

    /// Get the minimum max_event_tags across specified relays
    ///
    /// Returns DEFAULT_MAX_EVENT_TAGS if no relays are cached or all have None
    pub fn min_max_event_tags(&self, relay_urls: &[&str]) -> usize {
        self.min_limit(
            relay_urls,
            |info| info.limitation.max_event_tags,
            DEFAULT_MAX_EVENT_TAGS,
        )
    }

    /// Get the minimum max_limit across specified relays
    ///
    /// Returns DEFAULT_MAX_LIMIT if no relays are cached or all have None
    pub fn min_max_limit(&self, relay_urls: &[&str]) -> usize {
        self.min_limit(
            relay_urls,
            |info| info.limitation.max_limit,
            DEFAULT_MAX_LIMIT,
        )
    }

    /// Get the minimum max_subscriptions across specified relays
    pub fn min_max_subscriptions(&self, relay_urls: &[&str]) -> usize {
        self.min_limit(
            relay_urls,
            |info| info.limitation.max_subscriptions,
            DEFAULT_MAX_SUBSCRIPTIONS,
        )
    }

    /// Generic helper to get minimum of a limit field across relays
    fn min_limit<F>(&self, relay_urls: &[&str], get_field: F, default: usize) -> usize
    where
        F: Fn(&RelayInfo) -> Option<usize>,
    {
        let cache = match self.cache.read() {
            Ok(c) => c,
            Err(_) => return default,
        };

        let mut min_value: Option<usize> = None;

        for url in relay_urls {
            let normalized = normalize_relay_url(url);
            if let Some(entry) = cache.get(&normalized) {
                if !entry.is_expired() {
                    if let Some(value) = get_field(&entry.info) {
                        min_value = Some(min_value.map_or(value, |m| m.min(value)));
                    }
                }
            }
        }

        min_value.unwrap_or(default)
    }

    /// Get URLs of relays that haven't been fetched yet (and aren't in flight)
    pub fn unfetched_relays(&self, relay_urls: &[&str]) -> Vec<String> {
        let cache = match self.cache.read() {
            Ok(c) => c,
            Err(_) => return relay_urls.iter().map(|s| s.to_string()).collect(),
        };

        let in_flight = match self.in_flight.read() {
            Ok(f) => f,
            Err(_) => return vec![],
        };

        relay_urls
            .iter()
            .filter(|url| {
                // Skip non-websocket URLs (e.g., "multicast")
                if !url.starts_with("ws://") && !url.starts_with("wss://") {
                    return false;
                }
                let normalized = normalize_relay_url(url);
                if in_flight.contains(&normalized) {
                    return false;
                }
                match cache.get(&normalized) {
                    Some(entry) => entry.is_expired(),
                    None => true,
                }
            })
            .map(|s| s.to_string())
            .collect()
    }

    /// Spawn async fetches for relay info
    ///
    /// This is non-blocking - results will be stored in the cache when ready.
    /// Returns the number of fetches spawned.
    pub fn fetch_relay_infos(&self, relay_urls: &[String]) -> usize {
        let mut count = 0;

        for url in relay_urls {
            let normalized = normalize_relay_url(url);

            // Mark as in-flight
            {
                let mut in_flight = match self.in_flight.write() {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                if in_flight.contains(&normalized) {
                    continue;
                }
                in_flight.insert(normalized.clone());
            }

            // Clone what we need for the async task
            let cache = self.cache.clone();
            let in_flight = self.in_flight.clone();
            let url_for_task = url.clone();

            tokio::spawn(async move {
                match fetch_relay_info(&url_for_task).await {
                    Ok(info) => {
                        info!(
                            "NIP-11 fetched for {}: max_event_tags={:?}, max_limit={:?}",
                            url_for_task, info.limitation.max_event_tags, info.limitation.max_limit
                        );
                        if let Ok(mut cache) = cache.write() {
                            let normalized = normalize_relay_url(&url_for_task);
                            cache.insert(
                                normalized,
                                CacheEntry {
                                    info,
                                    fetched_at: Instant::now(),
                                },
                            );
                        }
                    }
                    Err(e) => {
                        warn!("Failed to fetch NIP-11 for {}: {}", url_for_task, e);
                    }
                }

                // Remove from in-flight
                if let Ok(mut in_flight) = in_flight.write() {
                    in_flight.remove(&normalize_relay_url(&url_for_task));
                }
            });

            count += 1;
        }

        count
    }

    /// Check if any relays need NIP-11 fetching and spawn fetches
    ///
    /// Call this during update cycles to lazily populate relay info
    pub fn ensure_fetched(&self, relay_urls: &[&str]) {
        let unfetched = self.unfetched_relays(relay_urls);
        if !unfetched.is_empty() {
            let count = self.fetch_relay_infos(&unfetched);
            if count > 0 {
                debug!("Spawned {} NIP-11 fetches", count);
            }
        }
    }
}

/// Convert websocket URL to HTTP URL for NIP-11 fetch
fn normalize_relay_url(url: &str) -> String {
    url.trim_end_matches('/')
        .replace("wss://", "https://")
        .replace("ws://", "http://")
}

/// Fetch NIP-11 relay information document
pub async fn fetch_relay_info(relay_url: &str) -> Result<RelayInfo, RelayInfoError> {
    let http_url = normalize_relay_url(relay_url);
    let uri: Uri = http_url.parse().map_err(|_| RelayInfoError::InvalidUrl)?;

    let https = {
        let builder = HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|_| RelayInfoError::TlsError)?;
        builder.https_or_http().enable_http1().build()
    };

    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(https);

    let authority = uri.authority().ok_or(RelayInfoError::InvalidUrl)?.clone();

    let req = Request::builder()
        .uri(&uri)
        .header(header::HOST, authority.as_str())
        .header(
            header::ACCEPT,
            HeaderValue::from_static("application/nostr+json"),
        )
        .body(Empty::<Bytes>::new())
        .map_err(|e| RelayInfoError::Http(e.to_string()))?;

    debug!("Fetching NIP-11 from {}", http_url);

    let res = client
        .request(req)
        .await
        .map_err(|e| RelayInfoError::Http(e.to_string()))?;

    if !res.status().is_success() {
        return Err(RelayInfoError::HttpStatus(res.status().as_u16()));
    }

    // Check content type
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !content_type.contains("application/nostr+json")
        && !content_type.contains("application/json")
    {
        warn!(
            "Relay {} returned unexpected content type: {}",
            relay_url, content_type
        );
    }

    // Read body with size limit
    let limited_body = Limited::new(res.into_body(), MAX_BODY_BYTES);
    let collected = BodyExt::collect(limited_body)
        .await
        .map_err(|e| RelayInfoError::Http(e.to_string()))?;
    let bytes = collected.to_bytes();

    let info: RelayInfo =
        serde_json::from_slice(&bytes).map_err(|e| RelayInfoError::Parse(e.to_string()))?;

    debug!(
        "Got NIP-11 for {}: max_event_tags={:?}",
        relay_url, info.limitation.max_event_tags
    );

    Ok(info)
}

#[derive(Debug)]
pub enum RelayInfoError {
    InvalidUrl,
    TlsError,
    Http(String),
    HttpStatus(u16),
    Parse(String),
}

impl std::fmt::Display for RelayInfoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl => write!(f, "Invalid relay URL"),
            Self::TlsError => write!(f, "TLS initialization error"),
            Self::Http(e) => write!(f, "HTTP error: {}", e),
            Self::HttpStatus(s) => write!(f, "HTTP status: {}", s),
            Self::Parse(e) => write!(f, "Parse error: {}", e),
        }
    }
}

impl std::error::Error for RelayInfoError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_relay_url() {
        assert_eq!(
            normalize_relay_url("wss://relay.damus.io/"),
            "https://relay.damus.io"
        );
        assert_eq!(normalize_relay_url("wss://nos.lol"), "https://nos.lol");
        assert_eq!(
            normalize_relay_url("ws://localhost:8080/"),
            "http://localhost:8080"
        );
    }

    #[test]
    fn test_parse_relay_info() {
        let json = r#"{
            "name": "Test Relay",
            "limitation": {
                "max_event_tags": 100,
                "max_subscriptions": 20
            }
        }"#;

        let info: RelayInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, Some("Test Relay".to_string()));
        assert_eq!(info.limitation.max_event_tags, Some(100));
        assert_eq!(info.max_event_tags(), 100);
    }

    #[test]
    fn test_default_max_event_tags() {
        let info = RelayInfo::default();
        assert_eq!(info.max_event_tags(), DEFAULT_MAX_EVENT_TAGS);
    }
}
