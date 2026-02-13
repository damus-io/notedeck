//! Image caching for notification profile pictures.
//!
//! Downloads remote images and caches them locally for use with native
//! notification APIs that require local file URLs.
//!
//! # Platform Requirements
//!
//! - **macOS**: `UNNotificationAttachment` only accepts local file URLs.
//!   Remote images cannot be loaded directly (OS-level restriction).
//! - **Linux**: `notify-rust` supports images directly, but caching still
//!   improves performance and offline reliability.
//!
//! # Cache Location
//!
//! Images are cached to:
//! - macOS/Linux: `~/.cache/notedeck/notification_avatars/`
//! - Fallback: `{app_data}/notedeck/notification_avatars/`
//!
//! # Usage
//!
//! ```ignore
//! use notedeck::notifications::image_cache::NotificationImageCache;
//!
//! // new() returns Option - handle initialization failure
//! let cache = NotificationImageCache::new()?;
//!
//! // Synchronous check (returns cached path if available)
//! if let Some(path) = cache.get_cached_path(url) {
//!     // Use path for notification
//! }
//!
//! // Blocking download and cache (for non-async contexts)
//! // Note: Do not call from within an async runtime - it will panic
//! if let Some(path) = cache.fetch_and_cache_blocking(url) {
//!     // Use path for notification
//! }
//! ```

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Maximum size for cached images (5MB).
const MAX_IMAGE_SIZE: usize = 5 * 1024 * 1024;

/// Maximum age for cached images before refresh (7 days).
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Maximum number of entries in the in-memory URL -> path cache.
const MAX_MEMORY_CACHE_ENTRIES: usize = 500;

/// Image cache for notification profile pictures.
///
/// Manages downloading, caching, and retrieving profile images for use
/// in native notification APIs. Thread-safe via internal locking.
pub struct NotificationImageCache {
    /// Directory where cached images are stored.
    cache_dir: PathBuf,
    /// In-memory cache of URL hash -> cached file path.
    /// Avoids repeated filesystem checks for recently accessed images.
    memory_cache: Arc<RwLock<std::collections::HashMap<String, PathBuf>>>,
    /// Reusable Tokio runtime for blocking downloads.
    /// Stored to avoid creating a new runtime for each image fetch.
    runtime: tokio::runtime::Runtime,
}

impl NotificationImageCache {
    /// Create a new notification image cache.
    ///
    /// Initializes the cache directory if it doesn't exist.
    /// Returns `None` if the cache directory cannot be determined or created.
    pub fn new() -> Option<Self> {
        let cache_dir = Self::get_cache_dir()?;

        // Create cache directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            error!("Failed to create notification image cache directory: {}", e);
            return None;
        }

