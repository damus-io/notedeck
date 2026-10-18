pub mod keymap;
pub mod setup;
pub mod shot;
pub mod testing;

#[cfg(target_os = "android")]
mod android;

mod app;
mod chrome;
mod options;

pub use app::NotedeckApp;
pub use chrome::{
    open_shortcut, Chrome, CommandBar, JumpList, RunnableTool, ToolPlace, EDIT_KEYMAP,
    HIDE_KEY_HINTS, JUMP_BACK, JUMP_FORWARD, JUMP_LIST_CAP, KEYMAP_TEMPLATE, NAV_BACK, NAV_FORWARD,
    OPEN, OPEN_COMMAND_BAR, TOGGLE_KEY_HINTS,
};
pub use options::ChromeOptions;
