use crate::{
    app_size_handler::AppSizeHandler,
    app_style::{create_custom_style, dark_mode, desktop_font_size, light_mode, mobile_font_size},
    fonts::setup_fonts,
    storage::DataPath,
};

use eframe::NativeOptions;

//pub const UI_SCALE_FACTOR: f32 = 0.2;

pub fn generate_native_options(paths: DataPath) -> NativeOptions {
    let window_builder = Box::new(move |builder: egui::ViewportBuilder| {
        let builder = builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(app_icon()).expect("icon"),
            ));

        if let Some(window_size) = AppSizeHandler::new(&paths).get_app_size() {
            builder.with_inner_size(window_size)
        } else {
            builder
        }
    });

    eframe::NativeOptions {
        window_builder: Some(window_builder),
        viewport: egui::ViewportBuilder::default().with_icon(std::sync::Arc::new(
            eframe::icon_data::from_png_bytes(app_icon()).expect("icon"),
        )),
        ..Default::default()
    }
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

pub fn app_icon() -> &'static [u8; 192739] {
    std::include_bytes!("../assets/damus_rounded_256.png")
}

pub fn generate_mobile_emulator_native_options() -> eframe::NativeOptions {
    generate_native_options_with_builder_modifiers(|builder| {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_inner_size([405.0, 915.0])
            .with_icon(eframe::icon_data::from_png_bytes(app_icon()).expect("icon"))
    })
}

pub fn setup_cc(ctx: &egui::Context, is_mobile: bool, light: bool) {
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
