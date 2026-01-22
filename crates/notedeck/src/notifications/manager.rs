//! Notification manager for owned notification state.
//!
//! Provides a simple interface for managing the notification service lifecycle
//! without using global statics. Designed to be owned by `Notedeck` and accessed
//! via `AppContext`.

use super::{NotificationData, NotificationService, PlatformBackend};
use tracing::{info, warn};

// =============================================================================
// macOS delegate wrapper (reduces unsafe surface area)
// =============================================================================

/// Wrapper for the macOS notification delegate with contained `Send + Sync` impls.
#[cfg(target_os = "macos")]
struct MacOSDelegate(Option<objc2::rc::Retained<objc2::runtime::AnyObject>>);

#[cfg(target_os = "macos")]
impl MacOSDelegate {
    fn new() -> Self {
        Self(super::macos::initialize_on_main_thread())
    }

    fn is_initialized(&self) -> bool {
        self.0.is_some()
    }
}

// SAFETY: The delegate is only kept alive for macOS callbacks,
// never accessed from Rust after initialization.
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
/// enable/disable notifications. The main event loop sends notification
/// data via `send()` which forwards to the worker thread.
///
/// # Usage
///
/// ```ignore
/// let mut manager = NotificationManager::new();
/// manager.start(&["pubkey_hex"])?;
///
/// // From main event loop:
/// manager.send(notification_data)?;
///
/// // Later:
/// manager.stop();
/// ```
pub struct NotificationManager {
    /// The underlying notification service.
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
    pub fn new() -> Self {
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

    /// Start the notification worker for the given accounts.
    ///
    /// # Arguments
    /// * `pubkey_hexes` - Hex-encoded pubkeys of accounts to monitor
    pub fn start(&mut self, pubkey_hexes: &[impl AsRef<str>]) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        if !self.macos_delegate.is_initialized() {
            warn!("macOS notifications started without main-thread initialization");
        }

        // Stop existing service if running
        if let Some(ref service) = self.service {
            if service.is_running() {
                service.stop();
            }
        }

        let service = NotificationService::new();
        service.start(PlatformBackend::new, pubkey_hexes)?;
        self.service = Some(service);

        info!("NotificationManager: started");
        Ok(())
    }

    /// Send notification data to the worker for display.
    ///
    /// Call this from the main event loop when a notification-relevant event
    /// is received and profile lookup is complete.
    pub fn send(&self, data: NotificationData) -> Result<(), String> {
        if let Some(ref service) = self.service {
            service.send(data)
        } else {
            Err("Notification service not running".to_string())
        }
    }

    /// Stop the notification worker.
    pub fn stop(&mut self) {
        if let Some(ref service) = self.service {
            service.stop();
            info!("NotificationManager: stopped");
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
