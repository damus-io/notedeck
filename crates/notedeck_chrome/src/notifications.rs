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
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use tracing::{debug, error, info, warn};

#[cfg(target_os = "android")]
use bech32::Bech32;
#[cfg(target_os = "android")]
use std::collections::HashMap;

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

/// Global shared state - only contains Send+Sync types
struct SharedState {
    /// Flag to signal worker thread to stop
    running: AtomicBool,
    /// Current count of connected relays (updated by worker thread)
    connected_count: AtomicI32,
    /// Handle to the worker thread
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            connected_count: AtomicI32::new(0),
            thread_handle: Mutex::new(None),
        }
    }
}

/// Global shared state singleton
static SHARED_STATE: OnceLock<Arc<SharedState>> = OnceLock::new();

fn get_shared_state() -> Arc<SharedState> {
    SHARED_STATE
        .get_or_init(|| Arc::new(SharedState::default()))
        .clone()
}

/// Callback interface for sending events back to Kotlin
/// Uses Mutex<Option<>> instead of OnceLock to allow refreshing on service restart
#[cfg(target_os = "android")]
static JAVA_CALLBACK: Mutex<Option<JavaCallback>> = Mutex::new(None);

#[cfg(target_os = "android")]
struct JavaCallback {
    jvm: jni::JavaVM,
    service_obj: jni::objects::GlobalRef,
}

#[cfg(target_os = "android")]
unsafe impl Send for JavaCallback {}
#[cfg(target_os = "android")]
unsafe impl Sync for JavaCallback {}

/// Cached profile information
#[derive(Clone, Default)]
struct CachedProfile {
    name: Option<String>,
    picture_url: Option<String>,
}

/// Thread-local state owned entirely by the worker thread (contains non-Send types)
struct WorkerState {
    pool: RelayPool,
    pubkey: Pubkey,
    processed_events: HashSet<String>,
    profile_cache: std::collections::HashMap<String, CachedProfile>,
    requested_profiles: HashSet<String>,
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

        Self {
            pool,
            pubkey,
            processed_events: HashSet::new(),
            profile_cache: std::collections::HashMap::new(),
            requested_profiles: HashSet::new(),
        }
    }
}

