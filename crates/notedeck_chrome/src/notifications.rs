//! Android notification service JNI interface
//!
//! This module provides the Rust side of the Pokey-style push notification
//! system. It manages relay connections and event subscriptions for the
//! Android foreground service.
//!
//! Architecture: Uses a worker thread that owns all non-Send types (RelayPool, etc.)
//! Communication happens via atomic flags and the worker thread handles all relay I/O.

#[cfg(target_os = "android")]
use jni::objects::{JObject, JString, JValue};
#[cfg(target_os = "android")]
use jni::sys::jint;
#[cfg(target_os = "android")]
use jni::JNIEnv;

use enostr::{Pubkey, RelayPool, RelayStatus};
use nostrdb::Filter;
use notedeck::notifications::{extract_event, CachedProfile, ExtractedEvent, NOTIFICATION_KINDS};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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
const SUB_PROFILES: &str = "notedeck_profiles";

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
            #[cfg(target_os = "android")]
            java_callback: Mutex::new(None),
        }
    }
}

/// Global shared state singleton.
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

#[cfg(target_os = "android")]
unsafe impl Send for JavaCallback {}
#[cfg(target_os = "android")]
unsafe impl Sync for JavaCallback {}

/// Thread-local state owned entirely by the worker thread (contains non-Send types)
struct WorkerState {
    pool: RelayPool,
    pubkey: Pubkey,
    processed_events: HashSet<String>,
    /// Queue tracking insertion order for bounded LRU eviction (oldest at front)
    processed_events_order: std::collections::VecDeque<String>,
    profile_cache: std::collections::HashMap<String, CachedProfile>,
    requested_profiles: HashSet<String>,
    /// Last-seen event timestamp for reconnect resume (avoids missing events)
    last_seen_timestamp: u64,
}

impl WorkerState {
    fn new(pubkey: Pubkey, relay_urls: Vec<String>) -> Self {
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
            pubkey,
            processed_events: HashSet::new(),
            processed_events_order: std::collections::VecDeque::new(),
            profile_cache: std::collections::HashMap::new(),
            requested_profiles: HashSet::new(),
            last_seen_timestamp: now,
        }
    }
}

