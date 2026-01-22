//! Cross-platform notification system for Nostr events.
//!
//! This module provides a platform-agnostic notification system that works
//! on Android (via JNI to Kotlin) and desktop (via notify-rust).
//!
//! # Architecture
//!
//! The notification system is built around these core components:
//!
//! - **Types** (`types.rs`): Core data structures like `ExtractedEvent`, `CachedProfile`,
//!   `NotificationAccount`, and `WorkerState`.
//!
//! - **Backend trait** (`backend.rs`): The `NotificationBackend` trait abstracts over
//!   platform-specific notification delivery.
//!
//! - **Worker** (`worker.rs`): The main notification worker thread that maintains
//!   relay connections, processes events, and delivers notifications.
//!
//! - **Profiles** (`profiles.rs`): Profile caching and mention resolution.
//!
//! - **Desktop** (`desktop.rs`): Desktop-specific backend using notify-rust.
//!
//! # Usage
//!
//! ```ignore
//! use notedeck::notifications::{
//!     NotificationService, NotificationBackend, DesktopBackend,
//! };
//!
//! // Create a desktop backend
//! let backend = Arc::new(DesktopBackend::new("Notedeck"));
//!
//! // Create and start the notification service
//! let service = NotificationService::new(backend);
//! service.start(&["<pubkey_hex>"], &["wss://relay.damus.io"])?;
//!
//! // Later, stop the service
//! service.stop();
//! ```
//!
//! # Platform Support
//!
//! - **macOS**: Full support with App Nap prevention
//! - **Linux**: Full support via libnotify
//! - **Android**: Uses JNI backend (implemented in notedeck_chrome)
//! - **Windows**: Partial support via notify-rust (notifications work, no action buttons)

mod backend;
mod bolt11;
mod desktop;
mod extraction;
mod profiles;
mod types;
mod worker;

pub use backend::{LoggingBackend, NoopBackend, NotificationBackend};
pub use desktop::DesktopBackend;
pub use types::{CachedProfile, ExtractedEvent, NotificationAccount, WorkerState, DEFAULT_RELAYS};

use enostr::Pubkey;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use tracing::info;

/// Handle to the worker thread for lifecycle management.
struct WorkerHandle {
    /// Flag to signal worker thread to stop
    running: Arc<AtomicBool>,
    /// Handle to the worker thread (Option to allow taking for join)
    thread: Option<JoinHandle<()>>,
}

/// Notification service that manages the background worker thread.
///
/// This is the main entry point for the notification system. It manages
/// the lifecycle of the worker thread and provides methods to start/stop
/// notifications.
pub struct NotificationService<B: NotificationBackend + 'static> {
    /// The notification backend
    backend: Arc<B>,
    /// Worker thread handle
    worker: RwLock<Option<WorkerHandle>>,
}

impl<B: NotificationBackend + 'static> NotificationService<B> {
    /// Create a new notification service with the given backend.
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            worker: RwLock::new(None),
        }
    }

    /// Start notification subscriptions for multiple accounts.
    ///
    /// If relay_urls is empty, falls back to DEFAULT_RELAYS.
    ///
    /// # Arguments
    /// * `pubkey_hexes` - Hex-encoded pubkeys of accounts to monitor
    /// * `relay_urls` - List of relay URLs to connect to
    pub fn start(
        &self,
        pubkey_hexes: &[impl AsRef<str>],
        relay_urls: &[String],
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

        // Acquire write lock for the entire check-stop-start sequence to avoid TOCTOU race
        let mut guard = self
            .worker
            .write()
            .map_err(|e| format!("Lock error: {e}"))?;

        // Check if already running and stop if so
        if let Some(ref handle) = *guard {
            if handle.running.load(Ordering::SeqCst) {
                info!("Notification subscriptions already running, restarting");
            }
        }

        // Stop any previous worker (while still holding the write lock)
        if let Some(mut old_handle) = guard.take() {
            old_handle.running.store(false, Ordering::SeqCst);
            if let Some(thread) = old_handle.thread.take() {
                // Wait with timeout to avoid blocking forever
                let _ = thread.join();
            }
        }

        // Create running flag for new worker
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let relay_urls_owned = relay_urls.to_vec();
        let account_count = accounts.len();
        let backend = self.backend.clone();

        // Spawn worker thread
        let thread = thread::spawn(move || {
            worker::notification_worker(running_clone, accounts, relay_urls_owned, backend);
        });

        // Store handle (we already hold the write lock)
        *guard = Some(WorkerHandle {
            running,
            thread: Some(thread),
        });

        info!(
            "Started notification subscriptions for {} accounts",
            account_count
        );
        Ok(())
    }

    /// Stop notification subscriptions and wait for worker to finish.
    pub fn stop(&self) {
        let handle = {
            if let Ok(mut guard) = self.worker.write() {
                guard.take()
            } else {
                return;
            }
        };

        if let Some(mut handle) = handle {
            // Signal worker to stop
            handle.running.store(false, Ordering::SeqCst);
            info!("Signaled notification subscriptions to stop");

            // Wait for worker thread to finish (with timeout)
            if let Some(thread) = handle.thread.take() {
                // Give worker time to finish (max 2 seconds)
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

    /// Check if notification subscriptions are currently running.
    pub fn is_running(&self) -> bool {
        self.worker
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|h| h.running.load(Ordering::SeqCst)))
            .unwrap_or(false)
    }

    /// Get a reference to the backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }
}

impl<B: NotificationBackend + 'static> Drop for NotificationService<B> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start notification subscriptions using the desktop backend.
///
/// Convenience function for quick setup on desktop platforms.
///
/// # Arguments
/// * `pubkey_hexes` - Hex-encoded pubkeys of accounts to monitor
/// * `relay_urls` - List of relay URLs to connect to (empty = use defaults)
///
/// # Returns
/// A NotificationService that will stop when dropped.
#[cfg(not(target_os = "android"))]
pub fn start_desktop_notifications(
    pubkey_hexes: &[impl AsRef<str>],
    relay_urls: &[String],
) -> Result<NotificationService<DesktopBackend>, String> {
    let backend = Arc::new(DesktopBackend::new("Notedeck"));
    let service = NotificationService::new(backend);
    service.start(pubkey_hexes, relay_urls)?;
    Ok(service)
}
