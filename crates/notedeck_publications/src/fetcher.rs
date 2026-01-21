//! Publication fetcher for relay subscriptions
//!
//! Handles fetching publication events from Nostr relays and populating
//! the publication tree.

use nostrdb::Filter;
use tracing::{debug, info};

use crate::address::EventAddress;
use crate::constants::KIND_PUBLICATION_INDEX;
use crate::tree::PublicationTree;

/// State of a fetch operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchState {
    /// Not started
    Idle,
    /// Subscription sent, waiting for events
    Fetching,
    /// Received EOSE (end of stored events)
    Complete,
    /// Error occurred
    Error,
}

/// Fetcher for publication events
#[derive(Debug)]
pub struct PublicationFetcher {
    /// Current fetch state
    state: FetchState,

    /// Subscription ID (if active)
    subscription_id: Option<String>,

    /// Number of events received
    events_received: usize,

    /// Number of batches fetched
    batches_completed: usize,
}

impl Default for PublicationFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PublicationFetcher {
    /// Create a new fetcher
    pub fn new() -> Self {
        Self {
            state: FetchState::Idle,
            subscription_id: None,
            events_received: 0,
            batches_completed: 0,
        }
    }

    /// Get current state
    pub fn state(&self) -> FetchState {
        self.state
    }

    /// Get subscription ID if active
    pub fn subscription_id(&self) -> Option<&str> {
        self.subscription_id.as_deref()
    }

    /// Check if currently fetching
    pub fn is_fetching(&self) -> bool {
        self.state == FetchState::Fetching
    }

    /// Get number of events received
    pub fn events_received(&self) -> usize {
        self.events_received
    }

    /// Build filters for fetching events by addresses
    ///
    /// Groups addresses by (kind, pubkey) to minimize filter count.
    pub fn build_filters(addresses: &[&EventAddress]) -> Vec<nostrdb::Filter> {
        use std::collections::HashMap;

        // Group by (kind, pubkey) for efficient filtering
        let mut grouped: HashMap<(u32, [u8; 32]), Vec<String>> = HashMap::new();

        for addr in addresses {
            grouped
                .entry((addr.kind, addr.pubkey))
                .or_default()
                .push(addr.dtag.clone());
        }

        grouped
            .into_iter()
            .map(|((kind, pubkey), dtags)| {
                let dtag_refs: Vec<&str> = dtags.iter().map(|s| s.as_str()).collect();
                Filter::new()
                    .kinds([kind as u64])
                    .authors([&pubkey])
                    .tags(dtag_refs, 'd')
                    .build()
            })
            .collect()
    }

    /// Build a filter for fetching a root publication by d-tag
    pub fn build_root_filter(kind: u32, pubkey: &[u8; 32], dtag: &str) -> nostrdb::Filter {
        Filter::new()
            .kinds([kind as u64])
            .authors([pubkey])
            .tags([dtag], 'd')
            .build()
    }

    /// Build a filter for discovering publications by a pubkey
    pub fn build_discovery_filter(pubkey: &[u8; 32], limit: u64) -> nostrdb::Filter {
        Filter::new()
            .kinds([KIND_PUBLICATION_INDEX as u64])
            .authors([pubkey])
            .limit(limit)
            .build()
    }

    /// Build a filter for searching publications by title/topic
    pub fn build_search_filter(_search_term: &str, limit: u64) -> nostrdb::Filter {
        // Note: Full-text search depends on relay support (NIP-50)
        // This returns recent publications; actual search may need
        // to be implemented client-side or with specific relays
        Filter::new()
            .kinds([KIND_PUBLICATION_INDEX as u64])
            .limit(limit)
            .build()
    }

    /// Generate a unique subscription ID
    pub fn generate_subscription_id(prefix: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("{}-{}", prefix, timestamp)
    }

    /// Mark as fetching with subscription ID
    pub fn start_fetch(&mut self, subscription_id: String) {
        self.state = FetchState::Fetching;
        self.subscription_id = Some(subscription_id);
        debug!(
            "Started fetch with subscription: {:?}",
            self.subscription_id
        );
    }

