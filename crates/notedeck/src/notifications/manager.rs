//! Notification manager for owned notification state.
//!
//! Provides a simple interface for managing the notification service lifecycle
//! without using global statics. Designed to be owned by `Notedeck` and accessed
//! via `AppContext`.

use super::{NotificationService, PlatformBackend};
use tracing::{info, warn};

// =============================================================================
// macOS delegate wrapper (reduces unsafe surface area)
// =============================================================================

/// Wrapper for the macOS notification delegate with contained `Send + Sync` impls.
///
/// This type exists solely to hold the delegate alive via RAII. The delegate is:
/// - Created on the main thread during initialization
/// - Installed on `UNUserNotificationCenter`
/// - Never accessed from Rust after creation
///
/// # Safety
///
/// The `Send + Sync` impls are safe because:
/// - The inner `Retained<AnyObject>` is only kept alive, never dereferenced
/// - The Objective-C runtime manages all thread-safety for delegate callbacks
/// - We never mutate or read the delegate after initialization
#[cfg(target_os = "macos")]
struct MacOSDelegate(Option<objc2::rc::Retained<objc2::runtime::AnyObject>>);

#[cfg(target_os = "macos")]
impl MacOSDelegate {
    /// Initialize the macOS delegate on the main thread.
    fn new() -> Self {
        Self(super::macos::initialize_on_main_thread())
    }

    /// Returns true if initialization succeeded (valid bundle, delegate installed).
    fn is_initialized(&self) -> bool {
        self.0.is_some()
    }
}

// SAFETY: See struct-level documentation. The delegate is only kept alive for
// macOS callbacks, never accessed from Rust after initialization.
#[cfg(target_os = "macos")]
unsafe impl Send for MacOSDelegate {}
#[cfg(target_os = "macos")]
unsafe impl Sync for MacOSDelegate {}

// =============================================================================
// NotificationManager
// =============================================================================

/// Manages notification service lifecycle.
///
/// This struct owns the notification service and provides methods to
/// enable/disable notifications. It replaces the global statics in
/// `platform::desktop_notifications`.
///
/// # Lifecycle
///
/// On macOS, the manager holds the notification delegate for the app's lifetime.
/// If dropped while the app is running, foreground notifications will stop working.
///
/// # Usage
///
/// ```ignore
/// let mut manager = NotificationManager::new();
/// manager.start(&["pubkey_hex"], &relay_urls)?;
/// // ... later ...
/// manager.stop();
/// ```
pub struct NotificationManager {
    /// The underlying notification service, created on first start.
    service: Option<NotificationService<PlatformBackend>>,

    /// macOS notification delegate (must be kept alive for foreground notifications).
    #[cfg(target_os = "macos")]
    macos_delegate: MacOSDelegate,
}

impl NotificationManager {
    /// Create a new notification manager.
    ///
    /// On macOS, this initializes the notification delegate and requests permission.
    /// **This should be called on the main thread** for the permission dialog to work.
    ///
    /// The notification service is not started until `start()` is called.
    pub fn new() -> Self {
        // Catch main-thread misuse early in debug builds
        #[cfg(target_os = "macos")]
        debug_assert!(
            super::macos::is_main_thread(),
            "NotificationManager::new() should be called on the main thread on macOS"
        );

        #[cfg(target_os = "macos")]
        let macos_delegate = MacOSDelegate::new();

        Self {
            service: None,
            #[cfg(target_os = "macos")]
            macos_delegate,
        }
    }

    /// Check if macOS initialization succeeded.
    #[cfg(target_os = "macos")]
    pub fn is_macos_initialized(&self) -> bool {
        self.macos_delegate.is_initialized()
    }

    /// Start notification subscriptions for the given accounts.
    ///
    /// Creates a new notification service if one doesn't exist, or restarts
    /// the existing one with new parameters.
    ///
    /// # Arguments
    /// * `pubkey_hexes` - Hex-encoded pubkeys of accounts to monitor
    /// * `relay_urls` - List of relay URLs to connect to
    ///
    /// # Returns
    /// * `Ok(())` - Notifications started successfully
    /// * `Err(String)` - Error message if start failed
    pub fn start(
        &mut self,
        pubkey_hexes: &[impl AsRef<str>],
        relay_urls: &[String],
    ) -> Result<(), String> {
        // Warn if macOS initialization didn't happen (no bundle or failed)
        #[cfg(target_os = "macos")]
        if !self.macos_delegate.is_initialized() {
            warn!("macOS notifications started without main-thread initialization; foreground notifications may not work");
        }

        // Stop existing service if running
        if let Some(ref service) = self.service {
            if service.is_running() {
                service.stop();
            }
        }

        // Create new service
        let service = NotificationService::new();

        // Start the service with platform backend factory
        // Backend is constructed inside the worker thread
        service.start(PlatformBackend::new, pubkey_hexes, relay_urls)?;

        // Store the service
        self.service = Some(service);

        info!("NotificationManager: notifications started");
        Ok(())
    }

    /// Stop notification subscriptions.
    ///
    /// Signals the worker thread to stop and waits for it to finish.
    pub fn stop(&mut self) {
        if let Some(ref service) = self.service {
            service.stop();
            info!("NotificationManager: notifications stopped");
        }
    }

    /// Check if notifications are currently running.
    pub fn is_running(&self) -> bool {
        self.service
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NotificationManager {
    fn drop(&mut self) {
        if self.is_running() {
            self.stop();
        }
    }
}
