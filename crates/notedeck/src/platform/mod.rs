#[cfg(target_os = "android")]
pub mod android;

#[cfg(target_os = "android")]
pub fn virtual_keyboard_height() -> i32 {
    android::virtual_keyboard_height()
}

#[cfg(not(target_os = "android"))]
pub fn virtual_keyboard_height() -> i32 {
    0
}
