use egui::{style::Interaction, Color32, FontId, Style, Visuals};
use notedeck::{ColorTheme, NotedeckTextStyle};
use strum::IntoEnumIterator;

pub const PURPLE: Color32 = Color32::from_rgb(0xCC, 0x43, 0xC5);
const PURPLE_ALT: Color32 = Color32::from_rgb(0x82, 0x56, 0xDD);
//pub const DARK_BG: Color32 = egui::Color32::from_rgb(40, 44, 52);
pub const GRAY_SECONDARY: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);
const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
const RED_700: Color32 = Color32::from_rgb(0xC7, 0x37, 0x5A);
const ORANGE_700: Color32 = Color32::from_rgb(0xF6, 0xB1, 0x4A);

// BACKGROUNDS
const SEMI_DARKER_BG: Color32 = Color32::from_rgb(0x39, 0x39, 0x39);
const DARKER_BG: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x1F);
const DARK_BG: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
const DARK_ISH_BG: Color32 = Color32::from_rgb(0x25, 0x25, 0x25);
const SEMI_DARK_BG: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);

const LIGHTER_GRAY: Color32 = Color32::from_rgb(0xf8, 0xf8, 0xf8);
const LIGHT_GRAY: Color32 = Color32::from_rgb(0xc8, 0xc8, 0xc8); // 78%
const DARKER_GRAY: Color32 = Color32::from_rgb(0xa5, 0xa5, 0xa5); // 65%
const EVEN_DARKER_GRAY: Color32 = Color32::from_rgb(0x89, 0x89, 0x89); // 54%

pub fn desktop_dark_color_theme() -> ColorTheme {
    ColorTheme {
        // VISUALS
        panel_fill: DARKER_BG,
        extreme_bg_color: DARK_ISH_BG,
        text_color: Color32::WHITE,
        err_fg_color: RED_700,
        warn_fg_color: ORANGE_700,
        hyperlink_color: PURPLE,
        selection_color: PURPLE_ALT,

        // WINDOW
        window_fill: DARK_ISH_BG,
        window_stroke_color: DARK_BG,

        // NONINTERACTIVE WIDGET
        noninteractive_bg_fill: DARK_ISH_BG,
        noninteractive_weak_bg_fill: DARK_BG,
        noninteractive_bg_stroke_color: SEMI_DARKER_BG,
        noninteractive_fg_stroke_color: GRAY_SECONDARY,

        // INACTIVE WIDGET
        inactive_bg_stroke_color: SEMI_DARKER_BG,
        inactive_bg_fill: Color32::from_rgb(0x25, 0x25, 0x25),
        inactive_weak_bg_fill: SEMI_DARK_BG,
    }
}

pub fn mobile_dark_color_theme() -> ColorTheme {
    ColorTheme {
        panel_fill: Color32::BLACK,
        noninteractive_weak_bg_fill: Color32::from_rgb(0x1F, 0x1F, 0x1F),
        ..desktop_dark_color_theme()
    }
}

pub fn light_color_theme() -> ColorTheme {
    ColorTheme {
        // VISUALS
        panel_fill: Color32::WHITE,
        extreme_bg_color: LIGHTER_GRAY,
        text_color: BLACK,
        err_fg_color: RED_700,
        warn_fg_color: ORANGE_700,
        hyperlink_color: PURPLE,
        selection_color: PURPLE_ALT,

        // WINDOW
        window_fill: Color32::WHITE,
        window_stroke_color: DARKER_GRAY,

        // NONINTERACTIVE WIDGET
        noninteractive_bg_fill: Color32::WHITE,
        noninteractive_weak_bg_fill: LIGHTER_GRAY,
        noninteractive_bg_stroke_color: LIGHT_GRAY,
        noninteractive_fg_stroke_color: GRAY_SECONDARY,

        // INACTIVE WIDGET
        inactive_bg_stroke_color: EVEN_DARKER_GRAY,
        inactive_bg_fill: LIGHTER_GRAY,
        inactive_weak_bg_fill: LIGHTER_GRAY,
    }
}

pub fn light_mode() -> Visuals {
    notedeck::theme::create_themed_visuals(light_color_theme(), Visuals::light())
}

pub fn dark_mode(is_oled: bool) -> Visuals {
    notedeck::theme::create_themed_visuals(
        if is_oled {
            mobile_dark_color_theme()
        } else {
            desktop_dark_color_theme()
        },
        Visuals::dark(),
    )
}

/// Create custom text sizes for any FontSizes
pub fn add_custom_style(is_mobile: bool, style: &mut Style) {
    let font_size = if is_mobile {
        notedeck::fonts::mobile_font_size
    } else {
        notedeck::fonts::desktop_font_size
    };

    style.text_styles = NotedeckTextStyle::iter()
        .map(|text_style| {
            (
                text_style.text_style(),
                FontId::new(font_size(&text_style), text_style.font_family()),
            )
        })
        .collect();

    style.interaction = Interaction {
        tooltip_delay: 0.1,
        show_tooltips_only_when_still: false,
        ..Interaction::default()
    };

    // debug: show callstack for the current widget on hover if all
    // modifier keys are pressed down.
    #[cfg(feature = "debug-widget-callstack")]
    {
        #[cfg(not(debug_assertions))]
        compile_error!(
            "The `debug-widget-callstack` feature requires a debug build, \
             release builds are unsupported."
        );
        style.debug.debug_on_hover_with_all_modifiers = true;
    }

    // debug: show an overlay on all interactive widgets
    #[cfg(feature = "debug-interactive-widgets")]
    {
        #[cfg(not(debug_assertions))]
        compile_error!(
            "The `debug-interactive-widgets` feature requires a debug build, \
             release builds are unsupported."
        );
        style.debug.show_interactive_widgets = true;
    }
}
