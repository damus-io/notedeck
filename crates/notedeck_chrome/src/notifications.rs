//! Android notification service JNI interface
//!
//! This module provides the Rust side of the Pokey-style push notification
//! system. It manages relay connections and event subscriptions for the
//! Android foreground service.

#[cfg(target_os = "android")]
use jni::objects::{JClass, JObject, JString, JValue};
#[cfg(target_os = "android")]
use jni::sys::jint;
#[cfg(target_os = "android")]
use jni::JNIEnv;

use enostr::{ClientMessage, Pubkey, RelayPool, RelayStatus};
use nostrdb::Filter;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use tracing::{debug, error, info, warn};

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

/// Global state for the notification service
static NOTIFICATION_STATE: OnceLock<Arc<Mutex<NotificationState>>> = OnceLock::new();

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

struct NotificationState {
    pool: RelayPool,
    pubkey: Option<Pubkey>,
    running: bool,
    processed_events: HashSet<String>,
    /// Handle to the event loop thread for proper shutdown
    event_loop_handle: Option<thread::JoinHandle<()>>,
}

impl Default for NotificationState {
    fn default() -> Self {
        Self {
            pool: RelayPool::new(),
            pubkey: None,
            running: false,
            processed_events: HashSet::new(),
            event_loop_handle: None,
        }
    }
}

fn get_state() -> Arc<Mutex<NotificationState>> {
    NOTIFICATION_STATE
        .get_or_init(|| Arc::new(Mutex::new(NotificationState::default())))
        .clone()
}

/// Start notification subscriptions for the given pubkey
pub fn start_subscriptions(pubkey_hex: &str) -> Result<(), String> {
    let pubkey = Pubkey::from_hex(pubkey_hex).map_err(|e| format!("Invalid pubkey: {e}"))?;

    let state = get_state();
    let mut state_guard = state.lock().map_err(|e| format!("Lock error: {e}"))?;

    if state_guard.running {
        info!("Notification subscriptions already running");
        return Ok(());
    }

    // Wait for any previous event loop thread to finish before starting a new one
    // This prevents duplicate threads on quick stop/start cycles
    if let Some(handle) = state_guard.event_loop_handle.take() {
        info!("Waiting for previous event loop thread to finish...");
        drop(state_guard); // Release lock while waiting
        let _ = handle.join(); // Wait for thread to finish
        state_guard = state.lock().map_err(|e| format!("Lock error: {e}"))?;

        // Double-check running state after reacquiring lock
        if state_guard.running {
            info!("Notification subscriptions already running (race condition avoided)");
            return Ok(());
        }
    }

    state_guard.pubkey = Some(pubkey.clone());
    state_guard.running = true;

    // Add default relays
    for relay_url in DEFAULT_RELAYS {
        if let Err(e) = state_guard.pool.add_url(relay_url.to_string(), || {}) {
            warn!("Failed to add relay {}: {}", relay_url, e);
        }
    }

    // Start the event loop in a background thread and store the handle
    let state_clone = state.clone();
    let handle = thread::spawn(move || {
        notification_event_loop(state_clone);
    });
    state_guard.event_loop_handle = Some(handle);

    drop(state_guard);

    info!("Started notification subscriptions for {}", pubkey_hex);
    Ok(())
}

/// Stop notification subscriptions
pub fn stop_subscriptions() {
    let state = get_state();
    if let Ok(mut state_guard) = state.lock() {
        state_guard.running = false;
        state_guard.pool.unsubscribe(SUB_NOTIFICATIONS.to_string());
        state_guard.pool.unsubscribe(SUB_DMS.to_string());
        state_guard.pool.unsubscribe(SUB_RELAY_LIST.to_string());
        info!("Stopped notification subscriptions");
    }
}

/// Get the number of connected relays
pub fn get_connected_relay_count() -> i32 {
    let state = get_state();
    if let Ok(state_guard) = state.lock() {
        state_guard
            .pool
            .relays
            .iter()
            .filter(|r| matches!(r.status(), RelayStatus::Connected))
            .count() as i32
    } else {
        0
    }
}

