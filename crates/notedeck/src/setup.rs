use crate::fonts;
use crate::theme;
use crate::NotedeckOptions;
use crate::NotedeckTextStyle;
use egui::FontId;
use egui::ThemePreference;

pub fn setup_egui_context(
    ctx: &egui::Context,
    options: NotedeckOptions,
    theme: ThemePreference,
    note_body_font_size: f32,
    zoom_factor: f32,
) {
    let is_mobile = options.contains(NotedeckOptions::Mobile) || crate::ui::is_compiled_as_mobile();

    let is_oled = crate::ui::is_oled();

    ctx.options_mut(|o| {
        tracing::info!("Loaded theme {:?} from disk", theme);
        o.theme_preference = theme;
    });
    ctx.set_visuals_of(egui::Theme::Dark, theme::dark_mode(is_oled));
    ctx.set_visuals_of(egui::Theme::Light, theme::light_mode());

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

    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        NotedeckTextStyle::NoteBody.text_style(),
        FontId::proportional(note_body_font_size),
    );
    ctx.set_style(style);
}
