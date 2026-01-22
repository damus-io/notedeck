//! Core types for the notification system.
//!
//! These types are platform-agnostic and shared between Android and desktop backends.

use enostr::Pubkey;
use std::collections::{HashMap, HashSet};

/// Cached profile information for notification display.
#[derive(Clone, Default, Debug)]
pub struct CachedProfile {
    /// Display name (prefers display_name over name)
    pub name: Option<String>,
    /// Profile picture URL
    pub picture_url: Option<String>,
}

/// A notification account with its pubkey and relay configuration.
#[derive(Clone, Debug)]
pub struct NotificationAccount {
    /// The account's pubkey
    pub pubkey: Pubkey,
    /// Hex string of pubkey for fast lookups
    pub pubkey_hex: String,
}

impl NotificationAccount {
    /// Create a new notification account from a pubkey.
    pub fn new(pubkey: Pubkey) -> Self {
        let pubkey_hex = pubkey.hex();
        Self { pubkey, pubkey_hex }
    }
}

/// Structured event data extracted from JSON.
///
/// This contains all the information needed to display a notification,
/// extracted from the raw Nostr event JSON.
#[derive(Clone, Debug)]
pub struct ExtractedEvent {
    /// Event ID (64-char hex)
    pub id: String,
    /// Event kind (1=text, 4=DM, 6=repost, 7=reaction, 1059=gift-wrapped DM, 9735=zap)
    pub kind: i32,
    /// Event author pubkey (64-char hex)
    pub pubkey: String,
    /// Event content (may be encrypted for DMs)
    pub content: String,
    /// Pubkeys from p-tags (for event attribution)
    pub p_tags: Vec<String>,
    /// Zap amount in satoshis (only for kind 9735 zap receipts)
    pub zap_amount_sats: Option<i64>,
    /// Raw event JSON for broadcast compatibility
    pub raw_json: String,
}

/// Thread-local state owned entirely by the worker thread.
///
/// This struct contains all non-Send types and is only accessed from the worker thread.
/// It manages relay connections, event deduplication, and profile caching.
pub struct WorkerState {
    /// Relay pool for WebSocket connections
    pub pool: enostr::RelayPool,
    /// Map of pubkey hex -> account for O(1) lookups when attributing events
    pub accounts: HashMap<String, NotificationAccount>,
    /// Set of processed event IDs for deduplication
    pub processed_events: HashSet<String>,
    /// Cache of pubkey hex -> profile info
    pub profile_cache: HashMap<String, CachedProfile>,
    /// Set of pubkeys for which profile requests are in flight
    pub requested_profiles: HashSet<String>,
    /// Buffer for events received during profile fetch wait loops
    pub pending_events: Vec<String>,
    /// Image cache for profile pictures (macOS notifications require local files)
    #[cfg(target_os = "macos")]
    pub image_cache: Option<super::image_cache::NotificationImageCache>,
}

impl WorkerState {
    /// Create new worker state with multiple accounts.
    pub fn new(accounts: HashMap<String, NotificationAccount>, relay_urls: Vec<String>) -> Self {
        use tracing::{info, warn};

        let mut pool = enostr::RelayPool::new();

        // Use provided relay URLs, or fall back to defaults if empty
        let relays_to_use: Vec<&str> = if relay_urls.is_empty() {
            info!("No relay URLs provided, using defaults");
            DEFAULT_RELAYS.to_vec()
        } else {
            info!("Using {} user-configured relays", relay_urls.len());
            relay_urls.iter().map(|s| s.as_str()).collect()
        };

        for relay_url in relays_to_use {
            if let Err(e) = pool.add_url(relay_url.to_string(), || {}) {
                warn!("Failed to add relay {}: {}", relay_url, e);
            }
        }

        info!("WorkerState created with {} accounts", accounts.len());

        Self {
            pool,
            accounts,
            processed_events: HashSet::new(),
            profile_cache: HashMap::new(),
            requested_profiles: HashSet::new(),
            pending_events: Vec::new(),
            #[cfg(target_os = "macos")]
            image_cache: super::image_cache::NotificationImageCache::new(),
        }
    }

    /// Create worker state for a single account.
    pub fn for_single_account(pubkey: Pubkey, relay_urls: Vec<String>) -> Self {
        let mut accounts = HashMap::new();
        let account = NotificationAccount::new(pubkey);
        accounts.insert(account.pubkey_hex.clone(), account);
        Self::new(accounts, relay_urls)
    }

    /// Get all account pubkeys as bytes for filter building.
    pub fn account_pubkey_bytes(&self) -> Vec<&[u8; 32]> {
        self.accounts.values().map(|a| a.pubkey.bytes()).collect()
    }
}

/// Default relays to connect to if user hasn't configured inbox relays.
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
    "wss://relay.snort.social",
    "wss://offchain.pub",
];

/// Event kinds that should trigger notifications.
pub const NOTIFICATION_KINDS: &[i32] = &[
    1,    // text note
    4,    // legacy DM
    6,    // repost
    7,    // reaction
    1059, // gift-wrapped DM
    9735, // zap receipt
];
