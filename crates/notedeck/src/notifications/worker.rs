//! Notification worker thread for displaying notifications.
//!
//! This module receives pre-processed notification data from the main event loop
//! via a channel and displays them using the platform-specific backend.
//!
//! The main event loop handles:
//! - Relay connections (via the existing RelayPool)
//! - Event filtering (checking if events mention monitored accounts)
//! - Profile lookups (via nostrdb)
//!
//! This worker just receives the ready-to-display NotificationData and shows it.

use super::backend::NotificationBackend;
use super::types::{NotificationAccount, NotificationData, WorkerState};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tracing::{debug, info};

/// Worker thread that receives notification data and displays it.
///
/// This is a simplified event loop that:
/// 1. Receives NotificationData from the main event loop via channel
/// 2. Deduplicates events by ID
/// 3. Displays notifications via the platform backend
///
/// The worker no longer maintains relay connections or fetches profiles -
/// that's all handled by the main event loop using existing infrastructure.
///
/// # Arguments
/// * `running` - Shared flag to signal when the worker should stop
/// * `accounts` - Map of pubkey hex -> account for reference
/// * `event_receiver` - Channel to receive NotificationData from main loop
/// * `backend` - Notification backend for delivering notifications
#[profiling::function]
pub fn notification_worker<B: NotificationBackend>(
    running: Arc<AtomicBool>,
    accounts: HashMap<String, NotificationAccount>,
    event_receiver: mpsc::Receiver<NotificationData>,
    backend: B,
) {
    info!(
        "Notification worker started with {} accounts",
        accounts.len()
    );

    // Prevent App Nap on macOS so notifications work in background
    super::desktop::disable_app_nap();

    // Create worker state with the event receiver
    let mut state = WorkerState::new(accounts, event_receiver);

    let mut loop_count: u64 = 0;

    // Main event loop - receive notifications from channel
    while running.load(Ordering::SeqCst) {
        loop_count += 1;

        // Log heartbeat every 60 iterations (~60 seconds with 1s timeout)
        if loop_count.is_multiple_of(60) {
            info!("Notification worker heartbeat: loop={}", loop_count);
        }

        // Try to receive notification data with timeout
        // Timeout allows periodic checking of the running flag
        match state.event_receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(notification_data) => {
                process_notification(&mut state, notification_data, &backend);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No notification, just loop and check running flag
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                info!("Notification channel disconnected, stopping worker");
                break;
            }
        }
    }

    info!("Notification worker stopped");
}

/// Process a notification and display it via the backend.
#[profiling::function]
fn process_notification<B: NotificationBackend>(
    state: &mut WorkerState,
    data: NotificationData,
    backend: &B,
) {
    // Check for duplicate events
    if !record_event_if_new(state, &data.event.id) {
        debug!(
            "Skipping duplicate event id={}",
            &data.event.id[..8.min(data.event.id.len())]
        );
        return;
    }

    let event_id_preview = &data.event.id[..8.min(data.event.id.len())];
    let pubkey_preview = &data.event.pubkey[..8.min(data.event.pubkey.len())];
    let target_preview = &data.target_pubkey_hex[..8.min(data.target_pubkey_hex.len())];

    info!(
        "Displaying notification: kind={} id={} from={} target={}",
        data.event.kind, event_id_preview, pubkey_preview, target_preview
    );

    // On macOS, download and cache the profile picture locally
    // (UNNotificationAttachment only accepts local file URLs)
    #[cfg(target_os = "macos")]
    let picture_path: Option<String> = {
        if let Some(ref url) = data.author_picture_url {
            if let Some(ref cache) = state.image_cache {
                cache
                    .fetch_and_cache_blocking(url)
                    .map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            }
        } else {
            None
        }
    };

    #[cfg(not(target_os = "macos"))]
    let picture_path: Option<String> = data.author_picture_url.clone();

    // Display the notification with pre-formatted title/body
    backend.send_notification(
        &data.title,
        &data.body,
        &data.event,
        &data.target_pubkey_hex,
        picture_path.as_deref(),
    );
}

/// Record an event ID if not already seen. Returns true if the event is new.
/// Maximum number of event IDs to track for deduplication.
/// When exceeded, oldest entries are evicted to maintain bounded memory usage.
const MAX_PROCESSED_EVENTS: usize = 10_000;

fn record_event_if_new(state: &mut WorkerState, event_id: &str) -> bool {
    if state.processed_events.contains(event_id) {
        return false;
    }

    // Add to both the set (for O(1) lookups) and queue (for insertion order)
    let event_id_owned = event_id.to_string();
    state.processed_events.insert(event_id_owned.clone());
    state.processed_events_order.push_back(event_id_owned);

    // Bounded eviction: remove oldest entries until size <= MAX_PROCESSED_EVENTS
    while state.processed_events.len() > MAX_PROCESSED_EVENTS {
        if let Some(oldest) = state.processed_events_order.pop_front() {
            state.processed_events.remove(&oldest);
        } else {
            // Queue is empty but set is not - shouldn't happen, but clear set to recover
            state.processed_events.clear();
            break;
        }
    }

    true
}
