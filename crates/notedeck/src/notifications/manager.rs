//! Notification manager for owned notification state.
//!
//! Provides a simple interface for managing the notification service lifecycle
//! without using global statics. Designed to be owned by `Notedeck` and accessed
//! via `AppContext`.

use super::ndb_helpers;
use super::types::ExtractedEvent;
use super::{NotificationData, NotificationService, PlatformBackend};
use crate::account::accounts::Accounts;
use crate::i18n::Localization;
use crate::tr;
use nostrdb::{Filter, Ndb, Subscription, Transaction};
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

// SAFETY: The Retained<AnyObject> is only held to prevent deallocation.
// Rust never sends Objective-C messages to the delegate; `is_initialized()`
// only checks the Option wrapper. All callbacks are dispatched by the ObjC runtime.
#[cfg(target_os = "macos")]
unsafe impl Send for MacOSDelegate {}
#[cfg(target_os = "macos")]
unsafe impl Sync for MacOSDelegate {}

// =============================================================================
// NotificationManager
// =============================================================================

/// Manages notification service lifecycle.
///
/// On desktop, subscribes to nostrdb for notification-relevant events and
/// polls each frame. Events are already ingested by `remote_api.rs` — this
/// just subscribes and polls for matching notes.
pub struct NotificationManager {
    /// The underlying notification service (worker thread).
    service: Option<NotificationService<PlatformBackend>>,

    /// Nostrdb subscription for notification-relevant events.
    #[cfg(not(target_os = "android"))]
    ndb_sub: Option<Subscription>,

    /// 32-byte pubkeys of monitored accounts (for p-tag matching).
    #[cfg(not(target_os = "android"))]
    monitored_pubkeys: Vec<[u8; 32]>,

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
            #[cfg(not(target_os = "android"))]
            ndb_sub: None,
            #[cfg(not(target_os = "android"))]
            monitored_pubkeys: Vec::new(),
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
    /// Creates a nostrdb subscription for notification-relevant events
    /// (mentions, reactions, reposts, zaps, DMs) targeting the given pubkeys.
    ///
    /// # Arguments
    /// * `ndb` - Nostrdb handle (events are already ingested by remote_api)
    /// * `pubkey_hexes` - Hex-encoded pubkeys of accounts to monitor
    pub fn start(&mut self, ndb: &Ndb, pubkey_hexes: &[impl AsRef<str>]) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        if !self.macos_delegate.is_initialized() {
            warn!("macOS notifications started without main-thread initialization");
        }

        // Stop existing service if running
        if let Some(ref mut service) = self.service {
            if service.is_running() {
                service.stop();
            }
        }

        // Parse pubkeys to bytes for ndb filter and p-tag matching
        #[cfg(not(target_os = "android"))]
        {
            self.monitored_pubkeys.clear();
            for hex in pubkey_hexes {
                let hex = hex.as_ref();
                let bytes =
                    hex::decode(hex).map_err(|e| format!("Invalid pubkey hex {hex}: {e}"))?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| format!("Pubkey {hex} is not 32 bytes"))?;
                self.monitored_pubkeys.push(arr);
            }

            // Build nostrdb subscription for notification-relevant kinds
            let pubkey_refs: Vec<&[u8; 32]> = self.monitored_pubkeys.iter().collect();

            let notification_filter = Filter::new()
                .kinds(super::types::NOTIFICATION_KINDS.iter().map(|&k| k as u64))
                .pubkey(pubkey_refs)
                .build();

            match ndb.subscribe(&[notification_filter]) {
                Ok(sub) => {
                    self.ndb_sub = Some(sub);
                    info!("NotificationManager: created ndb subscription");
                }
                Err(e) => {
                    warn!(
                        "NotificationManager: failed to create ndb subscription: {}",
                        e
                    );
                }
            }
        }

        let mut service = NotificationService::new();
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
        if let Some(ref mut service) = self.service {
            service.stop();
            info!("NotificationManager: stopped");
        }
        self.service = None;

        #[cfg(not(target_os = "android"))]
        {
            self.ndb_sub = None;
            self.monitored_pubkeys.clear();
        }
    }

    /// Check if notifications are currently running.
    pub fn is_running(&self) -> bool {
        self.service
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    /// Poll nostrdb for new notification-relevant events and forward to the worker.
    ///
    /// Called each frame from the main event loop. Events are already ingested
    /// into nostrdb by `remote_api.rs` — this polls the subscription for
    /// matching notes, builds NotificationData, and sends to the worker.
    #[cfg(not(target_os = "android"))]
    #[profiling::function]
    pub fn poll_notifications(&self, ndb: &Ndb, _accounts: &Accounts, i18n: &mut Localization) {
        if !self.is_running() {
            return;
        }

        let Some(sub) = self.ndb_sub else {
            return;
        };

        let note_keys = ndb.poll_for_notes(sub, 50);
        if note_keys.is_empty() {
            return;
        }

        let Ok(txn) = Transaction::new(ndb) else {
            return;
        };

        for nk in note_keys {
            let Ok(note) = ndb.get_note_by_key(&txn, nk) else {
                continue;
            };

            let kind = note.kind() as i32;

            // Find which monitored account this event targets
            let Some(target_pubkey_hex) =
                ndb_helpers::find_target_ptag(&note, &self.monitored_pubkeys)
            else {
                continue;
            };

            let author_hex = hex::encode(note.pubkey());

            // Self-notification suppression
            if author_hex == target_pubkey_hex {
                continue;
            }

            let id_hex = hex::encode(note.id());

            debug!(
                "Notification-relevant event: kind={} id={} target={}",
                kind,
                &id_hex[..8.min(id_hex.len())],
                &target_pubkey_hex[..8.min(target_pubkey_hex.len())]
            );

            // Build ExtractedEvent from Note API
            let p_tags = ndb_helpers::extract_p_tags_from_note(&note);
            let zap_amount_sats = if kind == 9735 {
                ndb_helpers::extract_zap_amount_from_note(&note)
            } else {
                None
            };

            let event = ExtractedEvent {
                id: id_hex,
                kind,
                pubkey: author_hex,
                created_at: note.created_at(),
                content: note.content().to_string(),
                p_tags,
                zap_amount_sats,
                raw_json: String::new(),
            };

            // Profile lookup via nostrdb
            let (author_name, author_picture_url) =
                ndb_helpers::lookup_profile_ndb(ndb, &txn, note.pubkey());

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