/// Main event loop for processing relay events
fn notification_event_loop(state: Arc<Mutex<NotificationState>>) {
    info!("Notification event loop started");

    // Initial subscription setup
    if let Ok(mut state_guard) = state.lock() {
        if let Some(ref pubkey) = state_guard.pubkey.clone() {
            setup_subscriptions(&mut state_guard.pool, &pubkey);
        }
    }

    loop {
        // Check if we should stop
        {
            let state_guard = match state.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            if !state_guard.running {
                break;
            }
        }

        // Process events
        let event = {
            let mut state_guard = match state.lock() {
                Ok(g) => g,
                Err(_) => break,
            };

            // Keep connections alive
            state_guard.pool.keepalive_ping(|| {});

            state_guard.pool.try_recv().map(|e| e.into_owned())
        };

        if let Some(pool_event) = event {
            handle_pool_event(&state, pool_event);
        } else {
            // No events, sleep a bit
            thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    info!("Notification event loop stopped");
}

fn setup_subscriptions(pool: &mut RelayPool, pubkey: &Pubkey) {
    let pubkey_hex = pubkey.hex();

    // Subscribe to mentions, replies, reactions, reposts, zaps
    // kinds: 1 (text), 6 (repost), 7 (reaction), 9735 (zap receipt)
    let notification_filter = Filter::new()
        .kinds([1, 6, 7, 9735])
        .pubkey_tag(pubkey.bytes())
        .limit(100)
        .build();

    pool.subscribe(SUB_NOTIFICATIONS.to_string(), vec![notification_filter]);

    // Subscribe to DMs (kind 4 legacy, kind 1059 gift wrap)
    let dm_filter = Filter::new()
        .kinds([4, 1059])
        .pubkey_tag(pubkey.bytes())
        .limit(50)
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

fn handle_pool_event(state: &Arc<Mutex<NotificationState>>, pool_event: enostr::PoolEventBuf) {
    use ewebsock::WsEvent;

    match pool_event.event {
        WsEvent::Opened => {
            debug!("Connected to relay: {}", pool_event.relay);
            // Re-subscribe on reconnect
            if let Ok(mut state_guard) = state.lock() {
                if let Some(ref pubkey) = state_guard.pubkey.clone() {
                    setup_subscriptions(&mut state_guard.pool, &pubkey);
                }
            }
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
        WsEvent::Message(ref msg) => {
            if let ewebsock::WsMessage::Text(ref text) = msg {
                handle_relay_message(state, text);
            }
        }
    }
}

fn handle_relay_message(state: &Arc<Mutex<NotificationState>>, message: &str) {
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
            // Extract event ID from the JSON for deduplication
            // The event_json is the full relay message, we need to parse the event
            if let Some(event_id) = extract_event_id(event_json) {
                // Check for duplicates
                {
                    let mut state_guard = match state.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    if state_guard.processed_events.contains(&event_id) {
                        return;
                    }
                    state_guard.processed_events.insert(event_id.clone());

                    // Limit cache size
                    if state_guard.processed_events.len() > 10000 {
                        state_guard.processed_events.clear();
                    }
                }

                // Extract event details
                if let Some((kind, pubkey)) = extract_event_details(event_json) {
                    debug!(
                        "Received event kind={} id={} sub={}",
                        kind,
                        &event_id[..8.min(event_id.len())],
                        sub_id
                    );

                    // Notify Kotlin about the event
                    notify_nostr_event(event_json, kind, &pubkey, None);
                }
            }
        }
        enostr::RelayMessage::OK(result) => {
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

/// Extract event ID from event JSON
fn extract_event_id(event_json: &str) -> Option<String> {
    // Simple extraction - look for "id":"<hex>"
    let id_start = event_json.find("\"id\":\"")?;
    let start = id_start + 6;
    let end = start + 64; // Event IDs are 64 hex chars
    if event_json.len() >= end {
        Some(event_json[start..end].to_string())
    } else {
        None
    }
}

/// Extract event kind and pubkey from event JSON
fn extract_event_details(event_json: &str) -> Option<(i32, String)> {
    // Extract kind
    let kind_start = event_json.find("\"kind\":")?;
    let kind_value_start = kind_start + 7;
    let kind_end = event_json[kind_value_start..]
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| kind_value_start + i)?;
    let kind: i32 = event_json[kind_value_start..kind_end].parse().ok()?;

    // Extract pubkey
    let pubkey_start = event_json.find("\"pubkey\":\"")?;
    let start = pubkey_start + 10;
    let end = start + 64; // Pubkeys are 64 hex chars
    if event_json.len() >= end {
        Some((kind, event_json[start..end].to_string()))
    } else {
        None
    }
}

/// Notify Kotlin about a new Nostr event
fn notify_nostr_event(event_json: &str, kind: i32, author_pubkey: &str, author_name: Option<&str>) {
    #[cfg(target_os = "android")]
    {
        let callback_guard = match JAVA_CALLBACK.lock() {
            Ok(guard) => guard,
            Err(e) => {
                error!("Failed to lock JAVA_CALLBACK: {}", e);
                return;
            }
        };

        if let Some(ref callback) = *callback_guard {
            if let Ok(mut env) = callback.jvm.attach_current_thread() {
                let event_json_jstring = match env.new_string(event_json) {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let author_pubkey_jstring = match env.new_string(author_pubkey) {
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

                let _ = env.call_method(
                    &callback.service_obj,
                    "onNostrEvent",
                    "(Ljava/lang/String;ILjava/lang/String;Ljava/lang/String;)V",
                    &[
                        JValue::Object(&JObject::from(event_json_jstring)),
                        JValue::Int(kind),
                        JValue::Object(&JObject::from(author_pubkey_jstring)),
                        JValue::Object(&author_name_jstring),
                    ],
                );
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = (event_json, author_name); // Suppress unused warnings
        debug!(
            "Nostr event (non-Android): kind={}, author={}",
            kind,
            &author_pubkey[..8.min(author_pubkey.len())]
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

        if let Some(ref callback) = *callback_guard {
            if let Ok(mut env) = callback.jvm.attach_current_thread() {
                let _ = env.call_method(
                    &callback.service_obj,
                    "onRelayStatusChanged",
                    "(I)V",
                    &[JValue::Int(connected_count)],
                );
            }
        }
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
) {
    // Always refresh the callback reference on each start
    // This ensures we have a valid reference even after service restart
    if let Ok(jvm) = env.get_java_vm() {
        if let Ok(global_ref) = env.new_global_ref(obj) {
            match JAVA_CALLBACK.lock() {
                Ok(mut guard) => {
                    *guard = Some(JavaCallback {
                        jvm,
                        service_obj: global_ref,
                    });
                    info!("JNI callback reference updated");
                }
                Err(e) => {
                    error!("Failed to lock JAVA_CALLBACK for update: {}", e);
                }
            }
        }
    }

    let pubkey: String = match env.get_string(&pubkey_hex) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get pubkey string: {}", e);
            return;
        }
    };

    if let Err(e) = start_subscriptions(&pubkey) {
        error!("Failed to start subscriptions: {}", e);
    }
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
    fn test_extract_event_id() {
        let json = r#"["EVENT","sub",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","kind":1}]"#;
        let id = extract_event_id(json);
        assert_eq!(
            id,
            Some("abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234".to_string())
        );
    }

    #[test]
    fn test_extract_event_details() {
        let json = r#"{"id":"abc","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"test"}"#;
        let details = extract_event_details(json);
        assert!(details.is_some());
        let (kind, pubkey) = details.unwrap();
        assert_eq!(kind, 1);
        assert_eq!(
            pubkey,
            "def0123456789012345678901234567890123456789012345678901234567890"
        );
    }
}
