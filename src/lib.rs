mod app;
//mod camera;
mod error;
//mod note;
//mod block;
mod abbrev;
pub mod account_login_view;
pub mod app_creation;
mod app_style;
mod colors;
mod filter;
mod fonts;
mod frame_history;
mod images;
mod imgcache;
mod key_parsing;
pub mod login_manager;
mod notecache;
mod profile;
pub mod relay_pool_manager;
mod result;
mod test_data;
mod time;
mod timecache;
mod timeline;
pub mod ui;

#[cfg(test)]
#[macro_use]
mod test_utils;

pub use app::Damus;
pub use error::Error;

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

    let res_ = eframe::run_native(
        "Damus NoteDeck",
        options,
        Box::new(|cc| Box::new(Damus::new(cc, path, vec![]))),
    );
}
