//! Notification worker thread for maintaining relay connections.
//!
//! This module contains the main event loop for the notification service.
//! It manages relay connections, processes incoming events, and delivers
//! notifications via the configured backend.

use super::backend::NotificationBackend;
use super::extraction::extract_event;
use super::profiles::{
    extract_mentioned_pubkeys, handle_profile_event, request_profile_if_needed, resolve_mentions,
};
use super::types::{ExtractedEvent, NotificationAccount, WorkerState, NOTIFICATION_KINDS};
use enostr::RelayStatus;
use nostrdb::Filter;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tracing::{debug, error, info, warn};

/// Subscription IDs for different notification types.
const SUB_NOTIFICATIONS: &str = "notedeck_notifications";
const SUB_DMS: &str = "notedeck_dms";
const SUB_RELAY_LIST: &str = "notedeck_relay_list";
const SUB_PROFILES: &str = "notedeck_profiles";

/// Worker thread that owns all non-Send state and handles relay I/O.
///
/// This is the main event loop for the notification service. It:
/// 1. Creates relay connections and subscriptions
/// 2. Polls for events at 1-second intervals
/// 3. Processes incoming Nostr events and sends them via the backend
/// 4. Updates the backend with connected relay count for status display
///
/// # Arguments
/// * `running` - Shared flag to signal when the worker should stop
/// * `accounts` - Map of pubkey hex -> account for O(1) lookups when attributing events
/// * `relay_urls` - List of relay URLs to connect to
/// * `backend` - Notification backend for delivering notifications (owned, constructed in worker thread)
#[profiling::function]
pub fn notification_worker<B: NotificationBackend>(
    running: Arc<AtomicBool>,
    accounts: HashMap<String, NotificationAccount>,
    relay_urls: Vec<String>,
    backend: B,
) {
    info!(
        "Notification worker thread started with {} accounts",
        accounts.len()
    );

    // Prevent App Nap on macOS
    super::desktop::disable_app_nap();

    // Create all state inside the worker thread
    let mut state = WorkerState::new(accounts, relay_urls);

    // Set up initial subscriptions
    setup_subscriptions(&mut state);

    let mut loop_count: u64 = 0;
    let mut last_connected_count: i32 = -1;

    // Main event loop
    while running.load(Ordering::SeqCst) {
        loop_count += 1;

        // Count connected relays
        let connected = count_connected_relays(&state.pool);

        // Log heartbeat every 30 iterations (~30 seconds)
        if loop_count.is_multiple_of(30) {
            info!(
                "Worker heartbeat: loop={}, connected={} relays",
                loop_count, connected
            );
        }

        // Send keepalive pings
        state.pool.keepalive_ping(|| {});

        // Update backend when connected count changes
        if connected != last_connected_count {
            last_connected_count = connected;
            backend.on_relay_status_changed(connected);
        }

        // Poll for events
        match state.pool.try_recv() {
            Some(pool_event) => {
                let event = pool_event.into_owned();
                handle_pool_event(&mut state, event, &backend);
            }
            None => {
                // 1 second idle sleep balances battery life vs notification latency
                thread::sleep(std::time::Duration::from_secs(1));
            }
        }

        // Process any events that were buffered during profile fetch wait loops
        process_pending_events(&mut state, &backend);
    }

    // Cleanup subscriptions
    state.pool.unsubscribe(SUB_NOTIFICATIONS.to_string());
    state.pool.unsubscribe(SUB_DMS.to_string());
    state.pool.unsubscribe(SUB_RELAY_LIST.to_string());
    state.pool.unsubscribe(SUB_PROFILES.to_string());

    info!("Notification worker thread stopped");
}

