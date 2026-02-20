mod app;
//mod camera;
mod error;
//mod note;
//mod block;
pub mod accounts;
pub mod actionbar;
pub mod app_creation;
mod app_style;
mod args;
pub mod column;
mod deck_state;
mod decks;
mod draft;
mod key_parsing;
pub mod login_manager;
mod media_upload;
mod multi_subscriber;
mod nav;
mod onboarding;
pub mod options;
mod post;
mod profile;
mod repost;
mod route;
mod scoped_sub_owner_keys;
mod search;
mod subscriptions;
mod support;
mod test_data;
pub mod timeline;
mod timeline_loader;
mod toolbar;
pub mod ui;
mod view_state;

#[cfg(test)]
#[macro_use]
mod test_utils;

pub mod storage;

pub use app::Damus;
pub use error::Error;
pub use route::Route;

pub type Result<T> = std::result::Result<T, error::Error>;