/// Start notification subscriptions for the given pubkey and relay URLs.
/// If relay_urls is empty, falls back to DEFAULT_RELAYS.
#[profiling::function]
pub fn start_subscriptions(pubkey_hex: &str, relay_urls: &[String]) -> Result<(), String> {
    let pubkey = Pubkey::from_hex(pubkey_hex).map_err(|e| format!("Invalid pubkey: {e}"))?;
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

    // Set running flag before spawning thread
    shared.running.store(true, Ordering::SeqCst);

    // Clone data needed by worker thread
    let relay_urls_owned = relay_urls.to_vec();
    let shared_clone = shared.clone();

    // Spawn worker thread that owns all non-Send state
    let handle = thread::spawn(move || {
        notification_worker(shared_clone, pubkey, relay_urls_owned, my_generation);
    });

    // Store thread handle
    if let Ok(mut handle_guard) = shared.thread_handle.lock() {
        *handle_guard = Some(handle);
    }

    info!(
        "Started notification subscriptions for {} (generation {})",
        pubkey_hex, my_generation
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
#[profiling::function]
fn notification_worker(
    shared: Arc<SharedState>,
    pubkey: Pubkey,
    relay_urls: Vec<String>,
    my_generation: u64,
) {
    info!("Notification worker thread started");

    // Create all state inside the worker thread
    let mut state = WorkerState::new(pubkey.clone(), relay_urls);

    // Set up initial subscriptions
    setup_subscriptions(&mut state.pool, &pubkey, state.last_seen_timestamp);

    let mut loop_count: u64 = 0;

    // Main event loop
    while shared.running.load(Ordering::SeqCst)
        && shared.generation.load(Ordering::SeqCst) == my_generation
    {
        loop_count += 1;

        // Log heartbeat every 30 iterations (~30 seconds)
        if loop_count % 30 == 0 {
            let connected = state
                .pool
                .relays
                .iter()
                .filter(|r| matches!(r.status(), RelayStatus::Connected))
                .count();
            info!(
                "Worker heartbeat: loop={}, connected={} relays",
                loop_count, connected
            );
        }

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

        // Poll for events
        match state.pool.try_recv() {
            Some(pool_event) => {
                let event = pool_event.into_owned();
                handle_pool_event(&mut state, event);
            }
            None => {
                // 1 second idle sleep balances battery life vs notification latency
                thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    // Cleanup subscriptions
    state.pool.unsubscribe(SUB_NOTIFICATIONS.to_string());
    state.pool.unsubscribe(SUB_DMS.to_string());
    state.pool.unsubscribe(SUB_RELAY_LIST.to_string());
    state.pool.unsubscribe(SUB_PROFILES.to_string());

    info!("Notification worker thread stopped");
}

/// Configure Nostr subscriptions for notifications, DMs, and relay lists.
/// Sets up filters for mentions, reactions, reposts, zaps, and direct messages.
/// `since` is the unix timestamp to subscribe from (typically the last-seen event time).
#[profiling::function]
fn setup_subscriptions(pool: &mut RelayPool, pubkey: &Pubkey, since: u64) {
    let pubkey_hex = pubkey.hex();

    // Subscribe to mentions, replies, reactions, reposts, zaps
    // kinds: 1 (text), 6 (repost), 7 (reaction), 9735 (zap receipt)
    let notification_filter = Filter::new()
        .kinds([1, 6, 7, 9735])
        .pubkey([pubkey.bytes()])
        .since(since)
        .build();

    info!("Subscribing to notifications since timestamp {}", since);
    pool.subscribe(SUB_NOTIFICATIONS.to_string(), vec![notification_filter]);

    // Subscribe to DMs (kind 4 legacy, kind 1059 gift wrap)
    let dm_filter = Filter::new()
        .kinds([4, 1059])
        .pubkey([pubkey.bytes()])
        .since(since)
        .build();

    pool.subscribe(SUB_DMS.to_string(), vec![dm_filter]);

    // Subscribe to relay list updates (NIP-65)
    let relay_list_filter = Filter::new()
        .kinds([10002, 10050])
        .authors([pubkey.bytes()])
        .limit(1)
        .build();

    pool.subscribe(SUB_RELAY_LIST.to_string(), vec![relay_list_filter]);

    debug!(
        "Set up notification subscriptions for pubkey {}",
        &pubkey_hex[..8]
    );
}

/// Handle a WebSocket event from the relay pool.
/// Dispatches to appropriate handlers for connection state changes and messages.
#[profiling::function]
fn handle_pool_event(state: &mut WorkerState, pool_event: enostr::PoolEventBuf) {
    use enostr::ewebsock::{WsEvent, WsMessage};

    match pool_event.event {
        WsEvent::Opened => {
            debug!("Connected to relay: {}", pool_event.relay);
            // Re-subscribe on reconnect using last-seen timestamp to avoid gaps
            setup_subscriptions(&mut state.pool, &state.pubkey, state.last_seen_timestamp);
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
        WsEvent::Message(WsMessage::Text(ref text)) => {
            handle_relay_message(state, text);
        }
        WsEvent::Message(_) => {}
    }
}

/// Process an incoming Nostr relay message.
/// Handles EVENT messages by extracting event data, deduplicating, and notifying Kotlin.
/// Also handles OK, NOTICE, and EOSE messages for logging.
fn handle_relay_message(state: &mut WorkerState, message: &str) {
    // Parse the relay message using enostr's parser
    let relay_msg = match enostr::RelayMessage::from_json(message) {
        Ok(msg) => msg,
        Err(e) => {
            // Not all messages are parseable (AUTH, COUNT, etc.)
            debug!("Could not parse relay message: {}", e);
            return;
        }
    };

    match relay_msg {
        enostr::RelayMessage::Event(sub_id, event_json) => {
            handle_event_message(state, &sub_id, event_json);
        }
        enostr::RelayMessage::OK(_) => {
            debug!("Event OK received");
        }
        enostr::RelayMessage::Notice(notice) => {
            debug!("Relay notice: {}", notice);
        }
        enostr::RelayMessage::Eose(sub_id) => {
            debug!("End of stored events for subscription: {}", sub_id);
        }
    }
}

/// Handle an EVENT message from a relay.
/// Extracts event data, checks for duplicates, and notifies Kotlin if new.
/// Also handles kind 0 (profile) events to cache author names.
#[profiling::function]
fn handle_event_message(state: &mut WorkerState, sub_id: &str, event_json: &str) {
    let event = match extract_event(event_json) {
        Some(e) => e,
        None => {
            debug!("Failed to extract event from JSON");
            return;
        }
    };

    // Handle kind 0 (profile metadata) events - extract and cache the name
    if event.kind == 0 {
        handle_profile_event(state, &event);
        return;
    }

    if !NOTIFICATION_KINDS.contains(&event.kind) {
        debug!(
            "Ignoring event kind {} (not a notification kind)",
            event.kind
        );
        return;
    }

    if !record_event_if_new(state, &event.id) {
        debug!("Skipping duplicate event id={}", &event.id[..8]);
        return;
    }

    // Track latest event timestamp for reconnect resume
    if event.created_at > state.last_seen_timestamp {
        state.last_seen_timestamp = event.created_at;
    }

    info!(
        "NEW EVENT: kind={} id={} from={} sub={}",
        event.kind,
        &event.id[..8],
        &event.pubkey[..8],
        sub_id
    );

    // Look up author profile from cache; request if missing (will be cached for future events)
    let profile = state.profile_cache.get(&event.pubkey).cloned();
    if profile.is_none() {
        request_profile_if_needed(state, &event.pubkey);
    }

    let author_name = profile.as_ref().and_then(|p| p.name.clone());
    let picture_url = profile.as_ref().and_then(|p| p.picture_url.clone());

    // Resolve @npub mentions using cached profiles (best-effort, no blocking)
    #[cfg(target_os = "android")]
    let resolved_content = {
        let mentioned_pubkeys = extract_mentioned_pubkeys(&event.content);
        for pubkey in &mentioned_pubkeys {
            request_profile_if_needed(state, pubkey);
        }
        resolve_mentions(&event.content, &state.profile_cache)
    };
    #[cfg(not(target_os = "android"))]
    let resolved_content = event.content.clone();

    info!(
        "Notifying with profile: name={:?}, picture={:?}",
        author_name,
        picture_url.as_ref().map(|s| &s[..s.len().min(50)])
    );
    info!(
        "Resolved content: {}",
        &resolved_content[..resolved_content.len().min(100)]
    );

    // Create event with resolved content for notification
    let resolved_event = ExtractedEvent {
        id: event.id.clone(),
        kind: event.kind,
        pubkey: event.pubkey.clone(),
        created_at: event.created_at,
        content: resolved_content,
        p_tags: event.p_tags.clone(),
        zap_amount_sats: event.zap_amount_sats,
        raw_json: event.raw_json.clone(),
    };

    notify_nostr_event(
        &resolved_event,
        author_name.as_deref(),
        picture_url.as_deref(),
    );
}

/// Handle a kind 0 (profile metadata) event by extracting and caching profile info.
fn handle_profile_event(state: &mut WorkerState, event: &ExtractedEvent) {
    // Parse the content as JSON to extract name and picture
    let profile = extract_profile_info(&event.content);
    if profile.name.is_none() && profile.picture_url.is_none() {
        return;
    }

    debug!(
        "Cached profile for {}: name={:?}, picture={:?}",
        &event.pubkey[..8],
        profile.name,
        profile.picture_url.as_ref().map(|s| &s[..s.len().min(50)])
    );

    // Remove from requested set since we now have the profile
    state.requested_profiles.remove(&event.pubkey);
    state.profile_cache.insert(event.pubkey.clone(), profile);

    // Prune cache if too large - also clear requested_profiles to allow re-requests
    if state.profile_cache.len() > 1000 {
        state.profile_cache.clear();
        state.requested_profiles.clear();
        debug!("Pruned profile cache and requested set");
    }
}

/// Extract profile info (name and picture URL) from profile content JSON.
/// Prefers "display_name" over "name" for the name field.
fn extract_profile_info(content: &str) -> CachedProfile {
    // Log first 200 chars of profile content for debugging
    debug!(
        "Parsing profile content: {}",
        &content[..content.len().min(200)]
    );

    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse profile JSON: {}", e);
            return CachedProfile::default();
        }
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            warn!("Profile content is not a JSON object");
            return CachedProfile::default();
        }
    };

    // Log available keys
    let keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
    debug!("Profile has keys: {:?}", keys);

    // Prefer display_name, fall back to name (handle empty strings properly)
    let display_name_str = obj
        .get("display_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let name_str = obj
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    debug!("display_name={:?}, name={:?}", display_name_str, name_str);

    let name = display_name_str.or(name_str).map(|s| s.to_string());

    // Get picture URL
    let picture_url = obj
        .get("picture")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && (s.starts_with("http://") || s.starts_with("https://")))
        .map(|s| s.to_string());

    CachedProfile { name, picture_url }
}

/// Request profile for the given pubkey if not already requested.
/// Uses unique subscription IDs per pubkey to avoid overwriting previous requests.
fn request_profile_if_needed(state: &mut WorkerState, pubkey: &str) {
    // Don't request if already requested or cached
    if state.requested_profiles.contains(pubkey) {
        return;
    }
    if state.profile_cache.contains_key(pubkey) {
        return;
    }

    // Limit pending requests to avoid too many open subscriptions
    if state.requested_profiles.len() >= 50 {
        debug!(
            "Too many pending profile requests, skipping {}",
            &pubkey[..8]
        );
        return;
    }

    state.requested_profiles.insert(pubkey.to_string());

    // Parse pubkey and subscribe to profile
    let pubkey_bytes = match Pubkey::from_hex(pubkey) {
        Ok(pk) => pk,
        Err(_) => return,
    };

    let profile_filter = Filter::new()
        .kinds([0])
        .authors([pubkey_bytes.bytes()])
        .limit(1)
        .build();

    // Use unique subscription ID per pubkey to avoid overwriting previous requests
    let sub_id = format!("{}_{}", SUB_PROFILES, &pubkey[..16]);
    state.pool.subscribe(sub_id, vec![profile_filter]);
    debug!("Requested profile for {}", &pubkey[..8]);
}

/// Maximum number of event IDs to track for deduplication.
const MAX_PROCESSED_EVENTS: usize = 10_000;

/// Record an event ID if not already seen. Returns true if the event is new.
/// Maintains a bounded dedup set with LRU eviction at `MAX_PROCESSED_EVENTS`.
fn record_event_if_new(state: &mut WorkerState, event_id: &str) -> bool {
    if state.processed_events.contains(event_id) {
        return false;
    }

    let event_id_owned = event_id.to_string();
    state.processed_events.insert(event_id_owned.clone());
    state.processed_events_order.push_back(event_id_owned);

    // Evict oldest entries to maintain bounded memory usage
    while state.processed_events.len() > MAX_PROCESSED_EVENTS {
        if let Some(oldest) = state.processed_events_order.pop_front() {
            state.processed_events.remove(&oldest);
        } else {
            state.processed_events.clear();
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
        &event.id[..8]
    );

    #[cfg(target_os = "android")]
    {
        let callback_guard = match get_shared_state().java_callback.lock() {
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
            Ok(_) => info!("JNI onNostrEvent call succeeded"),
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
            &event.pubkey[..8],
            event.zap_amount_sats
        );
    }
}

/// Notify Kotlin about relay connection status change
fn notify_relay_status_changed() {
    let connected_count = get_connected_relay_count();

    #[cfg(target_os = "android")]
    {
        let callback_guard = match get_shared_state().java_callback.lock() {
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
    pubkey_hex: JString,
    relay_urls_json: JString,
) {
    // Always refresh the callback reference on each start
    // This ensures we have a valid reference even after service restart
    update_jni_callback(&mut env, obj);

    let pubkey: String = match env.get_string(&pubkey_hex) {
        Ok(s) => s.into(),
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

    if let Err(e) = start_subscriptions(&pubkey, &relay_urls) {
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
    let mut guard = match get_shared_state().java_callback.lock() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_event() {
        // Note: enostr passes full relay message ["EVENT", "sub_id", {...}], not just event object
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"hello world"}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(
            event.id,
            "abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234"
        );
        assert_eq!(
            event.pubkey,
            "def0123456789012345678901234567890123456789012345678901234567890"
        );
        assert_eq!(event.kind, 1);
        assert_eq!(event.content, "hello world");
        assert_eq!(event.zap_amount_sats, None);
    }

    #[test]
    fn test_extract_event_with_braces_in_content() {
        // This would break manual brace-matching but works with serde_json
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"json example: {\"foo\": \"bar\"}"}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.content, r#"json example: {"foo": "bar"}"#);
    }

    #[test]
    fn test_extract_event_empty_content() {
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":7,"content":""}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, 7);
        assert_eq!(event.content, "");
    }

    #[test]
    fn test_extract_event_direct_object() {
        // Also handle direct event object format (fallback case)
        let json = r#"{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"hello"}"#;
        let event = extract_event(json);
        assert!(event.is_some());
        assert_eq!(event.unwrap().kind, 1);
    }
}
