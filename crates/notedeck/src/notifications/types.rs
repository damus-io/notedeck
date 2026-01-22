//! Core types for the notification system.
//!
//! These types are platform-agnostic and shared between Android and desktop backends.

use enostr::Pubkey;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;

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

/// A notification ready to be displayed.
///
/// Contains all the data needed to show a notification, including
/// pre-resolved profile information. This is sent from the main
/// event loop to the notification worker.
#[derive(Clone, Debug)]
pub struct NotificationData {
    /// The extracted event data
    pub event: ExtractedEvent,
    /// Author's display name (from nostrdb profile lookup)
    pub author_name: Option<String>,
    /// Author's profile picture URL (from nostrdb profile lookup)
    pub author_picture_url: Option<String>,
    /// Which of our accounts this notification is for
    pub target_pubkey_hex: String,
}

/// Thread-local state owned entirely by the worker thread.
///
/// This struct receives notification data from the main event loop
/// via a channel. It no longer maintains its own relay connections
/// or profile cache - that's handled by the main loop using the
/// existing RelayPool and nostrdb.
pub struct WorkerState {
    /// Map of pubkey hex -> account for O(1) lookups when attributing events
    pub accounts: HashMap<String, NotificationAccount>,
    /// Set of processed event IDs for O(1) deduplication lookups
    pub processed_events: HashSet<String>,
    /// Queue tracking insertion order for bounded eviction (oldest at front)
    pub processed_events_order: VecDeque<String>,
    /// Channel receiver for notification data from main loop
    pub event_receiver: mpsc::Receiver<NotificationData>,
    /// Image cache for profile pictures (macOS notifications require local files)
    #[cfg(target_os = "macos")]
    pub image_cache: Option<super::image_cache::NotificationImageCache>,
}

impl WorkerState {
    /// Create new worker state with an event receiver channel.
    ///
    /// The worker receives pre-processed notification data from the main
    /// event loop, which has already done profile lookups via nostrdb.
    pub fn new(
        accounts: HashMap<String, NotificationAccount>,
        event_receiver: mpsc::Receiver<NotificationData>,
    ) -> Self {
        use tracing::info;

        info!("WorkerState created with {} accounts", accounts.len());

        Self {
            accounts,
            processed_events: HashSet::new(),
            processed_events_order: VecDeque::new(),
            event_receiver,
            #[cfg(target_os = "macos")]
            image_cache: super::image_cache::NotificationImageCache::new(),
        }
    }

    /// Create worker state for a single account.
    pub fn for_single_account(
        pubkey: Pubkey,
        event_receiver: mpsc::Receiver<NotificationData>,
    ) -> Self {
        let mut accounts = HashMap::new();
        let account = NotificationAccount::new(pubkey);
        accounts.insert(account.pubkey_hex.clone(), account);
        Self::new(accounts, event_receiver)
    }

    /// Get all account pubkeys as bytes for filter building.
    pub fn account_pubkey_bytes(&self) -> Vec<&[u8; 32]> {
        self.accounts.values().map(|a| a.pubkey.bytes()).collect()
    }
}

/// Event kinds that should trigger notifications.
pub const NOTIFICATION_KINDS: &[i32] = &[
    1,    // text note
    4,    // legacy DM
    6,    // repost
    7,    // reaction
    1059, // gift-wrapped DM
    9735, // zap receipt
];

/// Check if an event kind is notification-relevant.
pub fn is_notification_kind(kind: i32) -> bool {
    NOTIFICATION_KINDS.contains(&kind)
}