        // Create a reusable runtime for blocking downloads
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                error!("Failed to create tokio runtime for image cache: {}", e);
                return None;
            }
        };

        info!("Notification image cache initialized at {:?}", cache_dir);

        Some(Self {
            cache_dir,
            memory_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            runtime,
        })
    }

    /// Get the cache directory path.
    ///
    /// Prefers `~/.cache/notedeck/notification_avatars/` on Unix systems,
    /// falls back to app data directory.
    fn get_cache_dir() -> Option<PathBuf> {
        // Try XDG cache directory first (Linux/macOS)
        if let Some(cache_dir) = dirs::cache_dir() {
            let mut path = cache_dir;
            path.push("notedeck");
            path.push("notification_avatars");
            return Some(path);
        }

        // Fallback to data directory
        if let Some(data_dir) = dirs::data_local_dir() {
            let mut path = data_dir;
            path.push("notedeck");
            path.push("notification_avatars");
            return Some(path);
        }

        None
    }

    /// Generate a cache filename from a URL.
    ///
    /// Uses SHA-256 hash of the URL to create a unique, filesystem-safe filename.
    /// Extension is derived from URL path or defaults to png.
    fn url_to_filename(url: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        let hash = hasher.finalize();

        // Determine extension from URL or default to png
        let ext = Self::extension_from_url(url).unwrap_or("png");

        format!("{:x}.{}", hash, ext)
    }

    /// Extract file extension from URL path.
    ///
    /// Recognizes common image formats that macOS UNNotificationAttachment supports.
    fn extension_from_url(url: &str) -> Option<&'static str> {
        let path = url.split('?').next()?; // Remove query params
        let ext = path.rsplit('.').next()?.to_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some("jpg"),
            "png" => Some("png"),
            "gif" => Some("gif"),
            "webp" => Some("webp"),
            "heic" => Some("heic"),
            "avif" => Some("avif"),
            _ => None,
        }
    }

    /// Get the full cache path for a URL.
    fn get_cache_path(&self, url: &str) -> PathBuf {
        let filename = Self::url_to_filename(url);
        self.cache_dir.join(filename)
    }

    /// Check if a cached image exists and is not expired.
    ///
    /// Returns the path if valid, `None` if missing or expired.
    pub fn get_cached_path(&self, url: &str) -> Option<PathBuf> {
        let path = self.get_cache_path(url);

        if !path.exists() {
            return None;
        }

        // Check if cache entry is too old
        if let Ok(metadata) = std::fs::metadata(&path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(age) = SystemTime::now().duration_since(modified) {
                    if age > CACHE_MAX_AGE {
                        debug!("Cached image expired: {:?}", path);
                        // Don't delete here - let fetch_and_cache refresh it
                        return None;
                    }
                }
            }
        }

        Some(path)
    }

    /// Check memory cache for a URL.
    ///
    /// This is a fast path that avoids filesystem access for recently used images.
    pub async fn get_from_memory_cache(&self, url: &str) -> Option<PathBuf> {
        let hash = Self::url_to_filename(url);
        let cache = self.memory_cache.read().await;
        cache.get(&hash).cloned()
    }

    /// Fetch an image from URL and cache it locally.
    ///
    /// Returns the local file path on success, `None` on failure.
    ///
    /// # Arguments
    /// * `url` - The remote image URL to fetch
    ///
    /// # Returns
    /// * `Some(PathBuf)` - Path to the cached local file
    /// * `None` - If download or caching failed
    #[profiling::function]
    pub async fn fetch_and_cache(&self, url: &str) -> Option<PathBuf> {
        // Quick check: already cached?
        if let Some(path) = self.get_cached_path(url) {
            // Update memory cache
            let hash = Self::url_to_filename(url);
            let mut cache = self.memory_cache.write().await;
            cache.insert(hash, path.clone());
            self.prune_memory_cache_if_needed(&mut cache);
            return Some(path);
        }

        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            warn!("Invalid image URL (not HTTP/HTTPS): {}", url);
            return None;
        }

        debug!("Fetching notification image: {}", url);

        // Download the image
        let response = match crate::media::network::http_req(url).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to download notification image: {}", e);
                return None;
            }
        };

        // Validate content type
        let content_type = response.content_type.as_deref().unwrap_or("");
        if !content_type.starts_with("image/") {
            warn!(
                "Notification image URL returned non-image content type: {}",
                content_type
            );
            return None;
        }

        // Check size
        if response.bytes.len() > MAX_IMAGE_SIZE {
            warn!(
                "Notification image too large: {} bytes (max {})",
                response.bytes.len(),
                MAX_IMAGE_SIZE
            );
            return None;
        }

        // Write to cache
        let cache_path = self.get_cache_path(url);
        if let Err(e) = std::fs::write(&cache_path, &response.bytes) {
            error!("Failed to write cached notification image: {}", e);
            return None;
        }

        debug!("Cached notification image: {:?}", cache_path);

        // Update memory cache
        let hash = Self::url_to_filename(url);
        let mut cache = self.memory_cache.write().await;
        cache.insert(hash, cache_path.clone());
        self.prune_memory_cache_if_needed(&mut cache);

        Some(cache_path)
    }

    /// Prune memory cache if it exceeds the maximum size.
    ///
    /// Uses a simple strategy: clear half the entries when limit is reached.
    fn prune_memory_cache_if_needed(&self, cache: &mut std::collections::HashMap<String, PathBuf>) {
        if cache.len() > MAX_MEMORY_CACHE_ENTRIES {
            // Simple pruning: remove oldest half
            let to_remove: Vec<String> = cache.keys().take(cache.len() / 2).cloned().collect();
            for key in to_remove {
                cache.remove(&key);
            }
            debug!(
                "Pruned notification image memory cache to {} entries",
                cache.len()
            );
        }
    }

    /// Clean up old cached files and synchronize the in-memory cache.
    ///
    /// Removes files older than `CACHE_MAX_AGE` from disk, then removes
    /// any stale entries from the in-memory cache that reference deleted files.
    pub fn cleanup_old_entries(&self) -> std::io::Result<usize> {
        let mut removed = 0;
        let mut removed_paths = Vec::new();
        let now = SystemTime::now();

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            if let Ok(metadata) = entry.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age > CACHE_MAX_AGE && std::fs::remove_file(&path).is_ok() {
                            removed_paths.push(path);
                            removed += 1;
                        }
                    }
                }
            }
        }

        // Synchronize memory cache: remove entries pointing to deleted files
        if !removed_paths.is_empty() {
            self.runtime.block_on(async {
                let mut cache = self.memory_cache.write().await;
                cache.retain(|_key, cached_path| !removed_paths.contains(cached_path));
            });

            info!(
                "Cleaned up {} old notification image cache entries",
                removed
            );
        }

        Ok(removed)
    }
}

