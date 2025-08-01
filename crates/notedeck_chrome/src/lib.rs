pub mod setup;

#[cfg(target_os = "android")]
mod android;

mod app;
mod chrome;

pub use app::NotedeckApp;
pub use chrome::Chrome;
