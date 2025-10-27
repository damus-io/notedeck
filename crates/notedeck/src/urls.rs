use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use mime_guess::Mime;
use poll_promise::Promise;
use serde::{Deserialize, Serialize};
use tracing::trace;
use url::Url;

use crate::{Error, MediaCacheType};

const FILE_NAME: &str = "urls.bin";
const SAVE_INTERVAL: Duration = Duration::from_secs(60);
const MIME_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 7); // one week
const FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(4);
const FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(60 * 60 * 6);
const FAILURE_BACKOFF_EXPONENT_LIMIT: u32 = 10;

type UrlsToMime = HashMap<String, StoredMimeEntry>;

#[derive(Clone, Serialize, Deserialize)]
struct StoredMimeEntry {
    entry: MimeEntry,
    last_updated_secs: u64,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
enum MimeEntry {
    Mime(String),
    Fail { count: u32 },
}

impl StoredMimeEntry {
    fn new_mime(mime: String, last_updated: SystemTime) -> Self {
        Self {
            entry: MimeEntry::Mime(mime),
            last_updated_secs: system_time_to_secs(last_updated),
        }
    }

    fn new_failure(count: u32, last_updated: SystemTime) -> Self {
        Self {
            entry: MimeEntry::Fail { count },
            last_updated_secs: system_time_to_secs(last_updated),
        }
    }

    fn last_updated(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.last_updated_secs)
    }

