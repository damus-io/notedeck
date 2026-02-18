//! Cross-platform notification system for Nostr events.
//!
//! This module provides a platform-agnostic notification system that works
//! on Android (via JNI to Kotlin) and desktop (via notify-rust).
//!
//! # Architecture
//!
//! The notification system receives relay events via `NotificationManager`,
//! which extracts, filters, and resolves profiles before forwarding to the
//! worker thread over an `mpsc` channel.
//!
//! **NotificationManager** (`manager.rs`):
//! 1. Receives raw relay messages from the main event loop
//! 2. Extracts events, checks kind and p-tag mentions
//! 3. Looks up author profile via nostrdb
//! 4. Sends `NotificationData` to the worker channel
//!
//! **Worker Thread** (`worker.rs`):
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
mod profiles;
mod types;
mod worker;

pub use backend::{LoggingBackend, NoopBackend, NotificationBackend};
pub use desktop::DesktopBackend;
pub use profiles::{decode_npub, extract_mentioned_pubkeys, resolve_mentions};
pub use types::{
    is_notification_kind, CachedProfile, ExtractedEvent, NotificationAccount, NotificationData,
    WorkerState, NOTIFICATION_KINDS,
};

/// Type alias for the platform-appropriate notification backend.
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

/// Re-export extraction for use in tests
pub use extraction::extract_event;
