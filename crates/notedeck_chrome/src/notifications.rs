//! Android notification service JNI interface
//!
//! This module provides the Rust side of the Pokey-style push notification
//! system. It manages relay connections and event subscriptions for the
//! Android foreground service.
//!
//! Architecture: Uses a worker thread that owns all non-Send types (RelayPool, etc.)
//! Communication happens via atomic flags and the worker thread handles all relay I/O.
//! Events are ingested into nostrdb and polled via subscription for notification processing.

#[cfg(target_os = "android")]
use jni::objects::{JObject, JString, JValue};
#[cfg(target_os = "android")]
use jni::sys::jint;
#[cfg(target_os = "android")]
use jni::JNIEnv;

use enostr::{Pubkey, RelayPool, RelayStatus};
use nostrdb::{Filter, IngestMetadata, Ndb, Subscription, Transaction};
use notedeck::notifications::{ndb_helpers, safe_prefix, ExtractedEvent, NOTIFICATION_KINDS};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use tracing::{debug, error, info, warn};

#[cfg(target_os = "android")]
use notedeck::notifications::{extract_mentioned_pubkeys, resolve_mentions};

/// Default relays to connect to if user hasn't configured inbox relays
const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
    "wss://relay.snort.social",
    "wss://offchain.pub",
];

/// Subscription IDs
const SUB_NOTIFICATIONS: &str = "notedeck_notifications";
const SUB_DMS: &str = "notedeck_dms";
const SUB_RELAY_LIST: &str = "notedeck_relay_list";

/// Shared state for the notification worker thread.
///
/// Uses `OnceLock` for single initialization and `Arc` for shared ownership.
/// The `Mutex` fields are only accessed during worker start/stop and JNI
/// callbacks — never from the UI thread — so they cannot cause frame stalls.
struct SharedState {
    /// Flag to signal worker thread to stop.
    running: AtomicBool,
    /// Monotonic generation counter used to invalidate old workers on restart.
    generation: AtomicU64,
    /// Current count of connected relays (updated by worker thread).
    connected_count: AtomicI32,
    /// Handle to the worker thread.
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
    /// Nostrdb handle for event ingestion and profile lookups.
    /// Set by `set_ndb()` before the worker is started.
    ndb: RwLock<Option<Ndb>>,
    /// Callback interface for sending events back to Kotlin.
    /// Uses `Mutex<Option<>>` instead of `OnceLock` to allow refreshing
    /// on Android service restart.
    #[cfg(target_os = "android")]
    java_callback: Mutex<Option<JavaCallback>>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            connected_count: AtomicI32::new(0),
            thread_handle: Mutex::new(None),
            ndb: RwLock::new(None),
            #[cfg(target_os = "android")]
            java_callback: Mutex::new(None),
        }
    }
}

/// Global shared state singleton.
///
/// **Why global?** JNI `extern "C"` functions have fixed signatures — they cannot
/// receive custom Rust state as parameters. This is an unavoidable constraint of
/// the Java Native Interface. The state is consolidated into a single `Arc<SharedState>`
/// to minimize global surface area. See also `NOTIFICATION_BRIDGE` in `platform/android.rs`
/// for the same JNI-constrained pattern.
static SHARED_STATE: OnceLock<Arc<SharedState>> = OnceLock::new();

/// Returns the shared state, initializing on first access.
fn get_shared_state() -> Arc<SharedState> {
    SHARED_STATE
        .get_or_init(|| Arc::new(SharedState::default()))
        .clone()
}

#[cfg(target_os = "android")]
struct JavaCallback {
    jvm: jni::JavaVM,
    service_obj: jni::objects::GlobalRef,
}

