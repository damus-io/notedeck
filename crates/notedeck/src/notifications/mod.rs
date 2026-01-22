//! Cross-platform notification system for Nostr events.
//!
//! This module provides a platform-agnostic notification system that works
//! on Android (via JNI to Kotlin) and desktop (via notify-rust).
//!
//! # Architecture
//!
//! The notification system receives events from the main event loop via a channel.
//! This avoids duplicating the RelayPool and uses the existing nostrdb for profiles.
//!
//! **Main Event Loop** (in app.rs):
//! 1. Receives events from RelayPool
//! 2. Checks if event is notification-relevant (mentions monitored account)
//! 3. Looks up author profile from nostrdb
//! 4. Sends `NotificationData` to notification channel
//!
//! **Notification Worker** (this module):
//! 1. Receives `NotificationData` from channel
//! 2. Deduplicates by event ID
//! 3. Displays via platform-specific backend
//!
//! # Components
//!
//! - **Types** (`types.rs`): Core data structures
//! - **Backend** (`backend.rs`): Platform notification delivery trait
//! - **Worker** (`worker.rs`): Background thread that displays notifications
//! - **Desktop** (`desktop.rs`): Linux backend using notify-rust
//! - **macOS** (`macos.rs`): macOS backend using UNUserNotificationCenter
//!
//! # Platform Support
//!
//! - **macOS**: Full support with App Nap prevention and profile pictures
//! - **Linux**: Full support via libnotify
//! - **Android**: Uses JNI backend (implemented in notedeck_chrome)

mod backend;
mod bolt11;
mod desktop;
mod extraction;
pub mod image_cache;
#[cfg(target_os = "macos")]
mod macos;
mod manager;
mod profiles;
mod types;
mod worker;

pub use backend::{LoggingBackend, NoopBackend, NotificationBackend};
pub use desktop::DesktopBackend;
#[cfg(target_os = "macos")]
pub use macos::{initialize_on_main_thread as macos_init, MacOSBackend};
pub use manager::NotificationManager;
pub use profiles::{decode_npub, extract_mentioned_pubkeys, resolve_mentions};
pub use types::{
    is_notification_kind, CachedProfile, ExtractedEvent, NotificationAccount, NotificationData,
    WorkerState, NOTIFICATION_KINDS,
};

/// Type alias for the platform-appropriate notification backend.
#[cfg(target_os = "macos")]
pub type PlatformBackend = MacOSBackend;

#[cfg(not(target_os = "macos"))]
pub type PlatformBackend = DesktopBackend;

use enostr::Pubkey;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread::{self, JoinHandle};
use tracing::info;

/// Handle to the worker thread for lifecycle management.
struct WorkerHandle {
    /// Flag to signal worker thread to stop
    running: Arc<AtomicBool>,
    /// Handle to the worker thread (Option to allow taking for join)
    thread: Option<JoinHandle<()>>,
    /// Channel sender for notification data
    sender: mpsc::Sender<NotificationData>,
}

/// Notification service that manages the background worker thread.
///
/// Events are sent to the worker via a channel from the main event loop.
/// The worker just displays them - no relay connections or profile fetching.
pub struct NotificationService<B: NotificationBackend + 'static> {
    /// Worker thread handle
    worker: RwLock<Option<WorkerHandle>>,
    /// Marker for the backend type
    _phantom: PhantomData<fn() -> B>,
}

impl<B: NotificationBackend + 'static> NotificationService<B> {
    /// Create a new notification service.
    pub fn new() -> Self {
        Self {
            worker: RwLock::new(None),
            _phantom: PhantomData,
        }
    }

    /// Start the notification worker for the given accounts.
    ///
    /// The backend is constructed inside the worker thread using the provided factory.
    ///
    /// # Arguments
    /// * `backend_factory` - Factory function to create the backend
    /// * `pubkey_hexes` - Hex-encoded pubkeys of accounts to monitor
    pub fn start(
        &self,
        backend_factory: impl FnOnce() -> B + Send + 'static,
        pubkey_hexes: &[impl AsRef<str>],
    ) -> Result<(), String> {
        // Parse and validate all pubkeys
        let mut accounts = HashMap::new();
        for pubkey_hex in pubkey_hexes {
            let pubkey_hex = pubkey_hex.as_ref();
            let pubkey = Pubkey::from_hex(pubkey_hex)
                .map_err(|e| format!("Invalid pubkey {}: {e}", pubkey_hex))?;
            let account = NotificationAccount::new(pubkey);
            accounts.insert(account.pubkey_hex.clone(), account);
        }

        if accounts.is_empty() {
            return Err("No valid pubkeys provided".to_string());
        }

        let mut guard = self
            .worker
            .write()
            .map_err(|e| format!("Lock error: {e}"))?;

        // Stop any previous worker
        if let Some(mut old_handle) = guard.take() {
            old_handle.running.store(false, Ordering::SeqCst);
            if let Some(thread) = old_handle.thread.take() {
                let _ = thread.join();
            }
        }

        // Create channel for notification data
        let (sender, receiver) = mpsc::channel();

        // Create running flag
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let account_count = accounts.len();

        // Spawn worker thread
        let thread = thread::spawn(move || {
            let backend = backend_factory();
            worker::notification_worker(running_clone, accounts, receiver, backend);
        });

        *guard = Some(WorkerHandle {
            running,
            thread: Some(thread),
            sender,
        });

        info!("Started notification worker for {} accounts", account_count);
        Ok(())
    }

    /// Send notification data to the worker for display.
    ///
    /// Call this from the main event loop when a notification-relevant event is received.
    pub fn send(&self, data: NotificationData) -> Result<(), String> {
        let guard = self.worker.read().map_err(|e| format!("Lock error: {e}"))?;
        if let Some(ref handle) = *guard {
            handle
                .sender
                .send(data)
                .map_err(|e| format!("Channel send error: {e}"))?;
            Ok(())
        } else {
            Err("Notification service not running".to_string())
        }
    }

    /// Stop the notification worker.
    pub fn stop(&self) {
        let handle = {
            if let Ok(mut guard) = self.worker.write() {
                guard.take()
            } else {
                return;
            }
        };

        if let Some(mut handle) = handle {
            handle.running.store(false, Ordering::SeqCst);
            info!("Signaled notification worker to stop");

            if let Some(thread) = handle.thread.take() {
                let start = std::time::Instant::now();
                while !thread.is_finished() && start.elapsed().as_secs() < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }

                if thread.is_finished() {
                    let _ = thread.join();
                    info!("Notification worker stopped");
                } else {
                    info!("Notification worker didn't stop in time, detaching");
                }
            }
        }
    }

    /// Check if the notification worker is running.
    pub fn is_running(&self) -> bool {
        self.worker
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|h| h.running.load(Ordering::SeqCst)))
            .unwrap_or(false)
    }
}

