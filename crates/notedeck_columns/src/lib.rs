mod app;
//mod camera;
mod error;
//mod note;
//mod block;
mod abbrev;
pub mod accounts;
mod actionbar;
pub mod app_creation;
mod app_style;
mod args;
mod column;
mod deck_state;
mod decks;
mod draft;
mod key_parsing;
pub mod login_manager;
mod media_upload;
mod multi_subscriber;
mod nav;
mod post;
mod profile;
mod profile_state;
pub mod relay_pool_manager;
mod route;
mod search;
mod subscriptions;
mod support;
mod test_data;
mod timeline;
pub mod ui;
mod unknowns;
mod view_state;

#[cfg(test)]
#[macro_use]
mod test_utils;

pub mod storage;

pub use app::Damus;
pub use error::Error;
pub use profile::NostrName;

pub type Result<T> = std::result::Result<T, error::Error>;
