use crate::{fonts, theme};

use eframe::NativeOptions;
use egui::ThemePreference;
use notedeck::{AppSizeHandler, DataPath};
use tracing::info;

pub fn setup_chrome(ctx: &egui::Context, args: &notedeck::Args, theme: ThemePreference) {
    let is_mobile = args
        .is_mobile
        .unwrap_or(notedeck::ui::is_compiled_as_mobile());

    let is_oled = notedeck::ui::is_oled();

    // Some people have been running notedeck in debug, let's catch that!
    if !args.tests && cfg!(debug_assertions) && !args.debug {
        println!("--- WELCOME TO DAMUS NOTEDECK! ---");
        println!("It looks like are running notedeck in debug mode, unless you are a developer, this is not likely what you want.");
        println!("If you are a developer, run `cargo run -- --debug` to skip this message.");
        println!("For everyone else, try again with `cargo run --release`. Enjoy!");
        println!("---------------------------------");
        panic!();
    }

    ctx.options_mut(|o| {
        info!("Loaded theme {:?} from disk", theme);
        o.theme_preference = theme;
    });
    ctx.set_visuals_of(egui::Theme::Dark, theme::dark_mode(is_oled));
    ctx.set_visuals_of(egui::Theme::Light, theme::light_mode());
    setup_cc(ctx, is_mobile);
}

pub fn setup_cc(ctx: &egui::Context, is_mobile: bool) {
    fonts::setup_fonts(ctx);

    if notedeck::ui::is_compiled_as_mobile() {
        ctx.set_pixels_per_point(ctx.pixels_per_point() + 0.2);
    }
    //ctx.set_pixels_per_point(1.0);
    //
    //
    //ctx.tessellation_options_mut(|to| to.feathering = false);

    egui_extras::install_image_loaders(ctx);

    ctx.all_styles_mut(|style| theme::add_custom_style(is_mobile, style));
}

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
        // for 3d widgets
        depth_buffer: 24,
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
        // for 3d widgets
        depth_buffer: 24,
        window_builder: Some(window_builder),
        ..Default::default()
    }
}

pub fn app_icon() -> &'static [u8; 271986] {
    std::include_bytes!("../../../assets/damus-app-icon.png")
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
