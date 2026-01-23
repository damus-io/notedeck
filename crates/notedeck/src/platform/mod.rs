use crate::{platform::file::SelectedMedia, Error};

#[cfg(target_os = "android")]
pub mod android;
pub mod file;

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
    cfg!(target_os = "android")
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

/// Check if notification permission is granted.
/// On non-Android platforms, always returns true.
#[cfg(target_os = "android")]
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    android::is_notification_permission_granted()
}

#[cfg(not(target_os = "android"))]
pub fn is_notification_permission_granted() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(true)
}

/// Request notification permission from the user.
/// On non-Android platforms, this is a no-op.
#[cfg(target_os = "android")]
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    android::request_notification_permission()
}

#[cfg(not(target_os = "android"))]
pub fn request_notification_permission() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Check if a notification permission request is currently pending.
/// On non-Android platforms, always returns false.
#[cfg(target_os = "android")]
pub fn is_notification_permission_pending() -> bool {
    android::is_notification_permission_pending()
}

#[cfg(not(target_os = "android"))]
pub fn is_notification_permission_pending() -> bool {
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
