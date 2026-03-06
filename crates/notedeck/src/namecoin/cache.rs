/// LRU cache with TTL for Namecoin name lookups.
///
/// Caches resolved pubkeys to avoid repeated ElectrumX queries.
/// Entries expire after 1 hour.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use enostr::Pubkey;

use super::NamecoinResolveError;

const CACHE_TTL: Duration = Duration::from_secs(3600); // 1 hour
const MAX_CACHE_SIZE: usize = 256;

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub pubkey: Option<Pubkey>,
    pub error: Option<NamecoinResolveError>,
    pub inserted_at: Instant,
}

impl CacheEntry {
    pub fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() >= CACHE_TTL
    }
}

pub struct NamecoinLookupCache {
    entries: HashMap<String, CacheEntry>,
    /// Insertion order for LRU eviction
    insertion_order: Vec<String>,
}

impl Default for NamecoinLookupCache {
    fn default() -> Self {
        Self::new()
    }
}

impl NamecoinLookupCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: Vec::new(),
        }
    }

    /// Get a cached entry if it exists and hasn't expired.
    pub fn get(&self, key: &str) -> Option<&CacheEntry> {
        self.entries.get(key).filter(|entry| !entry.is_expired())
    }

    /// Insert or update a cache entry.
    pub fn insert(&mut self, key: String, result: Result<Pubkey, NamecoinResolveError>) {
        // Evict if at capacity
        while self.entries.len() >= MAX_CACHE_SIZE && !self.insertion_order.is_empty() {
            let oldest = self.insertion_order.remove(0);
            self.entries.remove(&oldest);
        }

        // Remove old entry from insertion order if updating
        self.insertion_order.retain(|k| k != &key);

        let (pubkey, error) = match result {
            Ok(pk) => (Some(pk), None),
            Err(e) => (None, Some(e)),
        };

        self.entries.insert(
            key.clone(),
            CacheEntry {
                pubkey,
                error,
                inserted_at: Instant::now(),
            },
        );
        self.insertion_order.push(key);
    }

    /// Remove expired entries.
    pub fn evict_expired(&mut self) {
        self.entries.retain(|_, entry| !entry.is_expired());
        self.insertion_order
            .retain(|key| self.entries.contains_key(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = NamecoinLookupCache::new();
        let pk = Pubkey::new([1u8; 32]);

        cache.insert("d/test".to_string(), Ok(pk));

        let entry = cache.get("d/test");
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().pubkey, Some(pk));
    }

    #[test]
    fn test_cache_miss() {
        let cache = NamecoinLookupCache::new();
        assert!(cache.get("d/nonexistent").is_none());
    }

    #[test]
    fn test_cache_negative_result() {
        let mut cache = NamecoinLookupCache::new();
        cache.insert("d/expired".to_string(), Err(NamecoinResolveError::NameNotFound));

        let entry = cache.get("d/expired");
        assert!(entry.is_some());
        assert!(entry.unwrap().pubkey.is_none());
    }
}
