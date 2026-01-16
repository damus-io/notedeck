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
            resubscribe_on_reconnect(state);
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
        WsEvent::Message(ewebsock::WsMessage::Text(ref text)) => {
            handle_relay_message(state, text);
        }
        WsEvent::Message(_) => {}
    }
}

fn resubscribe_on_reconnect(state: &Arc<Mutex<NotificationState>>) {
    let mut state_guard = match state.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let pubkey = match state_guard.pubkey.clone() {
        Some(p) => p,
        None => return,
    };
    setup_subscriptions(&mut state_guard.pool, &pubkey);
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
            // Extract all event fields using proper JSON parsing
            let event = match extract_event(event_json) {
                Some(e) => e,
                None => {
                    debug!("Failed to extract event from JSON");
                    return;
                }
            };

            // Check for duplicates using the extracted event ID
            {
                let mut state_guard = match state.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if state_guard.processed_events.contains(&event.id) {
                    return;
                }
                state_guard.processed_events.insert(event.id.clone());

                // Limit cache size
                if state_guard.processed_events.len() > 10000 {
                    state_guard.processed_events.clear();
                }
            }

            debug!(
                "Received event kind={} id={} sub={}",
                event.kind,
                &event.id[..8],
                sub_id
            );

            // Notify Kotlin about the event with structured data
            notify_nostr_event(&event, None);
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
fn extract_event(event_json: &str) -> Option<ExtractedEvent> {
    // Use serde_json for robust parsing that handles escaped strings correctly
    let value: serde_json::Value = match serde_json::from_str(event_json) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse event JSON: {}", e);
            return None;
        }
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            warn!("Event JSON is not an object");
            return None;
        }
    };

    let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let kind = obj.get("kind").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let pubkey = obj.get("pubkey").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();

    // Validate id and pubkey are proper hex (64 chars)
    // Log warning but don't silently drop - helps debug relay issues
    if id.len() != 64 {
        warn!("Dropping event with invalid id length {}: {}", id.len(), &id[..id.len().min(16)]);
        return None;
    }
    if pubkey.len() != 64 {
        warn!("Dropping event with invalid pubkey length {}: {}", pubkey.len(), &pubkey[..pubkey.len().min(16)]);
        return None;
    }

    // Extract zap amount for kind 9735 (zap receipt) events
    let zap_amount_sats = if kind == 9735 {
        extract_zap_amount(obj)
    } else {
        None
    };

    // Use original JSON from relay to preserve byte-level fidelity
    // (keeps field order, duplicate keys, exact formatting)
    let raw_json = event_json.to_string();

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

    // Convert to millisatoshis based on multiplier, then to satoshis
    let msats = match multiplier_char {
        Some('m') => amount * 100_000_000,     // milli-bitcoin = 0.001 BTC = 100,000 sats
        Some('u') => amount * 100_000,         // micro-bitcoin = 0.000001 BTC = 100 sats
        Some('n') => amount * 100,             // nano-bitcoin = 0.000000001 BTC = 0.1 sats
        Some('p') => amount / 10,              // pico-bitcoin = 0.000000000001 BTC
        None => amount * 100_000_000_000,      // whole bitcoin (rare in practice)
        _ => return None,
    };

    // Convert millisatoshis to satoshis
    Some(msats / 1000)
}

/// Notify Kotlin about a new Nostr event with structured data
/// This passes individual fields instead of raw JSON, eliminating JSON parsing in Kotlin
fn notify_nostr_event(event: &ExtractedEvent, author_name: Option<&str>) {
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
        let author_name_jstring = author_name
            .and_then(|name| env.new_string(name).ok())
            .map(JObject::from)
            .unwrap_or_else(JObject::null);
        let raw_json_jstring = match env.new_string(&event.raw_json) {
            Ok(s) => s,
            Err(_) => return,
        };

        let zap_amount = event.zap_amount_sats.unwrap_or(-1);

        let _ = env.call_method(
            &callback.service_obj,
            "onNostrEvent",
            "(Ljava/lang/String;ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;JLjava/lang/String;)V",
            &[
                JValue::Object(&JObject::from(event_id_jstring)),
                JValue::Int(event.kind),
                JValue::Object(&JObject::from(author_pubkey_jstring)),
                JValue::Object(&JObject::from(content_jstring)),
                JValue::Object(&author_name_jstring),
                JValue::Long(zap_amount),
                JValue::Object(&JObject::from(raw_json_jstring)),
            ],
        );
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = author_name;
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

    if let Err(e) = start_subscriptions(&pubkey) {
        error!("Failed to start subscriptions: {}", e);
    }
}

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
        let json = r#"{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"hello world"}"#;
        let event = extract_event(json);
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
        let json = r#"{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"json example: {\"foo\": \"bar\"}"}"#;
        let event = extract_event(json);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.content, r#"json example: {"foo": "bar"}"#);
    }

    #[test]
    fn test_extract_event_empty_content() {
        let json = r#"{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":7,"content":""}"#;
        let event = extract_event(json);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, 7);
        assert_eq!(event.content, "");
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
        let json = r#"{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":9735,"content":"","tags":[["bolt11","lnbc1000u1pj..."]]}"#;
        let event = extract_event(json);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, 9735);
        assert_eq!(event.zap_amount_sats, Some(100_000)); // 1000 micro-BTC = 100,000 sats
    }
}
