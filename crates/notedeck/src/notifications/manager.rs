//! Notification manager for owned notification state.
//!
//! Provides a simple interface for managing the notification service lifecycle
//! without using global statics. Designed to be owned by `Notedeck` and accessed
//! via `AppContext`.

use super::types::ExtractedEvent;
use super::{NotificationData, NotificationService, PlatformBackend};
use crate::i18n::Localization;
use crate::tr;
use nostrdb::{Ndb, Transaction};
use tracing::{debug, info, warn};

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
        if !super::macos::is_main_thread() {
            warn!("NotificationManager::new() should be called on the main thread on macOS");
        }

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

    /// Process a relay message and forward to the worker if relevant.
    ///
    /// Extracts the event, checks kind and p-tag mentions, resolves the
    /// author profile via nostrdb, formats a localized title/body, then
    /// sends to the worker channel.
    #[cfg(not(target_os = "android"))]
    #[profiling::function]
    pub fn process_relay_message(
        &self,
        relay_message: &str,
        ndb: &Ndb,
        monitored_pubkeys: &[String],
        i18n: &mut Localization,
    ) {
        if !self.is_running() {
            return;
        }

        let Some(event) = super::extraction::extract_event(relay_message) else {
            return;
        };

        if !super::types::is_notification_kind(event.kind) {
            return;
        }

        let target_pubkey_hex = event
            .p_tags
            .iter()
            .find(|p| monitored_pubkeys.contains(p))
            .cloned();

        let Some(target_pubkey_hex) = target_pubkey_hex else {
            return;
        };

        // Don't notify for our own events
        if monitored_pubkeys.contains(&event.pubkey) {
            return;
        }

        debug!(
            "Notification-relevant event: kind={} id={} target={}",
            event.kind,
            &event.id[..8.min(event.id.len())],
            &target_pubkey_hex[..8.min(target_pubkey_hex.len())]
        );

        let (author_name, author_picture_url) = lookup_profile(ndb, &event.pubkey);

        let title = format_title(&event, author_name.as_deref(), i18n);
        let body = format_body(&event, i18n);

        let notification_data = NotificationData {
            event,
            title,
            body,
            author_picture_url,
            target_pubkey_hex,
        };

        if let Err(e) = self.send(notification_data) {
            debug!("Failed to send notification: {}", e);
        }
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

/// Format a localized notification title from event data.
#[cfg(not(target_os = "android"))]
fn format_title(
    event: &ExtractedEvent,
    author_name: Option<&str>,
    i18n: &mut Localization,
) -> String {
    let fallback: String;
    let author = match author_name {
        Some(name) => name,
        None => {
            fallback = event.pubkey.chars().take(8).collect();
            &fallback
        }
    };

    match event.kind {
        1 => tr!(
            i18n,
            "{name} mentioned you",
            "notification title for mention",
            name = author
        ),
        4 => tr!(
            i18n,
            "DM from {name}",
            "notification title for direct message",
            name = author
        ),
        6 => tr!(
            i18n,
            "{name} reposted your note",
            "notification title for repost",
            name = author
        ),
        7 => tr!(
            i18n,
            "{name} reacted to your note",
            "notification title for reaction",
            name = author
        ),
        1059 => tr!(
            i18n,
            "Encrypted message from {name}",
            "notification title for encrypted DM",
            name = author
        ),
        9735 => {
            if let Some(sats) = event.zap_amount_sats {
                tr!(
                    i18n,
                    "{name} zapped you {sats} sats",
                    "notification title for zap with amount",
                    name = author,
                    sats = sats
                )
            } else {
                tr!(
                    i18n,
                    "{name} zapped you",
                    "notification title for zap",
                    name = author
                )
            }
        }
        _ => tr!(
            i18n,
            "Notification from {name}",
            "notification title fallback",
            name = author
        ),
    }
}

/// Format a localized notification body from event data.
#[cfg(not(target_os = "android"))]
fn format_body(event: &ExtractedEvent, i18n: &mut Localization) -> String {
    if event.kind == 4 || event.kind == 1059 {
        return tr!(
            i18n,
            "Tap to view",
            "notification body for encrypted content"
        );
    }

    let max_chars = 200;
    if event.content.chars().count() > max_chars {
        let truncated: String = event.content.chars().take(max_chars).collect();
        format!("{}...", truncated)
    } else if event.content.is_empty() {
        if event.kind == 7 {
            "\u{2764}\u{fe0f}".to_string()
        } else {
            String::new()
        }
    } else {
        event.content.clone()
    }
}

/// Look up a profile's display name and picture URL from nostrdb.
#[cfg(not(target_os = "android"))]
fn lookup_profile(ndb: &Ndb, pubkey_hex: &str) -> (Option<String>, Option<String>) {
    let Ok(pubkey_bytes) = hex::decode(pubkey_hex) else {
        return (None, None);
    };
    if pubkey_bytes.len() != 32 {
        return (None, None);
    }

    let Ok(txn) = Transaction::new(ndb) else {
        return (None, None);
    };

    let pubkey_arr: [u8; 32] = pubkey_bytes.try_into().unwrap();
    let Ok(profile) = ndb.get_profile_by_pubkey(&txn, &pubkey_arr) else {
        return (None, None);
    };

    let record = profile.record();
    let name = record
        .profile()
        .and_then(|p| p.display_name().or_else(|| p.name()))
        .map(|s| s.to_string());

    let picture = record
        .profile()
        .and_then(|p| p.picture())
        .map(|s| s.to_string());

    (name, picture)
}