/// Configure Nostr subscriptions for notifications, DMs, and relay lists.
///
/// Sets up filters for mentions, reactions, reposts, zaps, and direct messages.
/// Subscribes to events targeting ANY of the configured accounts using a combined
/// p-tag filter. Event attribution to specific accounts happens in handle_event_message().
fn setup_subscriptions(state: &mut WorkerState) {
    // Clone pubkey bytes to owned data to avoid borrow conflict with pool.subscribe()
    let pubkey_bytes: Vec<[u8; 32]> = state.account_pubkey_bytes().iter().map(|b| **b).collect();

    if pubkey_bytes.is_empty() {
        warn!("No accounts configured, skipping subscription setup");
        return;
    }

    let num_accounts = pubkey_bytes.len();

    // Use current timestamp to only receive new events (not historical)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Build all filters first (before any mutable borrows of state.pool)
    // Subscribe to mentions, replies, reactions, reposts, zaps
    // kinds: 1 (text), 6 (repost), 7 (reaction), 9735 (zap receipt)
    // Uses p-tag filter (.pubkey) to match events referencing any of our accounts
    let notification_filter = Filter::new()
        .kinds([1, 6, 7, 9735])
        .pubkey(pubkey_bytes.iter())
        .since(now)
        .build();

    // Subscribe to DMs (kind 4 legacy, kind 1059 gift wrap)
    let dm_filter = Filter::new()
        .kinds([4, 1059])
        .pubkey(pubkey_bytes.iter())
        .since(now)
        .build();

    // Subscribe to relay list updates (NIP-65) for all accounts
    let relay_list_filter = Filter::new()
        .kinds([10002, 10050])
        .authors(pubkey_bytes.iter())
        .limit(num_accounts as u64)
        .build();

    info!(
        "Subscribing to notifications for {} accounts since timestamp {}",
        num_accounts, now
    );

    // Now do all the subscribe calls
    state
        .pool
        .subscribe(SUB_NOTIFICATIONS.to_string(), vec![notification_filter]);
    state.pool.subscribe(SUB_DMS.to_string(), vec![dm_filter]);
    state
        .pool
        .subscribe(SUB_RELAY_LIST.to_string(), vec![relay_list_filter]);

    debug!(
        "Set up notification subscriptions for {} accounts",
        num_accounts
    );
}

/// Handle a WebSocket event from the relay pool.
///
/// Dispatches to appropriate handlers for connection state changes and messages.
#[profiling::function]
fn handle_pool_event<B: NotificationBackend>(
    state: &mut WorkerState,
    pool_event: enostr::PoolEventBuf,
    backend: &B,
) {
    use enostr::ewebsock::{WsEvent, WsMessage};

    match pool_event.event {
        WsEvent::Opened => {
            debug!("Connected to relay: {}", pool_event.relay);
            // Re-subscribe on reconnect
            setup_subscriptions(state);
        }
        WsEvent::Closed => {
            debug!("Disconnected from relay: {}", pool_event.relay);
        }
        WsEvent::Error(ref err) => {
            error!("Relay error {}: {:?}", pool_event.relay, err);
        }
        WsEvent::Message(WsMessage::Text(ref text)) => {
            handle_relay_message(state, text, backend);
        }
        WsEvent::Message(_) => {}
    }
}

/// Count the number of currently connected relays in the pool.
fn count_connected_relays(pool: &enostr::RelayPool) -> i32 {
    pool.relays
        .iter()
        .filter(|r| matches!(r.status(), RelayStatus::Connected))
        .count() as i32
}

