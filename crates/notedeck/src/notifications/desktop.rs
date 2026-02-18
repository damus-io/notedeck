//! Desktop notification backend using notify-rust.
//!
//! Provides native desktop notifications on Linux. On macOS, the
//! `MacOSBackend` is used instead (via `PlatformBackend` type alias).

use super::backend::NotificationBackend;
use super::types::{safe_prefix, ExtractedEvent};
#[cfg(target_os = "linux")]
use tracing::error;
use tracing::{debug, info};

/// Desktop notification backend using notify-rust (Linux).
///
/// On macOS, `MacOSBackend` handles notifications via `UNUserNotificationCenter`.
/// This backend also provides the cross-platform `disable_app_nap()` helper.
pub struct DesktopBackend {
    /// App name shown in notifications
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    app_name: String,
}

impl DesktopBackend {
    /// Create a new desktop notification backend.
    ///
    /// # Arguments
    /// * `app_name` - Application name to show in notifications
    pub fn with_app_name(app_name: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
        }
    }

    /// Create a new desktop notification backend with default app name "Notedeck".
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for DesktopBackend {
    fn default() -> Self {
        Self {
            app_name: "Notedeck".to_string(),
        }
    }
}

impl NotificationBackend for DesktopBackend {
    fn send_notification(
        &self,
        #[cfg_attr(not(target_os = "linux"), allow(unused))] title: &str,
        #[cfg_attr(not(target_os = "linux"), allow(unused))] body: &str,
        event: &ExtractedEvent,
        target_account: &str,
        _picture_url: Option<&str>,
    ) {
        info!(
            "Sending desktop notification: kind={} id={} target={}",
            event.kind,
            safe_prefix(&event.id, 8),
            safe_prefix(target_account, 8),
        );

        #[cfg(target_os = "linux")]
        {
            use notify_rust::{Notification, Urgency};

            let urgency = match event.kind {
                4 | 1059 => Urgency::Critical, // DMs are high priority
                _ => Urgency::Normal,
            };

            match Notification::new()
                .appname(&self.app_name)
                .summary(title)
                .body(body)
                .urgency(urgency)
                .show()
            {
                Ok(_) => debug!("Desktop notification displayed"),
                Err(e) => error!("Failed to show desktop notification: {}", e),
            }
        }
    }

    fn on_relay_status_changed(&self, connected_count: i32) {
        debug!("Relay status: {} connected", connected_count);
    }
}

/// Prevent macOS App Nap from suspending the notification worker.
///
/// Uses `NSProcessInfo.beginActivityWithOptions:reason:` to disable App Nap.
/// This is a one-way operation for the lifetime of the returned activity token.
/// We intentionally leak the token so App Nap stays disabled for the process.
#[cfg(target_os = "macos")]
pub fn disable_app_nap() {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    unsafe {
        // NSActivityUserInitiatedAllowingIdleSystemSleep = 0x00FFFFFFULL
        // This prevents App Nap while allowing the system to sleep.
        let options: u64 = 0x00FF_FFFF;
        let reason = NSString::from_str("Maintaining relay connections for notifications");

        let process_info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let activity: Retained<AnyObject> = msg_send![
            process_info,
            beginActivityWithOptions: options,
            reason: &*reason
        ];

        // Leak the activity token so App Nap stays disabled for the process lifetime
        std::mem::forget(activity);
    }

    info!("App Nap disabled for notification worker via NSProcessInfo");
}

#[cfg(not(target_os = "macos"))]
pub fn disable_app_nap() {
    // No-op on non-macOS platforms
}
