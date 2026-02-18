use crate::{platform::file::SelectedMedia, Error};

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(target_os = "android"))]
mod desktop_notifications;
pub mod file;

#[cfg(not(target_os = "android"))]
use crate::notifications::NotificationManager;

// =============================================================================
// Notification Mode API (Android-only with stubs for other platforms)
// =============================================================================

/// Notification delivery method.
/// Disabled by default — users must opt in to a notification method.
/// FCM provides better battery life and reliability.
/// Native provides maximum privacy by connecting directly to relays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationMode {
    /// Firebase Cloud Messaging - battery efficient, requires Google services
    Fcm,
    /// Direct relay connection - maximum privacy, higher battery usage
    Native,
    /// Notifications disabled
    #[default]
    Disabled,
}

impl NotificationMode {
    /// Returns true if this is the FCM mode
    pub fn is_fcm(&self) -> bool {
        matches!(self, NotificationMode::Fcm)
    }

    /// Returns true if this is the Native mode
    pub fn is_native(&self) -> bool {
        matches!(self, NotificationMode::Native)
    }

    /// Returns true if notifications are disabled
    pub fn is_disabled(&self) -> bool {
        matches!(self, NotificationMode::Disabled)
    }

    /// Convert to index for UI selection (0=FCM, 1=Native, 2=Disabled)
    pub fn to_index(&self) -> usize {
        match self {
            NotificationMode::Fcm => 0,
            NotificationMode::Native => 1,
            NotificationMode::Disabled => 2,
        }
    }

    /// Create from index (0=FCM, 1=Native, 2=Disabled)
    pub fn from_index(index: usize) -> Self {
        match index {
            0 => NotificationMode::Fcm,
            1 => NotificationMode::Native,
            _ => NotificationMode::Disabled,
        }
    }
}

/// Returns true if the current platform supports push notifications.
pub fn supports_notifications() -> bool {
    cfg!(any(
        target_os = "android",
        target_os = "macos",
        target_os = "linux"
    ))
}

/// Get the current notification mode.
/// On non-Android platforms, always returns Disabled.
#[cfg(target_os = "android")]
pub fn get_notification_mode() -> NotificationMode {
    android::get_notification_mode()
}

#[cfg(not(target_os = "android"))]
pub fn get_notification_mode() -> NotificationMode {
    NotificationMode::Disabled
}

/// Set the notification mode, handling mutual exclusivity.
/// This will disable the previous mode before enabling the new one.
/// On non-Android platforms, this is a no-op.
#[cfg(target_os = "android")]
pub fn set_notification_mode(
    mode: NotificationMode,
    pubkey_hex: &str,
    relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    android::set_notification_mode(mode, pubkey_hex, relay_urls)
}

#[cfg(not(target_os = "android"))]
pub fn set_notification_mode(
    _mode: NotificationMode,
    _pubkey_hex: &str,
    _relay_urls: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Enable push notifications for the given pubkey.
///
/// On desktop, requires a mutable reference to the `NotificationManager`.
/// Events are forwarded from the main event loop via channel (no separate relay connection).
#[cfg(not(target_os = "android"))]
pub fn enable_notifications(
    manager: &mut Option<NotificationManager>,
    pubkey_hex: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    desktop_notifications::enable_notifications(manager, pubkey_hex)
}

/// Disable push notifications.
///
/// On desktop, requires a mutable reference to the `NotificationManager`.
#[cfg(not(target_os = "android"))]
pub fn disable_notifications(
    manager: &mut Option<NotificationManager>,
) -> Result<(), Box<dyn std::error::Error>> {
    desktop_notifications::disable_notifications(manager)
}

/// Check if notification permission is granted.
/// On desktop platforms, delegates to desktop_notifications.
#[cfg(target_os = "android")]
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    android::is_notification_permission_granted()
}

#[cfg(not(target_os = "android"))]
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    desktop_notifications::is_notification_permission_granted()
}

/// Request notification permission from the user.
#[cfg(target_os = "android")]
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    android::request_notification_permission()
}