// SAFETY: `JavaCallback` contains `jni::JavaVM` (thread-safe by JNI spec) and
// `jni::objects::GlobalRef` (valid across threads per JNI spec §5.1.1).
// `Send` is required because the callback is created on the JNI thread and used
// by the worker thread. `Sync` is technically not exercised (access is serialized
// by the `Mutex` around `java_callback`), but is required by the `Mutex<Option<>>`
// container. All JNI calls attach the current thread before use.
#[cfg(target_os = "android")]
unsafe impl Send for JavaCallback {}
#[cfg(target_os = "android")]
unsafe impl Sync for JavaCallback {}

/// Thread-local state owned entirely by the worker thread (contains non-Send types)
struct WorkerState {
    pool: RelayPool,
    /// All monitored account pubkeys (multi-account support)
    pubkeys: Vec<Pubkey>,
    processed_events: HashSet<String>,
    /// Queue tracking insertion order for bounded LRU eviction (oldest at front)
    processed_events_order: std::collections::VecDeque<String>,
    /// Last-seen event timestamp for reconnect resume (avoids missing events)
    last_seen_timestamp: u64,
}

impl WorkerState {
    fn new(pubkeys: Vec<Pubkey>, relay_urls: Vec<String>) -> Self {
        let mut pool = RelayPool::new();

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

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            pool,
            pubkeys,
            processed_events: HashSet::new(),
            processed_events_order: std::collections::VecDeque::new(),
            last_seen_timestamp: now,
        }
    }
}

/// Store a nostrdb handle for the worker thread to use.
///
/// Must be called before `start_subscriptions()` for the worker to have
/// nostrdb access. The Ndb is `Clone` (backed by `Arc`), so this is cheap.
///
/// Called from the main app setup (e.g., `chrome.rs::auto_enable_notifications`).
pub fn set_ndb(ndb: &Ndb) {
    let shared = get_shared_state();
    if let Ok(mut guard) = shared.ndb.write() {
        *guard = Some(ndb.clone());
        info!("Notification worker ndb handle stored");
    };
}

