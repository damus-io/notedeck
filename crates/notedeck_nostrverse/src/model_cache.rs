use std::collections::HashMap;
use std::path::PathBuf;

use poll_promise::Promise;
use sha2::{Digest, Sha256};

/// Status of a model fetch operation.
enum ModelFetchStatus {
    /// HTTP download in progress.
    Downloading(Promise<Result<PathBuf, String>>),
    /// Downloaded to disk, ready for GPU load on next poll.
    ReadyToLoad(PathBuf),
    /// Model handle assigned; terminal state.
    Loaded,
    /// Download or load failed; terminal state.
    Failed,
}

/// Manages async downloading and disk caching of remote 3D models.
///
/// Local file paths are passed through unchanged.
/// HTTP/HTTPS URLs are downloaded via `ehttp`, cached to disk under
/// a sha256-hashed filename, and then loaded from the cache path.
pub struct ModelCache {
    cache_dir: PathBuf,
    fetches: HashMap<String, ModelFetchStatus>,
}

impl ModelCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        Self {
            cache_dir,
            fetches: HashMap::new(),
        }
    }

    /// Returns true if `url` is an HTTP or HTTPS URL.
    fn is_remote(url: &str) -> bool {
        url.starts_with("http://") || url.starts_with("https://")
    }

    /// Compute on-disk cache path: `<cache_dir>/<sha256(url)>.<ext>`.
    fn cache_path(&self, url: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        let ext = std::path::Path::new(url)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("glb");

        self.cache_dir.join(format!("{hash}.{ext}"))
    }

    /// Request a model by URL.
    ///
    /// - Local paths: returns `Some(PathBuf)` immediately.
    /// - Cached remote URLs: returns `Some(PathBuf)` from disk cache.
    /// - Uncached remote URLs: initiates async download, returns `None`.
    ///   The download result will be available via [`poll`] on a later frame.
    pub fn request(&mut self, url: &str) -> Option<PathBuf> {
        if !Self::is_remote(url) {
            return Some(PathBuf::from(url));
        }

        if let Some(status) = self.fetches.get(url) {
            return match status {
                ModelFetchStatus::ReadyToLoad(path) => Some(path.clone()),
                ModelFetchStatus::Loaded
                | ModelFetchStatus::Failed
                | ModelFetchStatus::Downloading(_) => None,
            };
        }

        // Check disk cache
        let cached = self.cache_path(url);
        if cached.exists() {
            tracing::info!("Model cache hit: {}", url);
            self.fetches.insert(
                url.to_owned(),
                ModelFetchStatus::ReadyToLoad(cached.clone()),
            );
            return Some(cached);
        }

        // Start async download
        tracing::info!("Downloading model: {}", url);
        let (sender, promise) = Promise::new();
        let target_path = cached;
        let request = ehttp::Request::get(url);

        let url_owned = url.to_owned();
        ehttp::fetch(request, move |response: Result<ehttp::Response, String>| {
            let result = (|| -> Result<PathBuf, String> {
                let resp = response.map_err(|e| format!("HTTP error: {e}"))?;
                if !resp.ok {
                    return Err(format!("HTTP {}: {}", resp.status, resp.status_text));
                }
                if resp.bytes.is_empty() {
                    return Err("Empty response body".to_string());
                }

                if let Some(parent) = target_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
                }

                // Atomic write: .tmp then rename
                let tmp_path = target_path.with_extension("tmp");
                std::fs::write(&tmp_path, &resp.bytes).map_err(|e| format!("write: {e}"))?;
                std::fs::rename(&tmp_path, &target_path).map_err(|e| format!("rename: {e}"))?;

                tracing::info!("Cached {} bytes for {}", resp.bytes.len(), url_owned);
                Ok(target_path)
            })();
            sender.send(result);
        });

        self.fetches
            .insert(url.to_owned(), ModelFetchStatus::Downloading(promise));
        None
    }

    /// Poll in-flight downloads. Returns URLs whose files are now ready to load.
    pub fn poll(&mut self) -> Vec<(String, PathBuf)> {
        let mut ready = Vec::new();
        let keys: Vec<String> = self.fetches.keys().cloned().collect();

        for url in keys {
            let needs_transition = {
                let status = self.fetches.get_mut(&url).unwrap();
                if let ModelFetchStatus::Downloading(promise) = status {
                    promise.ready().is_some()
                } else {
                    false
                }
            };

            if needs_transition
                && let Some(ModelFetchStatus::Downloading(promise)) = self.fetches.remove(&url)
            {
                match promise.block_and_take() {
                    Ok(path) => {
                        ready.push((url.clone(), path.clone()));
                        self.fetches
                            .insert(url, ModelFetchStatus::ReadyToLoad(path));
                    }
                    Err(e) => {
                        tracing::warn!("Model download failed for {}: {}", url, e);
                        self.fetches.insert(url, ModelFetchStatus::Failed);
                    }
                }
            }
        }

        ready
    }

    /// Mark a URL as fully loaded (model handle assigned).
    pub fn mark_loaded(&mut self, url: &str) {
        self.fetches
            .insert(url.to_owned(), ModelFetchStatus::Loaded);
    }
}