#[cfg(not(target_os = "android"))]
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    desktop_notifications::request_notification_permission()
}

/// Check if a notification permission request is currently pending.
#[cfg(target_os = "android")]
pub fn is_notification_permission_pending() -> bool {
    android::is_notification_permission_pending()
}

#[cfg(not(target_os = "android"))]
pub fn is_notification_permission_pending() -> bool {
    desktop_notifications::is_notification_permission_pending()
}

/// Get the result of the last notification permission request.
#[cfg(target_os = "android")]
pub fn get_notification_permission_result() -> bool {
    android::get_notification_permission_result()
}

#[cfg(not(target_os = "android"))]
pub fn get_notification_permission_result() -> bool {
    desktop_notifications::get_notification_permission_result()
}

/// Check if notifications are currently enabled.
#[cfg(target_os = "android")]
pub fn are_notifications_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    android::are_notifications_enabled()
}

/// Check if notifications are currently enabled.
///
/// On desktop, checks if the `NotificationManager` service is running.
#[cfg(not(target_os = "android"))]
pub fn are_notifications_enabled(
    manager: &Option<NotificationManager>,
) -> Result<bool, Box<dyn std::error::Error>> {
    desktop_notifications::are_notifications_enabled(manager)
}

/// Check if the notification service is currently running.
#[cfg(target_os = "android")]
pub fn is_notification_service_running() -> Result<bool, Box<dyn std::error::Error>> {
    android::is_notification_service_running()
}

/// Check if the notification service is currently running.
///
/// On desktop, checks the `NotificationManager` state.
#[cfg(not(target_os = "android"))]
pub fn is_notification_service_running(
    manager: &Option<NotificationManager>,
) -> Result<bool, Box<dyn std::error::Error>> {
    desktop_notifications::is_notification_service_running(manager)
}

// =============================================================================
// Deep Link API (Android-only with stubs for other platforms)
// =============================================================================

/// Information about a deep link from a notification tap.
#[derive(Debug, Clone)]
pub struct DeepLinkInfo {
    pub event_id: String,
    pub event_kind: i32,
    pub author_pubkey: Option<String>,
}

/// Check if there's a pending deep link and consume it.
#[cfg(target_os = "android")]
pub fn take_pending_deep_link() -> Option<DeepLinkInfo> {
    android::take_pending_deep_link().map(|dl| DeepLinkInfo {
        event_id: dl.event_id,
        event_kind: dl.event_kind,
        author_pubkey: dl.author_pubkey,
    })
}

#[cfg(not(target_os = "android"))]
pub fn take_pending_deep_link() -> Option<DeepLinkInfo> {
    None
}

/// Check if there's a pending deep link without consuming it.
#[cfg(target_os = "android")]
pub fn has_pending_deep_link() -> bool {
    android::has_pending_deep_link()
}

#[cfg(not(target_os = "android"))]
pub fn has_pending_deep_link() -> bool {
    false
}

pub fn get_next_selected_file() -> Option<Result<SelectedMedia, Error>> {
    file::get_next_selected_file()
}

const VIRT_HEIGHT: i32 = 400;

#[cfg(target_os = "android")]
pub fn virtual_keyboard_height(virt: bool) -> i32 {
    if virt {
        VIRT_HEIGHT
    } else {
        android::virtual_keyboard_height()
    }
}

#[cfg(not(target_os = "android"))]
pub fn virtual_keyboard_height(virt: bool) -> i32 {
    if virt {
        VIRT_HEIGHT
    } else {
        0
    }
}

pub fn virtual_keyboard_rect(ui: &egui::Ui, virt: bool) -> Option<egui::Rect> {
    let height = virtual_keyboard_height(virt);
    if height <= 0 {
        return None;
    }
    let screen_rect = ui.ctx().screen_rect();
    let min = egui::Pos2::new(0.0, screen_rect.max.y - height as f32);
    Some(egui::Rect::from_min_max(min, screen_rect.max))
}
