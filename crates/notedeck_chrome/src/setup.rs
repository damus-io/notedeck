use eframe::NativeOptions;
use notedeck::{AppSizeHandler, DataPath};
use notedeck_ui::app_images;

pub fn generate_native_options(paths: DataPath) -> NativeOptions {
    let window_builder = Box::new(move |builder: egui::ViewportBuilder| {
        let builder = builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_icon(std::sync::Arc::new(app_images::app_icon()));

        if let Some(window_size) = AppSizeHandler::new(&paths).get_app_size() {
            builder.with_inner_size(window_size)
        } else {
            builder
        }
    });

    eframe::NativeOptions {
        // for 3d widgets
        depth_buffer: 24,
        window_builder: Some(window_builder),
        viewport: egui::ViewportBuilder::default()
            .with_icon(std::sync::Arc::new(app_images::app_icon())),
        ..Default::default()
    }
}

fn generate_native_options_with_builder_modifiers(
    apply_builder_modifiers: fn(egui::ViewportBuilder) -> egui::ViewportBuilder,
) -> NativeOptions {
    let window_builder =
        Box::new(move |builder: egui::ViewportBuilder| apply_builder_modifiers(builder));

    eframe::NativeOptions {
        // for 3d widgets
        depth_buffer: 24,
        window_builder: Some(window_builder),
        ..Default::default()
    }
}

pub fn generate_mobile_emulator_native_options() -> eframe::NativeOptions {
    generate_native_options_with_builder_modifiers(|builder| {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_inner_size([405.0, 915.0])
            .with_icon(app_images::app_icon())
    })
}
