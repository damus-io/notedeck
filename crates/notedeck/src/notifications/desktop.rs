//! Desktop notification backend using notify-rust.
//!
//! Provides native desktop notifications on macOS and Linux.

use super::backend::NotificationBackend;
use super::types::ExtractedEvent;
use tracing::{debug, error, info};

/// Safely truncate a string to at most `n` characters, avoiding panics on
/// short strings or multi-byte UTF-8 boundaries.
fn safe_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Desktop notification backend using notify-rust.
///
/// Displays native system notifications on macOS and Linux.
/// On macOS, also handles App Nap prevention to keep relay connections alive.
pub struct DesktopBackend {
    /// App name shown in notifications (used on Linux)
    #[allow(dead_code)]
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
        title: &str,
        body: &str,
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

        #[cfg(target_os = "macos")]
        {
            show_macos_notification(title, body, _picture_url);
        }

        #[cfg(target_os = "android")]
        {
            // Should not reach here - Android uses JNI backend
            debug!("Desktop backend called on Android - ignoring");
        }
    }

    fn on_relay_status_changed(&self, connected_count: i32) {
        debug!("Relay status: {} connected", connected_count);
    }
}

/// Show a native macOS notification using osascript.
///
/// We use osascript instead of notify-rust because notify-rust's macOS
/// implementation (mac-notification-sys) sets up action handlers that cause
/// a "Where is use_default?" dialog when clicked outside of a proper .app bundle.
/// osascript works reliably in all scenarios.
#[cfg(target_os = "macos")]
fn show_macos_notification(title: &str, body: &str, _picture_url: Option<&str>) {
    use std::process::Command;

    // Escape special characters for AppleScript string
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_body = body.replace('\\', "\\\\").replace('"', "\\\"");

    let script = format!(
        r#"display notification "{}" with title "{}""#,
        escaped_body, escaped_title
    );

    match Command::new("osascript").args(["-e", &script]).output() {
        Ok(output) => {
            if output.status.success() {
                debug!("macOS notification displayed");
            } else {
                error!(
                    "osascript failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Err(e) => error!("Failed to show macOS notification: {}", e),
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
