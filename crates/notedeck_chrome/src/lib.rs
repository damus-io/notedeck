pub mod setup;

#[cfg(target_os = "android")]
mod android;

mod app;
mod chrome;
mod options;

pub use app::NotedeckApp;
pub use chrome::Chrome;
pub use options::ChromeOptions;
