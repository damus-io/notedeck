use crate::{platform::file::SelectedMedia, Error};

#[cfg(target_os = "android")]
pub mod android;
pub mod file;

// =============================================================================
// Notification API (Android-only with stubs for other platforms)
// =============================================================================

/// Enable push notifications for the given pubkey.
/// On non-Android platforms, this is a no-op.
#[cfg(target_os = "android")]
pub fn enable_notifications(pubkey_hex: &str) -> Result<(), Box<dyn std::error::Error>> {
    android::enable_notifications(pubkey_hex)
}

#[cfg(not(target_os = "android"))]
pub fn enable_notifications(_pubkey_hex: &str) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Disable push notifications.
/// On non-Android platforms, this is a no-op.
#[cfg(target_os = "android")]
pub fn disable_notifications() -> Result<(), Box<dyn std::error::Error>> {
    android::disable_notifications()
}

#[cfg(not(target_os = "android"))]
pub fn disable_notifications() -> Result<(), Box<dyn std::error::Error>> {
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

/// Get the result of the last notification permission request.
/// On non-Android platforms, always returns true.
#[cfg(target_os = "android")]
pub fn get_notification_permission_result() -> bool {
    android::get_notification_permission_result()
}

#[cfg(not(target_os = "android"))]
pub fn get_notification_permission_result() -> bool {
    true
}

/// Check if notifications are currently enabled in preferences.
/// On non-Android platforms, always returns false.
#[cfg(target_os = "android")]
pub fn are_notifications_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    android::are_notifications_enabled()
}

#[cfg(not(target_os = "android"))]
pub fn are_notifications_enabled() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(false)
}

/// Check if the notification service is currently running.
/// On non-Android platforms, always returns false.
#[cfg(target_os = "android")]
pub fn is_notification_service_running() -> Result<bool, Box<dyn std::error::Error>> {
    android::is_notification_service_running()
}

#[cfg(not(target_os = "android"))]
pub fn is_notification_service_running() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(false)
}

/// Returns true if the current platform supports push notifications.
pub fn supports_notifications() -> bool {
    cfg!(target_os = "android")
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