    fn expires_at(&self) -> SystemTime {
        let ttl = match &self.entry {
            MimeEntry::Mime(_) => MIME_TTL,
            MimeEntry::Fail { count } => failure_backoff_duration(*count),
        };

        self.last_updated()
            .checked_add(ttl)
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    fn is_expired(&self, now: SystemTime) -> bool {
        self.expires_at() <= now
    }

    fn failure_count(&self) -> Option<u32> {
        match &self.entry {
            MimeEntry::Fail { count } => Some(*count),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct CachedMime {
    mime: Option<Mime>,
    expires_at: SystemTime,
}

/// caches mime type for a URL. saves to disk on interval [`SAVE_INTERVAL`]
pub struct UrlCache {
    last_saved: SystemTime,
    last_pruned: SystemTime,
    path: PathBuf,
    cache: Arc<RwLock<UrlsToMime>>,
    from_disk_promise: Option<Promise<Option<UrlsToMime>>>,
}

impl UrlCache {
    pub fn rel_dir() -> &'static str {
        FILE_NAME
    }

    pub fn new(path: PathBuf) -> Self {
        Self {
            last_saved: SystemTime::now(),
            last_pruned: SystemTime::now(),
            path: path.clone(),
            cache: Default::default(),
            from_disk_promise: Some(read_from_disk(path)),
        }
    }

    fn get_entry(&self, url: &str) -> Option<StoredMimeEntry> {
        self.cache.read().ok()?.get(url).cloned()
    }

    fn set_entry(&mut self, url: String, entry: StoredMimeEntry) {
        if url.is_empty() {
            return;
        }
        if let Ok(mut locked_cache) = self.cache.write() {
            locked_cache.insert(url, entry);
        }
    }

    fn remove(&mut self, url: &str) {
        if let Ok(mut locked_cache) = self.cache.write() {
            locked_cache.remove(url);
        }
    }

    pub fn handle_io(&mut self) {
        if let Some(promise) = &mut self.from_disk_promise {
            if let Some(maybe_cache) = promise.ready_mut() {
                if let Some(cache) = maybe_cache.take() {
                    merge_cache(self.cache.clone(), cache)
                }

                self.from_disk_promise = None;
            }
        }

        if let Ok(cur_duration) = SystemTime::now().duration_since(self.last_saved) {
            if cur_duration >= SAVE_INTERVAL {
                save_to_disk(self.path.clone(), self.cache.clone());
                self.last_saved = SystemTime::now();
            }
        }

        if let Ok(cur_duration) = SystemTime::now().duration_since(self.last_pruned) {
            if cur_duration >= SAVE_INTERVAL {
                self.purge_expired(SystemTime::now());
                self.last_pruned = SystemTime::now();
            }
        }
    }

    pub fn clear(&mut self) {
        if self.from_disk_promise.is_none() {
            let cache = self.cache.clone();
            std::thread::spawn(move || {
                if let Ok(mut locked_cache) = cache.write() {
                    locked_cache.clear();
                }
            });
        }
    }

    fn purge_expired(&self, now: SystemTime) {
        let cache = self.cache.clone();
        std::thread::spawn(move || {
            if let Ok(mut locked_cache) = cache.write() {
                locked_cache.retain(|_, entry| !entry.is_expired(now));
            }
        });
    }
}

fn merge_cache(cur_cache: Arc<RwLock<UrlsToMime>>, mut from_disk: UrlsToMime) {
    std::thread::spawn(move || {
        let now = SystemTime::now();
        from_disk.retain(|_, entry| !entry.is_expired(now));

        if let Ok(mut locked_cache) = cur_cache.write() {
            locked_cache.extend(from_disk);
        }
    });
}

fn read_from_disk(path: PathBuf) -> Promise<Option<UrlsToMime>> {
    let (sender, promise) = Promise::new();

    std::thread::spawn(move || {
        let result: Result<UrlsToMime, Error> = (|| {
            let mut file = File::open(path)?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)?;
            if buffer.is_empty() {
                return Ok(Default::default());
            }

            match bincode::deserialize::<UrlsToMime>(&buffer) {
                Ok(data) => {
                    trace!("Got {} mime entries", data.len());
                    Ok(data)
                }
                Err(err) => {
                    tracing::debug!("Unable to deserialize UrlMimes with new format: {err}. Attempting legacy fallback.");
                    let legacy: HashMap<String, String> =
                        bincode::deserialize(&buffer).map_err(|e| Error::Generic(e.to_string()))?;
                    trace!("legacy fallback has {} entries", legacy.len());
                    let now = SystemTime::now();
                    let migrated = legacy
                        .into_iter()
                        .map(|(url, mime)| (url, StoredMimeEntry::new_mime(mime, now)))
                        .collect();
                    Ok(migrated)
                }
            }
        })();

        match result {
            Ok(data) => sender.send(Some(data)),
            Err(e) => {
                tracing::error!("problem deserializing UrlMimes: {e}");
                sender.send(None)
            }
        }
    });

    promise
}

fn save_to_disk(path: PathBuf, cache: Arc<RwLock<UrlsToMime>>) {
    std::thread::spawn(move || {
        let result: Result<(), Error> = (|| {
            if let Ok(cache) = cache.read() {
                let cache = &*cache;
                let num_items = cache.len();
                let encoded =
                    bincode::serialize(cache).map_err(|e| Error::Generic(e.to_string()))?;
                let mut file = File::create(&path)?;
                file.write_all(&encoded)?;
                file.sync_all()?;
                tracing::debug!("Saved UrlCache with {num_items} mimes to disk.");
                Ok(())
            } else {
                Err(Error::Generic(
                    "Could not read UrlMimes behind RwLock".to_owned(),
                ))
            }
        })();

        if let Err(e) = result {
            tracing::error!("Failed to save UrlMimes: {}", e);
        }
    });
}

fn system_time_to_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn failure_backoff_duration(count: u32) -> Duration {
    if count == 0 {
        return FAILURE_BACKOFF_BASE;
    }

    let exponent = count.saturating_sub(1).min(FAILURE_BACKOFF_EXPONENT_LIMIT);
    let base_secs = FAILURE_BACKOFF_BASE.as_secs().max(1);
    let multiplier = 1u64 << exponent;
    let delay_secs = base_secs.saturating_mul(multiplier);
    let max_secs = FAILURE_BACKOFF_MAX.as_secs();

    Duration::from_secs(delay_secs.min(max_secs))
}

fn ehttp_get_mime_type(url: &str, sender: poll_promise::Sender<MimeResult>) {
    let request = ehttp::Request::head(url);

    let url = url.to_owned();
    ehttp::fetch(
        request,
        move |response: Result<ehttp::Response, String>| match response {
            Ok(resp) => {
                if let Some(content_type) = resp.headers.get("content-type") {
                    sender.send(MimeResult::Ok(extract_mime_type(content_type).to_owned()));
                } else {
                    sender.send(MimeResult::Err(HttpError::MissingHeader));
                    tracing::error!("Content-Type header not found for {url}");
                }
            }
            Err(err) => {
                sender.send(MimeResult::Err(HttpError::HttpFailure));
                tracing::error!("failed ehttp for UrlMimes: {err}");
            }
        },
    );
}

#[derive(Debug)]
enum HttpError {
    HttpFailure,
    MissingHeader,
}

type MimeResult = Result<String, HttpError>;

fn extract_mime_type(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
}

pub struct UrlMimes {
    pub cache: UrlCache,
    in_flight: HashMap<String, Promise<MimeResult>>,
    mime_cache: HashMap<String, CachedMime>,
}

impl UrlMimes {
    pub fn new(url_cache: UrlCache) -> Self {
        Self {
            cache: url_cache,
            in_flight: Default::default(),
            mime_cache: Default::default(),
        }
    }

    pub fn get_or_fetch(&mut self, url: &str) -> Option<&Mime> {
        let now = SystemTime::now();

        if let Some(cached) = self.mime_cache.get(url) {
            if cached.expires_at > now {
                return self
                    .mime_cache
                    .get(url)
                    .and_then(|cached| cached.mime.as_ref());
            }

            tracing::trace!("mime {:?} at url {url} has expired", cached.mime);

            self.mime_cache.remove(url);
        }

        let stored_entry = self.cache.get_entry(url);
        let previous_failure_count = stored_entry
            .as_ref()
            .and_then(|entry| entry.failure_count())
            .unwrap_or(0);

        if let Some(entry) = stored_entry.as_ref() {
            if !entry.is_expired(now) {
                return match &entry.entry {
                    MimeEntry::Mime(mime_string) => match mime_string.parse::<Mime>() {
                        Ok(mime) => {
                            let expires_at = entry.expires_at();
                            trace!("inserted {mime:?} in mime cache for {url}");
                            self.mime_cache.insert(
                                url.to_owned(),
                                CachedMime {
                                    mime: Some(mime),
                                    expires_at,
                                },
                            );
                            self.mime_cache
                                .get(url)
                                .and_then(|cached| cached.mime.as_ref())
                        }
                        Err(err) => {
                            tracing::warn!("Failed to parse mime '{mime_string}' for {url}: {err}");
                            self.record_failure(
                                url,
                                previous_failure_count.saturating_add(1),
                                SystemTime::now(),
                            );
                            None
                        }
                    },
                    MimeEntry::Fail { .. } => {
                        trace!("Read failure from storage for {url}, wrote None to cache");

                        let expires_at = entry.expires_at();
                        self.mime_cache.insert(
                            url.to_owned(),
                            CachedMime {
                                mime: None,
                                expires_at,
                            },
                        );
                        None
                    }
                };
            }

            if !matches!(entry.entry, MimeEntry::Fail { count: _ }) {
                self.cache.remove(url);
            }
        }

        let Some(promise) = self.in_flight.get_mut(url) else {
            if Url::parse(url).is_err() {
                trace!("Found invalid url: {url}");
                self.mime_cache.insert(
                    url.to_owned(),
                    CachedMime {
                        mime: None,
                        expires_at: SystemTime::UNIX_EPOCH + Duration::from_secs(u64::MAX / 2), // never expire...
                    },
                );
            }
            let (sender, promise) = Promise::new();
            ehttp_get_mime_type(url, sender);
            self.in_flight.insert(url.to_owned(), promise);
            return None;
        };

        let Ok(mime_type) = promise.ready_mut()? else {
            self.in_flight.remove(url);
            self.record_failure(
                url,
                previous_failure_count.saturating_add(1),
                SystemTime::now(),
            );
            return None;
        };

        let mime_string = std::mem::take(mime_type);
        self.in_flight.remove(url);

        match mime_string.parse::<Mime>() {
            Ok(mime) => {
                let fetched_at = SystemTime::now();
                let prev_entry = stored_entry;
                let entry = StoredMimeEntry::new_mime(mime_string, fetched_at);
                let expires_at = entry.expires_at();
                if let Some(Some(failed_count)) = prev_entry.map(|p| {
                    if let MimeEntry::Fail { count } = p.entry {
                        Some(count)
                    } else {
                        None
                    }
                }) {
                    trace!("found {mime:?} for {url}, inserting in cache & storage AFTER FAILING {failed_count} TIMES");
                } else {
                    trace!("found {mime:?} for {url}, inserting in cache & storage");
                }
                self.cache.set_entry(url.to_owned(), entry);
                self.mime_cache.insert(
                    url.to_owned(),
                    CachedMime {
                        mime: Some(mime),
                        expires_at,
                    },
                );
                self.mime_cache
                    .get(url)
                    .and_then(|cached| cached.mime.as_ref())
            }
            Err(err) => {
                tracing::warn!("Unable to parse mime type returned for {url}: {err}");
                self.record_failure(
                    url,
                    previous_failure_count.saturating_add(1),
                    SystemTime::now(),
                );
                None
            }
        }
    }

    fn record_failure(&mut self, url: &str, count: u32, timestamp: SystemTime) {
        let count = count.max(1);
        let entry = StoredMimeEntry::new_failure(count, timestamp);
        let expires_at = entry.expires_at();
        trace!(
            "failed to get mime for {url} {count} times. next request in {:?}",
            failure_backoff_duration(count)
        );
        self.cache.set_entry(url.to_owned(), entry);
        self.mime_cache.insert(
            url.to_owned(),
            CachedMime {
                mime: None,
                expires_at,
            },
        );
    }
}

#[derive(Debug)]
pub struct SupportedMimeType {
    mime: mime_guess::Mime,
}

impl SupportedMimeType {
    #[profiling::function]
    pub fn from_extension(extension: &str) -> Result<Self, Error> {
        if let Some(mime) = mime_guess::from_ext(extension)
            .first()
            .filter(is_mime_supported)
        {
            Ok(Self { mime })
        } else {
            Err(Error::Generic(
                format!("{extension} Unsupported mime type",),
            ))
        }
    }

    pub fn from_mime(mime: mime_guess::mime::Mime) -> Result<Self, Error> {
        if is_mime_supported(&mime) {
            Ok(Self { mime })
        } else {
            Err(Error::Generic("Unsupported mime type".to_owned()))
        }
    }

    pub fn to_mime(&self) -> &str {
        self.mime.essence_str()
    }

    pub fn to_cache_type(&self) -> MediaCacheType {
        mime_to_cache_type(&self.mime)
    }
}

fn mime_to_cache_type(mime: &Mime) -> MediaCacheType {
    if *mime == mime_guess::mime::IMAGE_GIF {
        MediaCacheType::Gif
    } else {
        MediaCacheType::Image
    }
}

fn is_mime_supported(mime: &mime_guess::Mime) -> bool {
    mime.type_() == mime_guess::mime::IMAGE
}

#[profiling::function]
fn url_has_supported_mime(url: &str) -> MimeHostedAtUrl {
    let url = {
        profiling::scope!("url parse");
        Url::parse(url)
    };
    if let Ok(url) = url {
        if let Some(mut path) = url.path_segments() {
            if let Some(file_name) = path.next_back() {
                if let Some(ext) = std::path::Path::new(file_name)
                    .extension()
                    .and_then(|ext| ext.to_str())
                {
                    if let Ok(supported) = SupportedMimeType::from_extension(ext) {
                        return MimeHostedAtUrl::Yes(supported.to_cache_type());
                    } else {
                        return MimeHostedAtUrl::No;
                    }
                }
            }
        }
    }
    MimeHostedAtUrl::Maybe
}

#[profiling::function]
pub fn supported_mime_hosted_at_url(urls: &mut UrlMimes, url: &str) -> Option<MediaCacheType> {
    let Some(mime) = urls.get_or_fetch(url) else {
        return match url_has_supported_mime(url) {
            MimeHostedAtUrl::Yes(media_cache_type) => Some(media_cache_type),
            MimeHostedAtUrl::Maybe | MimeHostedAtUrl::No => None,
        };
    };

    Some(mime)
        .filter(|mime| is_mime_supported(mime))
        .map(mime_to_cache_type)
}

enum MimeHostedAtUrl {
    Yes(MediaCacheType),
    Maybe,
    No,
}