/// Process an incoming Nostr relay message.
///
/// Handles EVENT messages by extracting event data, deduplicating, and notifying.
fn handle_relay_message<B: NotificationBackend>(
    state: &mut WorkerState,
    message: &str,
    backend: &B,
) {
    // Parse the relay message using enostr's parser
    let relay_msg = match enostr::RelayMessage::from_json(message) {
        Ok(msg) => msg,
        Err(e) => {
            debug!("Could not parse relay message: {}", e);
            return;
        }
    };

    match relay_msg {
        enostr::RelayMessage::Event(sub_id, event_json) => {
            handle_event_message(state, sub_id, event_json, backend);
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

/// Process events that were buffered during profile fetch wait loops.
fn process_pending_events<B: NotificationBackend>(state: &mut WorkerState, backend: &B) {
    if state.pending_events.is_empty() {
        return;
    }

    // Drain pending events to process them
    let events: Vec<String> = state.pending_events.drain(..).collect();
    debug!("Processing {} buffered events", events.len());

    for event_json in events {
        handle_event_message(state, "buffered", &event_json, backend);
    }
}

/// Handle an EVENT message from a relay.
///
/// Extracts event data, checks for duplicates, and notifies via backend if new.
#[profiling::function]
fn handle_event_message<B: NotificationBackend>(
    state: &mut WorkerState,
    sub_id: &str,
    event_json: &str,
    backend: &B,
) {
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
    if !NOTIFICATION_KINDS.contains(&event.kind) {
        debug!(
            "Ignoring event kind {} (not a notification kind)",
            event.kind
        );
        return;
    }

    // Determine which of our accounts this event targets
    let target_account = match get_target_account(state, &event) {
        Some(account) => account.pubkey_hex.clone(),
        None => {
            debug!(
                "Event id={} doesn't target any of our {} accounts, skipping",
                &event.id[..8],
                state.accounts.len()
            );
            return;
        }
    };

    if !record_event_if_new(state, &event.id) {
        debug!("Skipping duplicate event id={}", &event.id[..8]);
        return;
    }

    info!(
        "NEW EVENT: kind={} id={} from={} target={} sub={}",
        event.kind,
        &event.id[..8],
        &event.pubkey[..8],
        &target_account[..8],
        sub_id
    );

    // Look up author profile from cache, request if not cached
    let mut profile = state.profile_cache.get(&event.pubkey).cloned();
    if profile.is_none() {
        request_profile_if_needed(state, &event.pubkey);

        // Brief wait for profile to arrive - poll relay messages
        for _ in 0..20 {
            while let Some(ev) = state.pool.try_recv() {
                if let enostr::ewebsock::WsEvent::Message(enostr::ewebsock::WsMessage::Text(
                    ref text,
                )) = ev.event
                {
                    if let Ok(enostr::RelayMessage::Event(_sub, inner_event_json)) =
                        enostr::RelayMessage::from_json(text)
                    {
                        if let Some(evt) = extract_event(inner_event_json) {
                            if evt.kind == 0 {
                                handle_profile_event(state, &evt);
                            } else {
                                state.pending_events.push(inner_event_json.to_string());
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

            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    let author_name = profile.as_ref().and_then(|p| p.name.clone());
    let picture_url = profile.as_ref().and_then(|p| p.picture_url.clone());

    // Request profiles for mentioned users, then resolve mentions
    let resolved_content = {
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
                        if let Ok(enostr::RelayMessage::Event(_sub, inner_event_json)) =
                            enostr::RelayMessage::from_json(text)
                        {
                            if let Some(evt) = extract_event(inner_event_json) {
                                if evt.kind == 0 {
                                    handle_profile_event(state, &evt);
                                } else {
                                    state.pending_events.push(inner_event_json.to_string());
                                }
                            }
                        }
                    }
                }

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

    info!(
        "Notifying with profile: name={:?}, picture={:?}",
        author_name,
        picture_url.as_ref().map(|s| &s[..s.len().min(50)])
    );

    // Create event with resolved content for notification
    let resolved_event = ExtractedEvent {
        id: event.id.clone(),
        kind: event.kind,
        pubkey: event.pubkey.clone(),
        content: resolved_content,
        p_tags: event.p_tags.clone(),
        zap_amount_sats: event.zap_amount_sats,
        raw_json: event.raw_json.clone(),
    };

    backend.send_notification(
        &resolved_event,
        &target_account,
        author_name.as_deref(),
        picture_url.as_deref(),
    );
}

/// Determine which of our accounts an event targets based on kind and p-tags.
fn get_target_account<'a>(
    state: &'a WorkerState,
    event: &ExtractedEvent,
) -> Option<&'a NotificationAccount> {
    // For DMs (kind 4, 1059), the p-tag indicates the recipient
    // For other kinds, p-tags indicate who the event is about/for
    for p_tag in &event.p_tags {
        if let Some(account) = state.accounts.get(p_tag) {
            return Some(account);
        }
    }

    // Fallback: if event author is one of our accounts (e.g., for relay list updates)
    state.accounts.get(&event.pubkey)
}

/// Record an event ID if not already seen. Returns true if the event is new.
fn record_event_if_new(state: &mut WorkerState, event_id: &str) -> bool {
    if state.processed_events.contains(event_id) {
        return false;
    }

    state.processed_events.insert(event_id.to_string());

    // Prune cache when it exceeds 10,000 entries
    if state.processed_events.len() > 10000 {
        state.processed_events.clear();
    }

    true
}
