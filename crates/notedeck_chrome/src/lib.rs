pub mod keymap;
pub mod setup;

#[cfg(target_os = "android")]
mod android;

mod app;
mod chrome;
mod options;

pub use app::NotedeckApp;
pub use chrome::{Chrome, HIDE_KEY_HINTS, TOGGLE_KEY_HINTS};
pub use options::ChromeOptions;
