mod app;
//mod camera;
mod contacts;
mod error;
//mod note;
//mod block;
mod abbrev;
mod fonts;
mod images;
mod result;
mod ui;
mod frame_history;

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

    let mut options = eframe::NativeOptions::default();
    options.renderer = eframe::Renderer::Wgpu;
    options.event_loop_builder = Some(Box::new(move |builder| {
        builder.with_android_app(app);
    }));

    let res_ = eframe::run_native(
        "Damus NoteDeck",
        options,
        Box::new(|_cc| Box::new(Damus::new())),
    );
}
