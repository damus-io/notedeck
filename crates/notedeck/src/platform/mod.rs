use crate::{platform::file::SelectedMedia, Error};

#[cfg(target_os = "android")]
pub mod android;
pub mod file;

pub fn get_next_selected_file() -> Option<Result<SelectedMedia, Error>> {
    file::get_next_selected_file()
}

#[cfg(target_os = "android")]
pub fn virtual_keyboard_height() -> i32 {
    android::virtual_keyboard_height()
}

#[cfg(not(target_os = "android"))]
pub fn virtual_keyboard_height() -> i32 {
    0
}
