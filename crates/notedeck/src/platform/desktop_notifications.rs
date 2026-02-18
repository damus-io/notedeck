//! Desktop notification service management.
//!
//! Provides notification control functions that delegate to `NotificationManager`.

use crate::notifications::NotificationManager;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

/// Whether macOS notification permission has been granted.
/// On Linux, always true (no permission system). On macOS, updated
/// by the authorization callback in `macos.rs`.
static PERMISSION_GRANTED: AtomicBool = AtomicBool::new(cfg!(target_os = "linux"));

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
/// On Linux, always returns true (no permission system).
/// On macOS, returns the result from the authorization request.
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(PERMISSION_GRANTED.load(Ordering::SeqCst))
}

/// Set the notification permission state (called from macOS authorization callback).
pub fn set_permission_granted(granted: bool) {
    PERMISSION_GRANTED.store(granted, Ordering::SeqCst);
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

/// Re-query the OS for the current notification permission state.
/// On macOS, calls `getNotificationSettingsWithCompletionHandler`.
/// On Linux, this is a no-op (permissions are always granted).
pub fn refresh_notification_permission() {
    #[cfg(target_os = "macos")]
    {
        crate::notifications::macos::refresh_permission_status();
    }
}
