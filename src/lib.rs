mod app;
//mod camera;
mod error;
//mod note;
//mod block;
mod abbrev;
mod widgets;
mod fonts;
mod images;
mod result;
mod imgcache;
mod filter;
mod ui;
mod timecache;
mod time;
mod notecache;
mod frame_history;
mod timeline;
mod colors;
mod profile;
mod key_parsing;
mod login_manager;

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
