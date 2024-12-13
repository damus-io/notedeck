pub mod app_size;
pub mod fonts;
pub mod setup;
pub mod theme;

mod app;

pub use app::Notedeck;

#[cfg(target_os = "android")]
mod android;
