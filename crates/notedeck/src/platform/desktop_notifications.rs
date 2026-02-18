//! Desktop notification service management.
//!
//! Provides notification control functions that delegate to `NotificationManager`.

use crate::notifications::NotificationManager;
use tracing::info;

/// Enable push notifications for the given pubkey.
///
/// Delegates to `NotificationManager::start()`.
pub fn enable_notifications(
    manager: &mut Option<NotificationManager>,
    pubkey_hex: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mgr = manager.get_or_insert_with(NotificationManager::new);

    mgr.start(&[pubkey_hex])
        .map_err(|e| Box::new(std::io::Error::other(e)) as Box<dyn std::error::Error>)?;

    info!(
        "Desktop notifications enabled for pubkey {}",
        &pubkey_hex[..8.min(pubkey_hex.len())]
    );
    Ok(())
}

/// Disable push notifications.
pub fn disable_notifications(
    manager: &mut Option<NotificationManager>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(mgr) = manager.as_mut() {
        mgr.stop();
        info!("Desktop notifications disabled");
    }
    Ok(())
}

/// Check if notifications are currently enabled (service is running).
pub fn are_notifications_enabled(
    manager: &Option<NotificationManager>,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(manager.as_ref().map(|m| m.is_running()).unwrap_or(false))
}

/// Check if the notification service is currently running.
pub fn is_notification_service_running(
    manager: &Option<NotificationManager>,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(manager.as_ref().map(|m| m.is_running()).unwrap_or(false))
}

/// Check if notification permission is granted.
/// On desktop, always returns true (no permission system like Android).
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(true)
}

/// Request notification permission.
/// On desktop, this is a no-op.
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Check if a permission request is pending.
/// On desktop, always returns false.
pub fn is_notification_permission_pending() -> bool {
    false
}

/// Get the result of the last permission request.
/// On desktop, always returns true.
pub fn get_notification_permission_result() -> bool {
    true
}
