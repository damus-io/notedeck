//! Desktop notification service management.
//!
//! Manages the lifecycle of the desktop NotificationService, providing
//! platform functions similar to the Android implementation.

use crate::notifications::{DesktopBackend, NotificationService};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{error, info};

/// Global notification service instance.
///
/// NOTE: This global exists because the platform API functions are stateless
/// (they don't receive app state as a parameter). This mirrors how Android
/// manages notification state via JNI globals. The alternative would require
/// significant refactoring to pass the service through all UI layers.
static NOTIFICATION_SERVICE: RwLock<Option<NotificationService<DesktopBackend>>> =
    RwLock::new(None);

/// Tracks whether notifications are enabled (persisted preference).
static NOTIFICATIONS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable push notifications for the given pubkey and relay URLs.
pub fn enable_notifications(
    pubkey_hex: &str,
    relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    // Create the backend and service
    let backend = Arc::new(DesktopBackend::new("Notedeck"));
    let service = NotificationService::new(backend);

    // Start the service
    service
        .start(&[pubkey_hex], relay_urls)
        .map_err(|e| Box::new(std::io::Error::other(e)) as Box<dyn std::error::Error>)?;

    // Store the service
    match NOTIFICATION_SERVICE.write() {
        Ok(mut guard) => {
            *guard = Some(service);
            NOTIFICATIONS_ENABLED.store(true, Ordering::SeqCst);
            info!(
                "Desktop notifications enabled for pubkey {}",
                &pubkey_hex[..8]
            );
            Ok(())
        }
        Err(e) => {
            error!("Failed to store notification service: {}", e);
            Err(Box::new(std::io::Error::other("Lock error")))
        }
    }
}

/// Disable push notifications.
pub fn disable_notifications() -> Result<(), Box<dyn std::error::Error>> {
    match NOTIFICATION_SERVICE.write() {
        Ok(mut guard) => {
            if let Some(service) = guard.take() {
                service.stop();
            }
            NOTIFICATIONS_ENABLED.store(false, Ordering::SeqCst);
            info!("Desktop notifications disabled");
            Ok(())
        }
        Err(e) => {
            error!("Failed to disable notification service: {}", e);
            Err(Box::new(std::io::Error::other("Lock error")))
        }
    }
}

/// Check if notifications are currently enabled.
pub fn are_notifications_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(NOTIFICATIONS_ENABLED.load(Ordering::SeqCst))
}

/// Check if the notification service is currently running.
pub fn is_notification_service_running() -> Result<bool, Box<dyn std::error::Error>> {
    match NOTIFICATION_SERVICE.read() {
        Ok(guard) => Ok(guard.as_ref().map(|s| s.is_running()).unwrap_or(false)),
        Err(_) => Ok(false),
    }
}

/// Check if notification permission is granted.
/// On desktop, always returns true (no permission system like Android).
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(true)
}

/// Request notification permission.
/// On desktop, this is a no-op (no permission system like Android).
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
