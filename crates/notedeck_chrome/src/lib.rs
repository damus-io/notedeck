pub mod keymap;
pub mod setup;

#[cfg(target_os = "android")]
mod android;

mod app;
mod chrome;
mod options;

pub use app::NotedeckApp;
pub use chrome::{
    Chrome, JumpList, EDIT_KEYMAP, HIDE_KEY_HINTS, JUMP_BACK, JUMP_FORWARD, JUMP_LIST_CAP,
    KEYMAP_TEMPLATE, TOGGLE_KEY_HINTS,
};
pub use options::ChromeOptions;