    /// Record that an event was received
    pub fn record_event(&mut self) {
        self.events_received += 1;
    }

    /// Mark fetch as complete (EOSE received)
    pub fn mark_complete(&mut self) {
        self.state = FetchState::Complete;
        self.batches_completed += 1;
        info!(
            "Fetch complete: {} events received in batch {}",
            self.events_received, self.batches_completed
        );
    }

    /// Mark as error
    pub fn mark_error(&mut self) {
        self.state = FetchState::Error;
    }

    /// Reset for a new fetch operation
    pub fn reset(&mut self) {
        self.state = FetchState::Idle;
        self.subscription_id = None;
        self.events_received = 0;
    }

    /// Get addresses of pending nodes from tree, limited to batch size
    pub fn get_pending_batch<'a>(
        tree: &'a PublicationTree,
        batch_size: usize,
    ) -> Vec<&'a EventAddress> {
        tree.pending_addresses()
            .into_iter()
            .take(batch_size)
            .collect()
    }
}

/// Builder for fetching a specific publication
#[derive(Debug)]
pub struct PublicationRequest {
    /// The root address to fetch
    pub root_address: EventAddress,

    /// Maximum depth to fetch (0 = root only, 1 = root + immediate children, etc.)
    pub max_depth: Option<usize>,

    /// Maximum number of events to fetch per batch
    pub batch_size: usize,
}

impl PublicationRequest {
    /// Create a new request for a publication
    pub fn new(root_address: EventAddress) -> Self {
        Self {
            root_address,
            max_depth: None,
            batch_size: 25,
        }
    }

    /// Set maximum depth
    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Set batch size
    pub fn batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    /// Build filter for the root event
    pub fn root_filter(&self) -> Filter {
        PublicationFetcher::build_root_filter(
            self.root_address.kind,
            &self.root_address.pubkey,
            &self.root_address.dtag,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_filters() {
        let addr1 = EventAddress::new(30041, [0xaa; 32], "chapter-1".to_string());
        let addr2 = EventAddress::new(30041, [0xaa; 32], "chapter-2".to_string());
        let addr3 = EventAddress::new(30040, [0xbb; 32], "nested-index".to_string());

        let addresses = vec![&addr1, &addr2, &addr3];
        let filters = PublicationFetcher::build_filters(&addresses);

        // Should produce 2 filters: one for (30041, aa), one for (30040, bb)
        assert_eq!(filters.len(), 2);
    }

    #[test]
    fn test_subscription_id_generation() {
        let id1 = PublicationFetcher::generate_subscription_id("pub-tree");

        assert!(id1.starts_with("pub-tree-"));
        // Verify format: prefix-timestamp
        let parts: Vec<&str> = id1.split('-').collect();
        assert!(parts.len() >= 2);
        // Last part should be a number (timestamp)
        let timestamp_part = parts.last().unwrap();
        assert!(timestamp_part.parse::<u128>().is_ok());
    }

    #[test]
    fn test_fetch_state_transitions() {
        let mut fetcher = PublicationFetcher::new();
        assert_eq!(fetcher.state(), FetchState::Idle);

        fetcher.start_fetch("test-sub".to_string());
        assert_eq!(fetcher.state(), FetchState::Fetching);
        assert!(fetcher.is_fetching());

        fetcher.record_event();
        fetcher.record_event();
        assert_eq!(fetcher.events_received(), 2);

        fetcher.mark_complete();
        assert_eq!(fetcher.state(), FetchState::Complete);
        assert!(!fetcher.is_fetching());
    }

    #[test]
    fn test_publication_request() {
        let addr = EventAddress::new(30040, [0xaa; 32], "my-book".to_string());
        let request = PublicationRequest::new(addr).max_depth(2).batch_size(50);

        assert_eq!(request.max_depth, Some(2));
        assert_eq!(request.batch_size, 50);
        assert_eq!(request.root_address.kind, 30040);
        assert_eq!(request.root_address.dtag, "my-book");

        // Verify filter can be built (doesn't panic)
        let _filter = request.root_filter();
    }
}