// Note: Default is intentionally not implemented for NotificationImageCache.
// Callers must use new() and handle the Option explicitly since initialization
// can fail (cache directory creation, tokio runtime creation).

impl NotificationImageCache {
    /// Synchronous version of `fetch_and_cache` for use in non-async contexts.
    ///
    /// Uses the stored Tokio runtime to run the async fetch operation.
    ///
    /// # Arguments
    /// * `url` - The remote image URL to fetch
    ///
    /// # Returns
    /// * `Some(PathBuf)` - Path to the cached local file
    /// * `None` - If download or caching failed
    ///
    /// # Panics
    ///
    /// This method uses `runtime.block_on()` internally and **will panic** if
    /// called from within an existing async runtime (e.g., from a tokio task
    /// or async context). Only call this from synchronous code paths such as
    /// worker threads or non-async notification handlers.
    pub fn fetch_and_cache_blocking(&self, url: &str) -> Option<PathBuf> {
        // Quick check: already cached?
        if let Some(path) = self.get_cached_path(url) {
            return Some(path);
        }

        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            warn!("Invalid image URL (not HTTP/HTTPS): {}", url);
            return None;
        }

        debug!("Fetching notification image (blocking): {}", url);

        // Use the stored runtime for async download
        self.runtime.block_on(self.fetch_and_cache(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_to_filename() {
        // PNG URL should produce .png extension
        let url = "https://example.com/avatar.png";
        let filename = NotificationImageCache::url_to_filename(url);
        assert!(filename.ends_with(".png"));
        assert!(filename.len() > 10); // Hash should be substantial

        // Same URL should produce same filename
        let filename2 = NotificationImageCache::url_to_filename(url);
        assert_eq!(filename, filename2);

        // JPG URL should produce .jpg extension
        let filename3 = NotificationImageCache::url_to_filename("https://other.com/pic.jpg");
        assert!(filename3.ends_with(".jpg"));
        assert_ne!(filename, filename3);

        // URL with query params should extract extension correctly
        let filename4 =
            NotificationImageCache::url_to_filename("https://cdn.example.com/img.webp?size=100");
        assert!(filename4.ends_with(".webp"));

        // URL without extension should default to .png
        let filename5 = NotificationImageCache::url_to_filename("https://example.com/avatar");
        assert!(filename5.ends_with(".png"));
    }

    #[test]
    fn test_cache_dir_creation() {
        // This test just verifies the cache can be created
        // without actually downloading anything
        if let Some(cache) = NotificationImageCache::new() {
            assert!(cache.cache_dir.exists());
        }
    }
}
