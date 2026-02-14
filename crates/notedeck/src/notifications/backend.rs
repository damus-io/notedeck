//! Notification backend trait for platform-specific notification delivery.
//!
//! This trait abstracts over the platform-specific notification systems:
//! - Android: JNI callbacks to Kotlin NotificationHelper
//! - Desktop (macOS/Linux): notify-rust crate

use super::types::ExtractedEvent;

/// Backend for delivering notifications to the user.
///
/// Implementations handle platform-specific notification delivery:
/// - Android uses JNI to call Kotlin code
/// - Desktop uses notify-rust for native notifications
///
/// # Thread Safety
///
/// Backends do NOT need to implement `Send` or `Sync`. They are constructed
/// inside the worker thread via a factory function and never leave that thread.
pub trait NotificationBackend {
    /// Send a notification for a Nostr event.
    ///
    /// Title and body are pre-formatted (and localized) by the caller.
    ///
    /// # Arguments
    /// * `title` - Localized notification title
    /// * `body` - Localized notification body
    /// * `event` - The extracted event data (for logging / platform urgency)
    /// * `target_account` - Hex pubkey of the account this notification targets
    /// * `picture_url` - Optional profile picture URL or local path
    fn send_notification(
        &self,
        title: &str,
        body: &str,
        event: &ExtractedEvent,
        target_account: &str,
        picture_url: Option<&str>,
    );

    /// Called when relay connection status changes.
    ///
    /// # Arguments
    /// * `connected_count` - Number of currently connected relays
    fn on_relay_status_changed(&self, connected_count: i32);
}

/// A no-op backend that does nothing.
///
/// Used when notifications are disabled or on unsupported platforms.
pub struct NoopBackend;

impl NotificationBackend for NoopBackend {
    fn send_notification(
        &self,
        _title: &str,
        _body: &str,
        _event: &ExtractedEvent,
        _target_account: &str,
        _picture_url: Option<&str>,
    ) {
        // Do nothing
    }

    fn on_relay_status_changed(&self, _connected_count: i32) {
        // Do nothing
    }
}

/// A logging backend that just logs notifications.
///
/// Useful for debugging and testing.
pub struct LoggingBackend;

impl NotificationBackend for LoggingBackend {
    fn send_notification(
        &self,
        title: &str,
        body: &str,
        event: &ExtractedEvent,
        target_account: &str,
        _picture_url: Option<&str>,
    ) {
        let id_preview = event.id.get(..8).unwrap_or(&event.id);
        let target_preview = target_account.get(..8).unwrap_or(target_account);

        tracing::info!(
            "Notification: kind={} id={} target={} title={:?} body={:?}",
            event.kind,
            id_preview,
            target_preview,
            title,
            body,
        );
    }

    fn on_relay_status_changed(&self, connected_count: i32) {
        tracing::debug!("Relay status changed: {} connected", connected_count);
    }
}
