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

impl DesktopBackend {
    /// Format notification title based on event kind.
    fn format_title(&self, event: &ExtractedEvent, author_name: Option<&str>) -> String {
        let fallback: String;
        let author = match author_name {
            Some(name) => name,
            None => {
                fallback = safe_prefix(&event.pubkey, 8);
                &fallback
            }
        };

        match event.kind {
            1 => format!("{} mentioned you", author),
            4 => format!("DM from {}", author),
            6 => format!("{} reposted your note", author),
            7 => format!("{} reacted to your note", author),
            1059 => format!("Encrypted message from {}", author),
            9735 => {
                if let Some(sats) = event.zap_amount_sats {
                    format!("⚡ {} zapped you {} sats", author, sats)
                } else {
                    format!("⚡ {} zapped you", author)
                }
            }
            _ => format!("Notification from {}", author),
        }
    }

    /// Format notification body based on event content.
    fn format_body(&self, event: &ExtractedEvent) -> String {
        // For DMs and encrypted messages, don't show content
        if event.kind == 4 || event.kind == 1059 {
            return "Tap to view".to_string();
        }

        // Truncate long content (UTF-8 safe)
        let max_chars = 200;
        if event.content.chars().count() > max_chars {
            let truncated: String = event.content.chars().take(max_chars).collect();
            format!("{}...", truncated)
        } else if event.content.is_empty() {
            // Reactions often have empty content or just an emoji
            if event.kind == 7 {
                "❤️".to_string()
            } else {
                String::new()
            }
        } else {
            event.content.clone()
        }
    }
}

impl NotificationBackend for DesktopBackend {
    fn send_notification(
        &self,
        event: &ExtractedEvent,
        target_account: &str,
        author_name: Option<&str>,
        _picture_url: Option<&str>,
    ) {
        info!(
            "Sending desktop notification: kind={} id={} target={}",
            event.kind,
            safe_prefix(&event.id, 8),
            safe_prefix(target_account, 8),
        );

        let title = self.format_title(event, author_name);
        let body = self.format_body(event);

        #[cfg(target_os = "linux")]
        {
            use notify_rust::{Notification, Urgency};

            let urgency = match event.kind {
                4 | 1059 => Urgency::Critical, // DMs are high priority
                9735 => Urgency::Normal,       // Zaps
                _ => Urgency::Normal,
            };

            match Notification::new()
                .appname(&self.app_name)
                .summary(&title)
                .body(&body)
                .urgency(urgency)
                .show()
            {
                Ok(_) => debug!("Desktop notification displayed"),
                Err(e) => error!("Failed to show desktop notification: {}", e),
            }
        }

        #[cfg(target_os = "macos")]
        {
            show_macos_notification(&title, &body, _picture_url);
        }

        #[cfg(target_os = "android")]
        {
            // Should not reach here - Android uses JNI backend
            debug!("Desktop backend called on Android - ignoring");
        }
    }

    fn on_relay_status_changed(&self, connected_count: i32) {
        debug!("Relay status: {} connected", connected_count);
        // Desktop doesn't need to update a service notification like Android does
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
/// App Nap is a macOS power-saving feature that can suspend background apps.
/// We need to prevent this to maintain relay connections.
///
/// Note: This is a one-way operation - once called, App Nap remains disabled
/// for the lifetime of the process. This is intentional for a notification
/// worker that needs to maintain persistent relay connections.
#[cfg(target_os = "macos")]
pub fn disable_app_nap() {
    macos_app_nap::prevent();
    info!("App Nap disabled for notification worker");
}

#[cfg(not(target_os = "macos"))]
pub fn disable_app_nap() {
    // No-op on non-macOS platforms
}
