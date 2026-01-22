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
pub trait NotificationBackend: Send + Sync {
    /// Send a notification for a Nostr event.
    ///
    /// # Arguments
    /// * `event` - The extracted event data
    /// * `target_account` - Hex pubkey of the account this notification targets
    /// * `author_name` - Optional display name from cached profile
    /// * `picture_url` - Optional profile picture URL from cached profile
    fn send_notification(
        &self,
        event: &ExtractedEvent,
        target_account: &str,
        author_name: Option<&str>,
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
        _event: &ExtractedEvent,
        _target_account: &str,
        _author_name: Option<&str>,
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
        event: &ExtractedEvent,
        target_account: &str,
        author_name: Option<&str>,
        _picture_url: Option<&str>,
    ) {
        // Use safe slicing to avoid panics on malformed/short strings
        let id_preview = event.id.get(..8).unwrap_or(&event.id);
        let pubkey_preview = event.pubkey.get(..8).unwrap_or(&event.pubkey);
        let target_preview = target_account.get(..8).unwrap_or(target_account);

        tracing::info!(
            "Notification: kind={} id={} from={} target={} author_name={:?}",
            event.kind,
            id_preview,
            pubkey_preview,
            target_preview,
            author_name
        );
    }

    fn on_relay_status_changed(&self, connected_count: i32) {
        tracing::debug!("Relay status changed: {} connected", connected_count);
    }
}
