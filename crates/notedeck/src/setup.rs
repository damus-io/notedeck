use crate::fonts;
use crate::theme;
use crate::NotedeckOptions;
use egui::ThemePreference;

pub fn setup_egui_context(
    ctx: &egui::Context,
    options: NotedeckOptions,
    theme: ThemePreference,
    zoom_factor: f32,
) {
    let is_mobile = options.contains(NotedeckOptions::Mobile) || crate::ui::is_compiled_as_mobile();
    let is_oled = crate::ui::is_oled(is_mobile);

    ctx.options_mut(|o| {
        tracing::info!("Loaded theme {:?} from disk", theme);
        o.theme_preference = theme;
    });
    let dark_theme = if is_oled {
        theme::mobile_dark_color_theme()
    } else {
        theme::desktop_dark_color_theme()
    };
    let light_theme = theme::light_color_theme();

    ctx.set_visuals_of(
        egui::Theme::Dark,
        theme::create_themed_visuals(dark_theme, egui::Visuals::dark()),
    );
    ctx.set_visuals_of(
        egui::Theme::Light,
        theme::create_themed_visuals(light_theme, egui::Visuals::light()),
    );

    crate::ColorTheme::store_themes(ctx, light_theme, dark_theme);

    fonts::setup_fonts(ctx);

    if crate::ui::is_compiled_as_mobile() {
        ctx.set_pixels_per_point(ctx.pixels_per_point() + 0.2);
    }

    egui_extras::install_image_loaders(ctx);

    ctx.options_mut(|o| {
        o.input_options.max_click_duration = 0.4;
    });
    ctx.all_styles_mut(|style| crate::theme::add_custom_style(is_mobile, style));

    ctx.set_zoom_factor(zoom_factor);
}
