mod app;
//mod camera;
mod error;
//mod note;
//mod block;
mod abbrev;
pub mod account_manager;
mod actionbar;
pub mod app_creation;
mod app_style;
mod args;
mod colors;
mod column;
mod draft;
mod filter;
mod fonts;
mod frame_history;
mod images;
mod imgcache;
mod key_parsing;
mod key_storage;
pub mod login_manager;
mod macos_key_storage;
mod note;
mod notecache;
mod post;
mod profile;
pub mod relay_pool_manager;
mod result;
mod route;
mod subscriptions;
mod test_data;
mod thread;
mod time;
mod timecache;
mod timeline;
pub mod ui;
mod user_account;

#[cfg(test)]
#[macro_use]
mod test_utils;
mod linux_key_storage;

pub use app::Damus;
pub use error::Error;
pub use profile::DisplayName;

#[cfg(target_os = "android")]
use winit::platform::android::EventLoopBuilderExtAndroid;

pub type Result<T> = std::result::Result<T, error::Error>;

//#[cfg(target_os = "android")]
//use egui_android::run_android;

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

#[cfg(target_os = "android")]
#[no_mangle]
#[tokio::main]
pub async fn android_main(app: AndroidApp) {
    std::env::set_var("RUST_BACKTRACE", "full");
    android_logger::init_once(android_logger::Config::default().with_min_level(log::Level::Info));

    let path = app.internal_data_path().expect("data path");
    let mut options = eframe::NativeOptions::default();
    options.renderer = eframe::Renderer::Wgpu;
    options.event_loop_builder = Some(Box::new(move |builder| {
        builder.with_android_app(app);
    }));

    let _res = eframe::run_native(
        "Damus NoteDeck",
        options,
        Box::new(|cc| Ok(Box::new(Damus::new(cc, path, vec![])))),
    );
}
