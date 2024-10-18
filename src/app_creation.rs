use crate::app_size_handler::AppSizeHandler;
use crate::app_style::{
    create_custom_style, dark_mode, desktop_font_size, light_mode, mobile_font_size,
};
use crate::fonts::setup_fonts;
use eframe::NativeOptions;
use tracing::info;

//pub const UI_SCALE_FACTOR: f32 = 0.2;

pub fn generate_native_options() -> NativeOptions {
    generate_native_options_with_builder_modifiers(|builder| {
        let builder = builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);

        if let Some(window_size) = AppSizeHandler::default().get_app_size() {
            builder.with_inner_size(window_size)
        } else {
            info!("Could not read app window size from file");
            builder
        }
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

pub fn setup_cc(cc: &eframe::CreationContext<'_>, is_mobile: bool, light: bool) {
    let ctx = &cc.egui_ctx;
    setup_fonts(ctx);

    //ctx.set_pixels_per_point(ctx.pixels_per_point() + UI_SCALE_FACTOR);
    //ctx.set_pixels_per_point(1.0);
    //
    //
    //ctx.tessellation_options_mut(|to| to.feathering = false);

    egui_extras::install_image_loaders(ctx);

    if light {
        ctx.set_visuals(light_mode())
    } else {
        ctx.set_visuals(dark_mode(is_mobile));
    }

    ctx.set_style(if is_mobile {
        create_custom_style(ctx, mobile_font_size)
    } else {
        create_custom_style(ctx, desktop_font_size)
    });
}
