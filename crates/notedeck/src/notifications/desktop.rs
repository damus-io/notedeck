//! Desktop notification backend using notify-rust.
//!
//! Provides native desktop notifications on macOS and Linux.

use super::backend::NotificationBackend;
use super::types::ExtractedEvent;
use tracing::{debug, error, info};

/// Desktop notification backend using notify-rust.
///
/// Displays native system notifications on macOS and Linux.
/// On macOS, also handles App Nap prevention to keep relay connections alive.
pub struct DesktopBackend {
    /// App name shown in notifications
    app_name: String,
}

impl DesktopBackend {
    /// Create a new desktop notification backend.
    ///
    /// # Arguments
    /// * `app_name` - Application name to show in notifications
    pub fn new(app_name: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
        }
    }

    /// Format notification title based on event kind.
    fn format_title(&self, event: &ExtractedEvent, author_name: Option<&str>) -> String {
        let author = author_name.unwrap_or(&event.pubkey[..8]);

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

        // Truncate long content
        let max_len = 200;
        if event.content.len() > max_len {
            format!("{}...", &event.content[..max_len])
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
            &event.id[..8],
            &target_account[..8.min(target_account.len())]
        );

        let title = self.format_title(event, author_name);
        let body = self.format_body(event);

        #[cfg(not(target_os = "android"))]
        {
            use notify_rust::Notification;

            let mut notification = Notification::new();
            notification
                .appname(&self.app_name)
                .summary(&title)
                .body(&body);

            // Set urgency based on event kind
            #[cfg(target_os = "linux")]
            {
                use notify_rust::Urgency;
                let urgency = match event.kind {
                    4 | 1059 => Urgency::Critical, // DMs are high priority
                    9735 => Urgency::Normal,       // Zaps
                    _ => Urgency::Normal,
                };
                notification.urgency(urgency);
            }

            match notification.show() {
                Ok(_) => debug!("Desktop notification displayed"),
                Err(e) => error!("Failed to show desktop notification: {}", e),
            }
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
