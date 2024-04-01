use crate::fonts::setup_fonts;
use eframe::NativeOptions;

pub const UI_SCALE_FACTOR: f32 = 0.2;

pub fn generate_native_options() -> NativeOptions {
    let window_builder = Box::new(|builder: egui::ViewportBuilder| {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
    });
    let mut native_options = eframe::NativeOptions::default();
    native_options.window_builder = Some(window_builder);
    native_options
}

pub fn setup_cc(cc: &eframe::CreationContext<'_>) {
    setup_fonts(&cc.egui_ctx);

    cc.egui_ctx
        .set_pixels_per_point(cc.egui_ctx.pixels_per_point() + UI_SCALE_FACTOR);

    egui_extras::install_image_loaders(&cc.egui_ctx);
}
