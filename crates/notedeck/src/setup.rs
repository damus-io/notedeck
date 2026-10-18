use crate::fonts;
use crate::theme;
use egui::ThemePreference;

/// Install notedeck's look on `ctx`: the dark and light themes, fonts, text
/// sizes, image loaders and zoom. The dark theme has a black ground on every
/// platform; only a mobile build gets the larger mobile text sizes.
pub fn setup_egui_context(ctx: &egui::Context, theme: ThemePreference, zoom_factor: f32) {
    let is_mobile = crate::ui::is_compiled_as_mobile();

    ctx.options_mut(|o| {
        tracing::info!("Loaded theme {:?} from disk", theme);
        o.theme_preference = theme;
    });
    let dark_theme = theme::dark_color_theme();
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
