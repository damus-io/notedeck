use crate::{platform::file::SelectedMedia, Error};

#[cfg(target_os = "android")]
pub mod android;
pub mod file;

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
