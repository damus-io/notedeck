pub mod fonts;
pub mod setup;
pub mod theme;

#[cfg(target_os = "android")]
mod android;

mod app;
mod chrome;

pub use app::NotedeckApp;
pub use chrome::Chrome;
