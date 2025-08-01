use crate::{fonts, NotedeckTextStyle};
use egui::style::Interaction;
use egui::style::Selection;
use egui::style::WidgetVisuals;
use egui::style::Widgets;
use egui::Color32;
use egui::CornerRadius;
use egui::FontId;
use egui::Stroke;
use egui::Style;
use egui::Visuals;
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

pub struct ColorTheme {
    // VISUALS
    pub panel_fill: Color32,
    pub extreme_bg_color: Color32,
    pub text_color: Color32,
    pub err_fg_color: Color32,
    pub warn_fg_color: Color32,
    pub hyperlink_color: Color32,
    pub selection_color: Color32,

    // WINDOW
    pub window_fill: Color32,
    pub window_stroke_color: Color32,

    // NONINTERACTIVE WIDGET
    pub noninteractive_bg_fill: Color32,
    pub noninteractive_weak_bg_fill: Color32,
    pub noninteractive_bg_stroke_color: Color32,
    pub noninteractive_fg_stroke_color: Color32,

    // INACTIVE WIDGET
    pub inactive_bg_stroke_color: Color32,
    pub inactive_bg_fill: Color32,
    pub inactive_weak_bg_fill: Color32,
}

const WIDGET_CORNER_RADIUS: CornerRadius = CornerRadius::same(8);

pub fn create_themed_visuals(theme: ColorTheme, default: Visuals) -> Visuals {
    Visuals {
        hyperlink_color: theme.hyperlink_color,
        override_text_color: Some(theme.text_color),
        panel_fill: theme.panel_fill,
        selection: Selection {
            bg_fill: theme.selection_color,
            stroke: Stroke {
                width: 1.0,
                color: theme.selection_color,
            },
        },
        warn_fg_color: theme.warn_fg_color,
        widgets: Widgets {
            noninteractive: WidgetVisuals {
                bg_fill: theme.noninteractive_bg_fill,
                weak_bg_fill: theme.noninteractive_weak_bg_fill,
                bg_stroke: Stroke {
                    width: 1.0,
                    color: theme.noninteractive_bg_stroke_color,
                },
                fg_stroke: Stroke {
                    width: 1.0,
                    color: theme.noninteractive_fg_stroke_color,
                },
                ..default.widgets.noninteractive
            },
            inactive: WidgetVisuals {
                bg_fill: theme.inactive_bg_fill,
                weak_bg_fill: theme.inactive_weak_bg_fill,
                bg_stroke: Stroke {
                    width: 1.0,
                    color: theme.inactive_bg_stroke_color,
                },
                corner_radius: WIDGET_CORNER_RADIUS,
                ..default.widgets.inactive
            },
            hovered: WidgetVisuals {
                corner_radius: WIDGET_CORNER_RADIUS,
                ..default.widgets.hovered
            },
            active: WidgetVisuals {
                corner_radius: WIDGET_CORNER_RADIUS,
                ..default.widgets.active
            },
            open: WidgetVisuals {
                ..default.widgets.open
            },
        },
        extreme_bg_color: theme.extreme_bg_color,
        error_fg_color: theme.err_fg_color,
        image_loading_spinners: false,
        ..default
    }
}

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

/// Create custom text sizes for any FontSizes
pub fn add_custom_style(is_mobile: bool, style: &mut Style) {
    let font_size = if is_mobile {
        fonts::mobile_font_size
    } else {
        fonts::desktop_font_size
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
    /*
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
    */
}

pub fn light_mode() -> Visuals {
    create_themed_visuals(crate::theme::light_color_theme(), Visuals::light())
}

pub fn dark_mode(is_oled: bool) -> Visuals {
    create_themed_visuals(
        if is_oled {
            mobile_dark_color_theme()
        } else {
            desktop_dark_color_theme()
        },
        Visuals::dark(),
    )
}