impl<B: NotificationBackend + 'static> Default for NotificationService<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: NotificationBackend + 'static> Drop for NotificationService<B> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start notification service using the desktop backend.
#[cfg(not(target_os = "android"))]
pub fn start_desktop_notifications(
    pubkey_hexes: &[impl AsRef<str>],
) -> Result<NotificationService<PlatformBackend>, String> {
    let service = NotificationService::new();
    service.start(PlatformBackend::new, pubkey_hexes)?;
    Ok(service)
}

// =============================================================================
// Main Event Loop Integration
// =============================================================================

use nostrdb::{Ndb, Transaction};
use tracing::debug;

/// Process a relay message and forward to notifications if relevant.
///
/// Call this from the main event loop's relay message handler.
/// This function:
/// 1. Extracts event data from the relay message
/// 2. Checks if the event kind is notification-relevant
/// 3. Checks if any monitored account is mentioned in p-tags
/// 4. Looks up author profile from nostrdb
/// 5. Sends NotificationData to the notification worker
///
/// # Arguments
/// * `relay_message` - Raw JSON relay message (["EVENT", "sub_id", {...}])
/// * `ndb` - Reference to nostrdb for profile lookups
/// * `manager` - Reference to the notification manager
/// * `monitored_pubkeys` - List of pubkey hex strings to monitor for mentions
#[cfg(not(target_os = "android"))]
#[profiling::function]
pub fn process_relay_message_for_notifications(
    relay_message: &str,
    ndb: &Ndb,
    manager: &Option<NotificationManager>,
    monitored_pubkeys: &[String],
) {
    // Skip if notifications aren't running
    let Some(mgr) = manager.as_ref() else {
        return;
    };
    if !mgr.is_running() {
        return;
    }

    // Extract event from relay message
    let Some(event) = extraction::extract_event(relay_message) else {
        return;
    };

    // Check if event kind is notification-relevant
    if !is_notification_kind(event.kind) {
        return;
    }

    // Find which monitored account(s) are mentioned in p-tags
    let target_pubkey_hex = event
        .p_tags
        .iter()
        .find(|p| monitored_pubkeys.contains(p))
        .cloned();

    let Some(target_pubkey_hex) = target_pubkey_hex else {
        return; // Event doesn't mention any monitored account
    };

    // Don't notify for our own events
    if monitored_pubkeys.contains(&event.pubkey) {
        return;
    }

    debug!(
        "Notification-relevant event: kind={} id={} target={}",
        event.kind,
        &event.id[..8.min(event.id.len())],
        &target_pubkey_hex[..8.min(target_pubkey_hex.len())]
    );

    // Look up author profile from nostrdb
    let (author_name, author_picture_url) = lookup_profile(ndb, &event.pubkey);

    // Create notification data
    let notification_data = NotificationData {
        event,
        author_name,
        author_picture_url,
        target_pubkey_hex,
    };

    // Send to worker
    if let Err(e) = mgr.send(notification_data) {
        debug!("Failed to send notification: {}", e);
    }
}

/// Look up a profile from nostrdb.
///
/// Returns (display_name, picture_url) if found.
#[cfg(not(target_os = "android"))]
fn lookup_profile(ndb: &Ndb, pubkey_hex: &str) -> (Option<String>, Option<String>) {
    let Ok(pubkey_bytes) = hex::decode(pubkey_hex) else {
        return (None, None);
    };
    if pubkey_bytes.len() != 32 {
        return (None, None);
    }

    let Ok(txn) = Transaction::new(ndb) else {
        return (None, None);
    };

    let pubkey_arr: [u8; 32] = pubkey_bytes.try_into().unwrap();
    let Ok(profile) = ndb.get_profile_by_pubkey(&txn, &pubkey_arr) else {
        return (None, None);
    };

    let record = profile.record();
    let name = record
        .profile()
        .and_then(|p| p.display_name().or_else(|| p.name()))
        .map(|s| s.to_string());

    let picture = record
        .profile()
        .and_then(|p| p.picture())
        .map(|s| s.to_string());

    (name, picture)
}

/// Re-export extraction for use in tests
pub use extraction::extract_event;
