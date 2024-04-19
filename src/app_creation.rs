use crate::app_style::{create_text_styles, dark_mode, desktop_font_size, mobile_font_size};
use crate::fonts::setup_fonts;
use crate::ui::is_mobile;
use eframe::NativeOptions;

pub const UI_SCALE_FACTOR: f32 = 0.2;

pub fn generate_native_options() -> NativeOptions {
    generate_native_options_with_builder_modifiers(|builder| {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
    })
}

fn generate_native_options_with_builder_modifiers(
    apply_builder_modifiers: fn(egui::ViewportBuilder) -> egui::ViewportBuilder,
) -> NativeOptions {
    let window_builder =
        Box::new(move |builder: egui::ViewportBuilder| apply_builder_modifiers(builder));

    eframe::NativeOptions {
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
    })
}

pub fn setup_cc(cc: &eframe::CreationContext<'_>) {
    let ctx = &cc.egui_ctx;
    setup_fonts(ctx);

    ctx.set_pixels_per_point(ctx.pixels_per_point() + UI_SCALE_FACTOR);

    egui_extras::install_image_loaders(ctx);

    ctx.set_visuals(dark_mode(is_mobile(ctx)));

    ctx.set_style(if is_mobile(ctx) {
        create_text_styles(ctx, mobile_font_size)
    } else {
        create_text_styles(ctx, desktop_font_size)
    });
}