/// Start notification subscriptions for the given pubkeys and relay URLs.
/// If relay_urls is empty, falls back to DEFAULT_RELAYS.
#[profiling::function]
pub fn start_subscriptions(pubkey_hexes: &[String], relay_urls: &[String]) -> Result<(), String> {
    if pubkey_hexes.is_empty() {
        return Err("No pubkeys provided".to_string());
    }

    let pubkeys: Vec<Pubkey> = pubkey_hexes
        .iter()
        .map(|hex| Pubkey::from_hex(hex).map_err(|e| format!("Invalid pubkey {hex}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;

    let shared = get_shared_state();
    // Signal any existing worker to stop before creating a new generation.
    shared.running.store(false, Ordering::SeqCst);
    let my_generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;

    // Signal any previous thread to stop (don't wait - it will exit on its own)
    // This avoids blocking on join() which can cause ANR
    {
        let mut handle_guard = shared
            .thread_handle
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        if handle_guard.is_some() {
            info!("Previous worker thread exists, will be replaced");
            // Don't join - just drop the handle, the thread will exit when it checks running flag
            let _ = handle_guard.take();
        }
    }

    // Read Ndb clone for the worker thread
    let ndb_clone = shared.ndb.read().ok().and_then(|guard| guard.clone());

    if ndb_clone.is_none() {
        warn!("Ndb not set — worker will operate without nostrdb integration");
    }

    // Set running flag before spawning thread
    shared.running.store(true, Ordering::SeqCst);

    // Clone data needed by worker thread
    let relay_urls_owned = relay_urls.to_vec();
    let shared_clone = shared.clone();

    // Spawn worker thread that owns all non-Send state
    let handle = thread::spawn(move || {
        notification_worker(
            shared_clone,
            pubkeys,
            relay_urls_owned,
            my_generation,
            ndb_clone,
        );
    });

    // Store thread handle
    if let Ok(mut handle_guard) = shared.thread_handle.lock() {
        *handle_guard = Some(handle);
    }

    info!(
        "Started notification subscriptions for {} accounts (generation {})",
        pubkey_hexes.len(),
        my_generation
    );
    Ok(())
}

/// Stop notification subscriptions and signal the worker thread to exit.
#[profiling::function]
pub fn stop_subscriptions() {
    let shared = get_shared_state();
    shared.running.store(false, Ordering::SeqCst);
    info!("Signaled notification subscriptions to stop");
}

/// Get the number of currently connected relays.
pub fn get_connected_relay_count() -> i32 {
    get_shared_state().connected_count.load(Ordering::SeqCst)
}

/// Worker thread that owns all non-Send state and handles relay I/O.
///
/// Events from relays are ingested into nostrdb, then polled via a
/// subscription to build notifications using the Note API.
#[profiling::function]
fn notification_worker(
    shared: Arc<SharedState>,
    pubkeys: Vec<Pubkey>,
    relay_urls: Vec<String>,
    my_generation: u64,
    ndb: Option<Ndb>,
) {
    info!(
        "Notification worker thread started for {} accounts (ndb={})",
        pubkeys.len(),
        ndb.is_some()
    );

    // Create all state inside the worker thread
    let mut state = WorkerState::new(pubkeys, relay_urls);

    // Set up relay-level subscriptions
    setup_subscriptions(&mut state.pool, &state.pubkeys, state.last_seen_timestamp);

    // Set up nostrdb subscription for notification-relevant events
    let ndb_sub = ndb.as_ref().and_then(|ndb| {
        let pubkey_refs: Vec<&[u8; 32]> = state.pubkeys.iter().map(|pk| pk.bytes()).collect();

        let notification_filter = Filter::new()
            .kinds(NOTIFICATION_KINDS.iter().map(|&k| k as u64))
            .pubkey(pubkey_refs.clone())
            .build();

        let dm_filter = Filter::new().kinds([4, 1059]).pubkey(pubkey_refs).build();

        match ndb.subscribe(&[notification_filter, dm_filter]) {
            Ok(sub) => {
                info!("Created ndb subscription for notification worker");
                Some(sub)
            }
            Err(e) => {
                warn!("Failed to create ndb subscription: {}", e);
                None
            }
        }
    });

    let pubkey_bytes: Vec<[u8; 32]> = state.pubkeys.iter().map(|pk| *pk.bytes()).collect();

    // Main event loop
    while shared.running.load(Ordering::SeqCst)
        && shared.generation.load(Ordering::SeqCst) == my_generation
    {
        // Send keepalive pings
        state.pool.keepalive_ping(|| {});

        // Update connected relay count
        let connected = state
            .pool
            .relays
            .iter()
            .filter(|r| matches!(r.status(), RelayStatus::Connected))
            .count() as i32;
        shared.connected_count.store(connected, Ordering::SeqCst);

        // Step 1: Drain relay pool and ingest events into ndb
        let mut ingested = 0;
        loop {
            match state.pool.try_recv() {
                Some(pool_event) => {
                    let event = pool_event.into_owned();
                    // Handle connection events for re-subscription
                    handle_pool_connection_event(&mut state, &event);

                    // Ingest message into ndb if available
                    if let Some(ref ndb) = ndb {
                        if let Some(json) = extract_ws_message_text(&event) {
                            let _ = ndb.process_event_with(json, IngestMetadata::new());
                            ingested += 1;
                        }
                    }
                }
                None => break,
            }
        }

        // Step 2: Wait for ndb ingestion (~150ms based on nostrdb tests)
        if ingested > 0 {
            thread::sleep(std::time::Duration::from_millis(200));
        }

        // Step 3: Poll for notification-relevant notes via ndb
        if let (Some(ref ndb), Some(sub)) = (&ndb, ndb_sub) {
            let note_keys = ndb.poll_for_notes(sub, 50);
            if !note_keys.is_empty() {
                if let Ok(txn) = Transaction::new(ndb) {
                    for nk in note_keys {
                        let Ok(note) = ndb.get_note_by_key(&txn, nk) else {
                            continue;
                        };
                        process_ndb_notification(
                            ndb,
                            &txn,
                            &note,
                            &state.pubkeys,
                            &pubkey_bytes,
                            &mut state.processed_events,
                            &mut state.processed_events_order,
                            &mut state.last_seen_timestamp,
                        );
                    }
                }
            }
        }

        // Step 4: Idle sleep if nothing happened
        if ingested == 0 {
            thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    // Cleanup subscriptions
    state.pool.unsubscribe(SUB_NOTIFICATIONS.to_string());
    state.pool.unsubscribe(SUB_DMS.to_string());
    state.pool.unsubscribe(SUB_RELAY_LIST.to_string());

    info!("Notification worker thread stopped");
}

/// Process a note from nostrdb into a notification.
///
/// Replaces the old `handle_event_message()` — uses Note API instead of
/// serde_json parsing, and ndb profile lookups instead of manual cache.
fn process_ndb_notification(
    ndb: &Ndb,
    txn: &Transaction,
    note: &nostrdb::Note,
    pubkeys: &[Pubkey],
    pubkey_bytes: &[[u8; 32]],
    processed_events: &mut HashSet<String>,
    processed_events_order: &mut std::collections::VecDeque<String>,
    last_seen_timestamp: &mut u64,
) {
    let kind = note.kind() as i32;
    let id_hex = hex::encode(note.id());
    let author_hex = hex::encode(note.pubkey());

    // Only process notification-relevant kinds
    if !NOTIFICATION_KINDS.contains(&kind) {
        return;
    }

    // Self-notification suppression
    let is_own_event = pubkeys.iter().any(|pk| pk.hex() == author_hex);
    if is_own_event {
        let p_tags = ndb_helpers::extract_p_tags_from_note(note);
        let mentions_only_self = p_tags
            .iter()
            .all(|p| pubkeys.iter().any(|pk| pk.hex() == *p));
        if mentions_only_self {
            debug!(
                "Suppressing self-notification: kind={} id={}",
                kind,
                safe_prefix(&id_hex, 8)
            );
            return;
        }
    }

    // Dedup
    if !record_event_if_new(processed_events, processed_events_order, &id_hex) {
        debug!("Skipping duplicate event id={}", safe_prefix(&id_hex, 8));
        return;
    }

    // Track latest event timestamp for reconnect resume
    let created_at = note.created_at();
    if created_at > *last_seen_timestamp {
        *last_seen_timestamp = created_at;
    }

    info!(
        "NEW EVENT: kind={} id={} from={}",
        kind,
        safe_prefix(&id_hex, 8),
        safe_prefix(&author_hex, 8),
    );

    // Profile lookup via nostrdb (replaces manual cache + JSON parsing)
    let (author_name, picture_url) = ndb_helpers::lookup_profile_ndb(ndb, txn, note.pubkey());

    // Resolve @npub mentions using nostrdb profiles
    #[cfg(target_os = "android")]
    let resolved_content = {
        let content = note.content();
        let mentioned_pubkeys = extract_mentioned_pubkeys(content);
        // Build a temporary profile cache from ndb for resolve_mentions
        let mut profile_cache = std::collections::HashMap::new();
        for pk_hex in &mentioned_pubkeys {
            if let Ok(pk_bytes) = hex::decode(pk_hex) {
                if let Ok(arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) {
                    let (name, picture) = ndb_helpers::lookup_profile_ndb(ndb, txn, &arr);
                    profile_cache.insert(
                        pk_hex.clone(),
                        notedeck::notifications::CachedProfile {
                            name,
                            picture_url: picture,
                        },
                    );
                }
            }
        }
        resolve_mentions(content, &profile_cache)
    };
    #[cfg(not(target_os = "android"))]
    let resolved_content = note.content().to_string();

    // Extract p-tags and zap amount via Note tag API
    let p_tags = ndb_helpers::extract_p_tags_from_note(note);
    let zap_amount_sats = if kind == 9735 {
        ndb_helpers::extract_zap_amount_from_note(note)
    } else {
        None
    };

    let event = ExtractedEvent {
        id: id_hex,
        kind,
        pubkey: author_hex,
        created_at,
        content: resolved_content,
        p_tags,
        zap_amount_sats,
        raw_json: String::new(),
    };

    notify_nostr_event(&event, author_name.as_deref(), picture_url.as_deref());
}

/// Configure Nostr subscriptions for notifications, DMs, and relay lists.
/// Sets up filters for mentions, reactions, reposts, zaps, and direct messages
/// across all monitored pubkeys.
/// `since` is the unix timestamp to subscribe from (typically the last-seen event time).
#[profiling::function]
fn setup_subscriptions(pool: &mut RelayPool, pubkeys: &[Pubkey], since: u64) {
    let all_pubkey_bytes: Vec<&[u8; 32]> = pubkeys.iter().map(|pk| pk.bytes()).collect();

    // Subscribe to mentions, replies, reactions, reposts, zaps for ALL accounts
    // kinds: 1 (text), 6 (repost), 7 (reaction), 9735 (zap receipt)
    let notification_filter = Filter::new()
        .kinds([1, 6, 7, 9735])
        .pubkey(all_pubkey_bytes.clone())
        .since(since)
        .build();

    info!(
        "Subscribing to notifications for {} accounts since timestamp {}",
        pubkeys.len(),
        since
    );
    pool.subscribe(SUB_NOTIFICATIONS.to_string(), vec![notification_filter]);

    // Subscribe to DMs (kind 4 legacy, kind 1059 gift wrap) for ALL accounts
    let dm_filter = Filter::new()
        .kinds([4, 1059])
        .pubkey(all_pubkey_bytes)
        .since(since)
        .build();

    pool.subscribe(SUB_DMS.to_string(), vec![dm_filter]);

    // Subscribe to relay list updates (NIP-65) for ALL accounts
    let relay_list_filter = Filter::new()
        .kinds([10002, 10050])
        .authors(pubkeys.iter().map(|pk| pk.bytes()).collect::<Vec<_>>())
        .limit(pubkeys.len() as u64)
        .build();

    pool.subscribe(SUB_RELAY_LIST.to_string(), vec![relay_list_filter]);

    debug!(
        "Set up notification subscriptions for {} accounts",
        pubkeys.len()
    );
}

/// Handle connection-related pool events (opened/closed/error).
/// Dispatches re-subscription on reconnect.
fn handle_pool_connection_event(state: &mut WorkerState, pool_event: &enostr::PoolEventBuf) {
    use enostr::ewebsock::WsEvent;

    match pool_event.event {
        WsEvent::Opened => {
            debug!("Connected to relay: {}", pool_event.relay);
            let pubkeys = state.pubkeys.clone();
            setup_subscriptions(&mut state.pool, &pubkeys, state.last_seen_timestamp);
            notify_relay_status_changed();
        }
        WsEvent::Closed => {
            debug!("Disconnected from relay: {}", pool_event.relay);
            notify_relay_status_changed();
        }
        WsEvent::Error(ref err) => {
            error!("Relay error {}: {:?}", pool_event.relay, err);
            notify_relay_status_changed();
        }
        _ => {}
    }
}

/// Extract the raw text from a WebSocket message for ndb ingestion.
///
/// The relay sends `["EVENT","sub",{...}]` — exactly what `process_event_with()` expects.
fn extract_ws_message_text(pool_event: &enostr::PoolEventBuf) -> Option<&str> {
    use enostr::ewebsock::{WsEvent, WsMessage};
    match &pool_event.event {
        WsEvent::Message(WsMessage::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// Maximum number of event IDs to track for deduplication.
const MAX_PROCESSED_EVENTS: usize = 10_000;

/// Record an event ID if not already seen. Returns true if the event is new.
/// Maintains a bounded dedup set with LRU eviction at `MAX_PROCESSED_EVENTS`.
fn record_event_if_new(
    processed_events: &mut HashSet<String>,
    processed_events_order: &mut std::collections::VecDeque<String>,
    event_id: &str,
) -> bool {
    if processed_events.contains(event_id) {
        return false;
    }

    let event_id_owned = event_id.to_string();
    processed_events.insert(event_id_owned.clone());
    processed_events_order.push_back(event_id_owned);

    // Evict oldest entries to maintain bounded memory usage
    while processed_events.len() > MAX_PROCESSED_EVENTS {
        if let Some(oldest) = processed_events_order.pop_front() {
            processed_events.remove(&oldest);
        } else {
            processed_events.clear();
            break;
        }
    }

    true
}

/// Notify Kotlin about a new Nostr event with structured data
/// This passes individual fields instead of raw JSON, eliminating JSON parsing in Kotlin
fn notify_nostr_event(
    event: &ExtractedEvent,
    author_name: Option<&str>,
    picture_url: Option<&str>,
) {
    info!(
        "notify_nostr_event called: kind={}, id={}",
        event.kind,
        safe_prefix(&event.id, 8)
    );

    #[cfg(target_os = "android")]
    {
        let shared = get_shared_state();
        let callback_guard = match shared.java_callback.lock() {
            Ok(guard) => guard,
            Err(e) => {
                error!("Failed to lock java_callback: {}", e);
                return;
            }
        };

        let callback = match *callback_guard {
            Some(ref cb) => cb,
            None => {
                warn!("java_callback is None - JNI callback not set up");
                return;
            }
        };

        let mut env = match callback.jvm.attach_current_thread() {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to attach JNI thread: {:?}", e);
                return;
            }
        };

        info!("JNI thread attached, calling onNostrEvent");

        let event_id_jstring = match env.new_string(&event.id) {
            Ok(s) => s,
            Err(_) => return,
        };
        let author_pubkey_jstring = match env.new_string(&event.pubkey) {
            Ok(s) => s,
            Err(_) => return,
        };
        let content_jstring = match env.new_string(&event.content) {
            Ok(s) => s,
            Err(_) => return,
        };
        let author_name_jstring = match author_name {
            Some(name) => match env.new_string(name) {
                Ok(s) => JObject::from(s),
                Err(_) => JObject::null(),
            },
            None => JObject::null(),
        };
        let picture_url_jstring = match picture_url {
            Some(url) => match env.new_string(url) {
                Ok(s) => JObject::from(s),
                Err(_) => JObject::null(),
            },
            None => JObject::null(),
        };
        let raw_json_jstring = match env.new_string(&event.raw_json) {
            Ok(s) => s,
            Err(_) => return,
        };

        let zap_amount = event.zap_amount_sats.unwrap_or(-1);

        match env.call_method(
            &callback.service_obj,
            "onNostrEvent",
            "(Ljava/lang/String;ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JLjava/lang/String;)V",
            &[
                JValue::Object(&JObject::from(event_id_jstring)),
                JValue::Int(event.kind),
                JValue::Object(&JObject::from(author_pubkey_jstring)),
                JValue::Object(&JObject::from(content_jstring)),
                JValue::Object(&author_name_jstring),
                JValue::Object(&picture_url_jstring),
                JValue::Long(zap_amount),
                JValue::Object(&JObject::from(raw_json_jstring)),
            ],
        ) {
            Ok(_) => {
                if env.exception_check().unwrap_or(false) {
                    env.exception_clear().ok();
                    error!("JNI exception after onNostrEvent call");
                    return;
                }
                info!("JNI onNostrEvent call succeeded");
            }
            Err(e) => error!("JNI onNostrEvent call failed: {:?}", e),
        }
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = author_name;
        let _ = picture_url;
        debug!(
            "Nostr event (non-Android): kind={}, author={}, zap_sats={:?}",
            event.kind,
            safe_prefix(&event.pubkey, 8),
            event.zap_amount_sats
        );
    }
}

/// Notify Kotlin about relay connection status change
fn notify_relay_status_changed() {
    let connected_count = get_connected_relay_count();

    #[cfg(target_os = "android")]
    {
        let shared = get_shared_state();
        let callback_guard = match shared.java_callback.lock() {
            Ok(guard) => guard,
            Err(e) => {
                error!("Failed to lock java_callback: {}", e);
                return;
            }
        };

        let callback = match *callback_guard {
            Some(ref cb) => cb,
            None => return,
        };

        let mut env = match callback.jvm.attach_current_thread() {
            Ok(e) => e,
            Err(_) => return,
        };

        let _ = env.call_method(
            &callback.service_obj,
            "onRelayStatusChanged",
            "(I)V",
            &[JValue::Int(connected_count)],
        );
    }

    #[cfg(not(target_os = "android"))]
    {
        debug!("Relay status changed: {} connected", connected_count);
    }
}

// =============================================================================
// JNI exports for Android
// =============================================================================

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_service_NotificationsService_nativeStartSubscriptions(
    mut env: JNIEnv,
    obj: JObject,
    pubkey_hexes_json: JString,
    relay_urls_json: JString,
) {
    // Always refresh the callback reference on each start
    // This ensures we have a valid reference even after service restart
    update_jni_callback(&mut env, obj);

    let pubkey_hexes: Vec<String> = match env.get_string(&pubkey_hexes_json) {
        Ok(s) => {
            let json_str: String = s.into();
            // Support both JSON array and bare string (backward compat)
            match serde_json::from_str::<Vec<String>>(&json_str) {
                Ok(arr) => arr,
                Err(_) => {
                    // Bare string — wrap as single-element array
                    if json_str.is_empty() {
                        error!("Empty pubkey string");
                        return;
                    }
                    vec![json_str]
                }
            }
        }
        Err(e) => {
            error!("Failed to get pubkey string: {}", e);
            return;
        }
    };

    let relay_urls: Vec<String> = match env.get_string(&relay_urls_json) {
        Ok(s) => {
            let json_str: String = s.into();
            serde_json::from_str(&json_str).unwrap_or_default()
        }
        Err(e) => {
            warn!("Failed to get relay URLs, using defaults: {}", e);
            Vec::new()
        }
    };

    if let Err(e) = start_subscriptions(&pubkey_hexes, &relay_urls) {
        error!("Failed to start subscriptions: {}", e);
    }
}

/// Update the global JNI callback reference.
/// Called on each service start to ensure we have a valid reference even after restart.
/// Stores a global reference to the service object for later JNI calls.
#[cfg(target_os = "android")]
fn update_jni_callback(env: &mut JNIEnv, obj: JObject) {
    let jvm = match env.get_java_vm() {
        Ok(jvm) => jvm,
        Err(_) => return,
    };
    let global_ref = match env.new_global_ref(obj) {
        Ok(r) => r,
        Err(_) => return,
    };
    let shared = get_shared_state();
    let mut guard = match shared.java_callback.lock() {
        Ok(g) => g,
        Err(e) => {
            error!("Failed to lock java_callback for update: {}", e);
            return;
        }
    };
    *guard = Some(JavaCallback {
        jvm,
        service_obj: global_ref,
    });
    info!("JNI callback reference updated");
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_service_NotificationsService_nativeStopSubscriptions(
    _env: JNIEnv,
    _obj: JObject,
) {
    stop_subscriptions();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_damus_notedeck_service_NotificationsService_nativeGetConnectedRelayCount(
    _env: JNIEnv,
    _obj: JObject,
) -> jint {
    get_connected_relay_count()
}