/// Start notification subscriptions for the given pubkey and relay URLs.
/// If relay_urls is empty, falls back to DEFAULT_RELAYS.
#[profiling::function]
pub fn start_subscriptions(pubkey_hex: &str, relay_urls: &[String]) -> Result<(), String> {
    let pubkey = Pubkey::from_hex(pubkey_hex).map_err(|e| format!("Invalid pubkey: {e}"))?;
    let shared = get_shared_state();

    // Check if already running
    if shared.running.load(Ordering::SeqCst) {
        info!("Notification subscriptions already running");
        return Ok(());
    }

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
        notification_worker(shared_clone, pubkey, relay_urls_owned);
    });

    // Store thread handle
    if let Ok(mut handle_guard) = shared.thread_handle.lock() {
        *handle_guard = Some(handle);
    }

    info!("Started notification subscriptions for {}", pubkey_hex);
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
fn notification_worker(shared: Arc<SharedState>, pubkey: Pubkey, relay_urls: Vec<String>) {
    info!("Notification worker thread started");

    // Create all state inside the worker thread
    let mut state = WorkerState::new(pubkey.clone(), relay_urls);

    // Set up initial subscriptions
    setup_subscriptions(&mut state.pool, &pubkey);

    let mut loop_count: u64 = 0;

    // Main event loop
    while shared.running.load(Ordering::SeqCst) {
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
fn setup_subscriptions(pool: &mut RelayPool, pubkey: &Pubkey) {
    let pubkey_hex = pubkey.hex();

    // Use current timestamp to only receive new events (not historical)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Subscribe to mentions, replies, reactions, reposts, zaps
    // kinds: 1 (text), 6 (repost), 7 (reaction), 9735 (zap receipt)
    let notification_filter = Filter::new()
        .kinds([1, 6, 7, 9735])
        .pubkey([pubkey.bytes()])
        .since(now)
        .build();

    info!("Subscribing to notifications since timestamp {}", now);
    pool.subscribe(SUB_NOTIFICATIONS.to_string(), vec![notification_filter]);

    // Subscribe to DMs (kind 4 legacy, kind 1059 gift wrap)
    let dm_filter = Filter::new()
        .kinds([4, 1059])
        .pubkey([pubkey.bytes()])
        .since(now)
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
fn handle_pool_event(state: &mut WorkerState, pool_event: enostr::PoolEventBuf) {
    use enostr::ewebsock::{WsEvent, WsMessage};

    match pool_event.event {
        WsEvent::Opened => {
            debug!("Connected to relay: {}", pool_event.relay);
            // Re-subscribe on reconnect
            setup_subscriptions(&mut state.pool, &state.pubkey);
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

    // Only notify for relevant event kinds
    // 1=text note, 4=legacy DM, 6=repost, 7=reaction, 1059=gift-wrapped DM, 9735=zap
    const NOTIFICATION_KINDS: &[i32] = &[1, 4, 6, 7, 1059, 9735];
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

    info!(
        "NEW EVENT: kind={} id={} from={} sub={}",
        event.kind,
        &event.id[..8],
        &event.pubkey[..8],
        sub_id
    );

    // Look up author profile from cache, request if not cached
    let mut profile = state.profile_cache.get(&event.pubkey).cloned();
    if profile.is_none() {
        request_profile_if_needed(state, &event.pubkey);

        // Brief wait for profile to arrive - poll relay messages
        // This gives relays a chance to respond with the profile before we show the notification
        for _ in 0..20 {
            // Process any pending messages (including potential profile response)
            while let Some(ev) = state.pool.try_recv() {
                // Only process text messages that might be profile responses
                if let enostr::ewebsock::WsEvent::Message(enostr::ewebsock::WsMessage::Text(
                    ref text,
                )) = ev.event
                {
                    // Try to parse as relay message and handle kind 0 events
                    if let Ok(enostr::RelayMessage::Event(_sub, event_json)) =
                        enostr::RelayMessage::from_json(text)
                    {
                        if let Some(evt) = extract_event(event_json) {
                            if evt.kind == 0 {
                                handle_profile_event(state, &evt);
                            }
                        }
                    }
                }
            }

            // Check if profile arrived
            if let Some(p) = state.profile_cache.get(&event.pubkey) {
                profile = Some(p.clone());
                info!(
                    "Profile fetched for {}: name={:?}",
                    &event.pubkey[..8],
                    p.name
                );
                break;
            }

            // Brief sleep before next poll
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    let author_name = profile.as_ref().and_then(|p| p.name.clone());
    let picture_url = profile.as_ref().and_then(|p| p.picture_url.clone());

    // Request profiles for mentioned users, then resolve mentions
    #[cfg(target_os = "android")]
    let resolved_content = {
        // Extract mentioned npubs and request their profiles
        let mentioned_pubkeys = extract_mentioned_pubkeys(&event.content);
        for pubkey in &mentioned_pubkeys {
            request_profile_if_needed(state, pubkey);
        }

        // Brief wait for mentioned profiles to arrive
        if !mentioned_pubkeys.is_empty() {
            for _ in 0..10 {
                while let Some(ev) = state.pool.try_recv() {
                    if let enostr::ewebsock::WsEvent::Message(enostr::ewebsock::WsMessage::Text(
                        ref text,
                    )) = ev.event
                    {
                        if let Ok(enostr::RelayMessage::Event(_sub, event_json)) =
                            enostr::RelayMessage::from_json(text)
                        {
                            if let Some(evt) = extract_event(event_json) {
                                if evt.kind == 0 {
                                    handle_profile_event(state, &evt);
                                }
                            }
                        }
                    }
                }
                // Check if all mentioned profiles are cached
                if mentioned_pubkeys
                    .iter()
                    .all(|pk| state.profile_cache.contains_key(pk))
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
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
        content: resolved_content,
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
    info!(
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
    info!("Profile has keys: {:?}", keys);

    // Prefer display_name, fall back to name (handle empty strings properly)
    let display_name_str = obj
        .get("display_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let name_str = obj
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    info!("display_name={:?}, name={:?}", display_name_str, name_str);

    let name = display_name_str.or(name_str).map(|s| s.to_string());

    // Get picture URL
    let picture_url = obj
        .get("picture")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && (s.starts_with("http://") || s.starts_with("https://")))
        .map(|s| s.to_string());

    CachedProfile { name, picture_url }
}

/// Decode a bech32 npub to hex pubkey.
/// Returns None if decoding fails.
#[cfg(target_os = "android")]
fn decode_npub(npub: &str) -> Option<String> {
    use bech32::primitives::decode::CheckedHrpstring;

    // npub must start with "npub1"
    if !npub.starts_with("npub1") {
        return None;
    }

    let checked = CheckedHrpstring::new::<Bech32>(npub).ok()?;
    if checked.hrp().as_str() != "npub" {
        return None;
    }

    let data: Vec<u8> = checked.byte_iter().collect();
    if data.len() != 32 {
        return None;
    }

    Some(hex::encode(data))
}

/// Extract hex pubkeys from nostr:npub mentions in content.
#[cfg(target_os = "android")]
fn extract_mentioned_pubkeys(content: &str) -> Vec<String> {
    let mut pubkeys = Vec::new();
    let mut search_start = 0;

    while let Some(pos) = content[search_start..].find("nostr:npub1") {
        let abs_pos = search_start + pos;
        let after_prefix = abs_pos + 11;

        let npub_end = content[after_prefix..]
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            .map(|p| after_prefix + p)
            .unwrap_or(content.len());

        let npub = &content[abs_pos + 6..npub_end];

        if let Some(hex_pubkey) = decode_npub(npub) {
            pubkeys.push(hex_pubkey);
        }

        search_start = npub_end;
    }

    pubkeys
}

/// Resolve nostr:npub mentions in content to display names.
/// Looks up profiles from cache and replaces npub references with @name.
#[cfg(target_os = "android")]
fn resolve_mentions(
    content: &str,
    profile_cache: &std::collections::HashMap<String, CachedProfile>,
) -> String {
    let mut result = content.to_string();
    let mut search_start = 0;

    // Find all nostr:npub1... patterns
    while let Some(pos) = result[search_start..].find("nostr:npub1") {
        let abs_pos = search_start + pos;
        let after_prefix = abs_pos + 11; // length of "nostr:npub1"

        // Find end of npub (bech32 chars are lowercase alphanumeric)
        let npub_end = result[after_prefix..]
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            .map(|p| after_prefix + p)
            .unwrap_or(result.len());

        let full_match = &result[abs_pos..npub_end];
        let npub = &result[abs_pos + 6..npub_end]; // skip "nostr:"

        // Decode npub to hex pubkey and look up profile
        let replacement = if let Some(hex_pubkey) = decode_npub(npub) {
            if let Some(profile) = profile_cache.get(&hex_pubkey) {
                if let Some(name) = &profile.name {
                    format!("@{}", name)
                } else {
                    // Fallback: shorten npub
                    format!("@{}...", &npub[..npub.len().min(12)])
                }
            } else {
                format!("@{}...", &npub[..npub.len().min(12)])
            }
        } else {
            format!("@{}...", &npub[..npub.len().min(12)])
        };

        result = format!(
            "{}{}{}",
            &result[..abs_pos],
            replacement,
            &result[npub_end..]
        );
        search_start = abs_pos + replacement.len();
    }

    result
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

/// Record an event ID if not already seen. Returns true if the event is new.
/// Also prunes the cache when it exceeds 10,000 entries.
fn record_event_if_new(state: &mut WorkerState, event_id: &str) -> bool {
    if state.processed_events.contains(event_id) {
        return false;
    }

    state.processed_events.insert(event_id.to_string());

    if state.processed_events.len() > 10000 {
        state.processed_events.clear();
    }

    true
}

/// Structured event data extracted from JSON - passed to Kotlin via JNI
/// This avoids JSON parsing in Kotlin entirely
struct ExtractedEvent {
    id: String,
    kind: i32,
    pubkey: String,
    content: String,
    /// Zap amount in satoshis (only for kind 9735 zap receipts)
    zap_amount_sats: Option<i64>,
    /// Raw event JSON for broadcast compatibility (includes tags, created_at, sig)
    raw_json: String,
}

/// Extract all event fields from JSON using proper JSON parsing
/// Note: enostr RelayMessage::Event passes the ENTIRE relay message ["EVENT", "sub_id", {...}]
/// not just the event object, so we need to extract the third element.
fn extract_event(relay_message: &str) -> Option<ExtractedEvent> {
    // Use serde_json for robust parsing that handles escaped strings correctly
    let value: serde_json::Value = match serde_json::from_str(relay_message) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse relay message JSON: {}", e);
            return None;
        }
    };

    // The relay message is ["EVENT", "sub_id", {event}] - extract the event object (index 2)
    let obj = if let Some(arr) = value.as_array() {
        // This is the expected format from enostr: ["EVENT", "sub_id", {event}]
        if arr.len() < 3 {
            warn!("EVENT message array too short: {} elements", arr.len());
            return None;
        }
        match arr[2].as_object() {
            Some(o) => o,
            None => {
                warn!("Third element of EVENT message is not an object");
                return None;
            }
        }
    } else if let Some(o) = value.as_object() {
        // Direct event object (shouldn't happen with enostr but handle it anyway)
        o
    } else {
        warn!("Relay message is neither array nor object");
        return None;
    };

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let kind = obj.get("kind").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let pubkey = obj
        .get("pubkey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content = obj
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Validate id and pubkey are proper hex (64 chars)
    // Log warning but don't silently drop - helps debug relay issues
    if id.len() != 64 {
        warn!(
            "Dropping event with invalid id length {}: {}",
            id.len(),
            &id[..id.len().min(16)]
        );
        return None;
    }
    if pubkey.len() != 64 {
        warn!(
            "Dropping event with invalid pubkey length {}: {}",
            pubkey.len(),
            &pubkey[..pubkey.len().min(16)]
        );
        return None;
    }

    // Extract zap amount for kind 9735 (zap receipt) events
    let zap_amount_sats = if kind == 9735 {
        extract_zap_amount(obj)
    } else {
        None
    };

    // Serialize just the event object for broadcast (not the full relay message)
    let raw_json = serde_json::to_string(obj).unwrap_or_default();

    Some(ExtractedEvent {
        id,
        kind,
        pubkey,
        content,
        zap_amount_sats,
        raw_json,
    })
}

/// Extract zap amount from a kind 9735 event's tags
/// Looks for bolt11 tag and parses the invoice amount
fn extract_zap_amount(event: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
    let tags = event.get("tags")?.as_array()?;

    for tag in tags {
        let tag_arr = match tag.as_array() {
            Some(arr) => arr,
            None => continue,
        };
        if tag_arr.len() < 2 {
            continue;
        }
        let tag_name = match tag_arr[0].as_str() {
            Some(name) => name,
            None => continue,
        };
        if tag_name != "bolt11" {
            continue;
        }
        let bolt11 = match tag_arr[1].as_str() {
            Some(s) => s,
            None => continue,
        };
        return parse_bolt11_amount(bolt11);
    }
    None
}

/// Parse amount from a BOLT11 invoice string
/// BOLT11 format: ln<prefix><amount><multiplier>1<data>
/// - prefix: bc (mainnet), tb (testnet), bs (signet)
/// - amount: optional digits
/// - multiplier: optional m/u/n/p
/// - 1: separator (always present)
/// - data: timestamp and tagged fields
///
/// Examples:
/// - lnbc1... = no amount (1 is separator)
/// - lnbc1000u1... = 1000 micro-BTC = 100,000 sats
/// - lnbc1m1... = 1 milli-BTC = 100,000 sats
fn parse_bolt11_amount(bolt11: &str) -> Option<i64> {
    let lower = bolt11.to_lowercase();

    // Find the amount portion after prefix
    let after_prefix = if lower.starts_with("lnbc") {
        &lower[4..]
    } else if lower.starts_with("lntb") || lower.starts_with("lnbs") {
        &lower[4..]
    } else {
        return None;
    };

    // BOLT11: amount is digits followed by optional multiplier, then '1' separator
    // If first char is '1', it's the separator (no amount specified)
    let chars: Vec<char> = after_prefix.chars().collect();
    if chars.is_empty() {
        return None;
    }

    // Check for no-amount invoice: first char after prefix is '1' separator
    if chars[0] == '1' {
        return None; // No amount specified
    }

    // Parse digits for amount
    let mut amount_end = 0;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() {
            amount_end = i + 1;
        } else {
            break;
        }
    }

    if amount_end == 0 {
        return None;
    }

    // Check for multiplier after digits
    let multiplier_char = if amount_end < chars.len() {
        let c = chars[amount_end];
        if c == 'm' || c == 'u' || c == 'n' || c == 'p' {
            Some(c)
        } else if c == '1' {
            None // '1' is separator, no multiplier means whole BTC (very rare)
        } else {
            return None; // Invalid character
        }
    } else {
        return None; // No separator found
    };

    let amount_str: String = chars[..amount_end].iter().collect();
    let amount: i64 = amount_str.parse().ok()?;

    // Convert to millisatoshis based on multiplier using checked arithmetic to avoid overflow.
    // Multipliers: (numerator, denominator) to compute msats = amount * numerator / denominator
    // - None (whole BTC): 1 BTC = 100,000,000,000 msats
    // - 'm' (milli): 1 mBTC = 100,000,000 msats
    // - 'u' (micro): 1 uBTC = 100,000 msats
    // - 'n' (nano): 1 nBTC = 100 msats
    // - 'p' (pico): 1 pBTC = 0.1 msats, so we use numerator=1, denom=10
    let (numerator, denominator): (i64, i64) = match multiplier_char {
        Some('m') => (100_000_000, 1),
        Some('u') => (100_000, 1),
        Some('n') => (100, 1),
        Some('p') => (1, 10),
        None => (100_000_000_000, 1),
        _ => return None,
    };

    // Use checked arithmetic to detect overflow
    let msats = amount.checked_mul(numerator)?.checked_div(denominator)?;

    // Convert millisatoshis to satoshis (floors sub-satoshi amounts)
    Some(msats / 1000)
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
        let callback_guard = match JAVA_CALLBACK.lock() {
            Ok(guard) => guard,
            Err(e) => {
                error!("Failed to lock JAVA_CALLBACK: {}", e);
                return;
            }
        };

        let callback = match *callback_guard {
            Some(ref cb) => cb,
            None => {
                warn!("JAVA_CALLBACK is None - JNI callback not set up");
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
        let callback_guard = match JAVA_CALLBACK.lock() {
            Ok(guard) => guard,
            Err(e) => {
                error!("Failed to lock JAVA_CALLBACK: {}", e);
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
    let mut guard = match JAVA_CALLBACK.lock() {
        Ok(g) => g,
        Err(e) => {
            error!("Failed to lock JAVA_CALLBACK for update: {}", e);
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

    #[test]
    fn test_bolt11_amount_parsing() {
        // Test micro-bitcoin (u) - 1000u = 100,000 sats
        assert_eq!(parse_bolt11_amount("lnbc1000u1pj9..."), Some(100_000));

        // Test milli-bitcoin (m) - 10m = 1,000,000 sats
        assert_eq!(parse_bolt11_amount("lnbc10m1pj9..."), Some(1_000_000));

        // Test nano-bitcoin (n) - 1000000n = 100 sats
        assert_eq!(parse_bolt11_amount("lnbc1000000n1pj9..."), Some(100));

        // Test no-amount invoice (1 is separator, not amount)
        assert_eq!(parse_bolt11_amount("lnbc1pj9..."), None);

        // Test whole BTC without multiplier - 2 BTC (rare)
        // Format: lnbc<amount>1<data> where amount=2
        assert_eq!(parse_bolt11_amount("lnbc21pj9..."), Some(200_000_000));

        // Test invalid prefix
        assert_eq!(parse_bolt11_amount("invalid"), None);

        // Test testnet prefix
        assert_eq!(parse_bolt11_amount("lntb1000u1pj9..."), Some(100_000));

        // Test signet prefix
        assert_eq!(parse_bolt11_amount("lnbs500u1pj9..."), Some(50_000));
    }

    #[test]
    fn test_extract_zap_event_with_amount() {
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":9735,"content":"","tags":[["bolt11","lnbc1000u1pj..."]]}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, 9735);
        assert_eq!(event.zap_amount_sats, Some(100_000)); // 1000 micro-BTC = 100,000 sats
    }
}
