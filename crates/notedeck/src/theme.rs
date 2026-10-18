use crate::{fonts, NotedeckTextStyle};
use egui::scroll_area::DragScroll;
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
/// [`PURPLE_ALT`] lifted for a dark background: the selected-text and accent
/// color of the dark themes (see [`ColorTheme::selection_text_color`]).
const LAVENDER: Color32 = Color32::from_rgb(0xC4, 0xB0, 0xF5);
/// [`PURPLE_ALT`] deepened for a light background: the selected-text and accent
/// color of the light theme (see [`ColorTheme::selection_text_color`]).
const DEEP_PURPLE: Color32 = Color32::from_rgb(0x4A, 0x2A, 0x9A);
/// How much of [`ColorTheme::selection_color`] a selection highlight lays over
/// what's behind it, so the selected text still shows through.
const SELECTION_TINT: f32 = 0.35;
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

#[derive(Clone, Copy)]
pub struct ColorTheme {
    // VISUALS
    pub panel_fill: Color32,
    pub extreme_bg_color: Color32,
    pub text_color: Color32,
    pub err_fg_color: Color32,
    pub warn_fg_color: Color32,
    pub hyperlink_color: Color32,
    /// The hue of a selection highlight. It is laid down as a see-through tint
    /// ([`SELECTION_TINT`]), so it never hides the text it highlights.
    pub selection_color: Color32,
    /// The color of selected text, and of egui's selection stroke: a focused
    /// field's outline and every accent drawn with `selection.stroke`. egui
    /// repaints selected glyphs in this color, so it must read on
    /// [`Self::selection_color`]'s tint over [`Self::panel_fill`].
    pub selection_text_color: Color32,
    /// egui's "faint accent": an additive lift for striped rows, chart
    /// backings, blockquotes and meter tracks. Additive, so it lifts whatever
    /// it lands on, but it has to be strong enough to show on
    /// [`Self::panel_fill`].
    pub faint_bg_color: Color32,

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

    // SEMANTIC: Surfaces
    /// The ground a pane or page is drawn on: the same as
    /// [`Self::panel_fill`], so a frame filled with it sits flush with the
    /// panel and a ring stroked in it knocks out to the background.
    pub surface_primary: Color32,
    pub surface_secondary: Color32,
    pub surface_elevated: Color32,

    // SEMANTIC: Text
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,

    // SEMANTIC: Actions
    pub accent: Color32,
    pub destructive: Color32,
    pub warning: Color32,
    pub success: Color32,

    // SEMANTIC: Borders
    pub border_default: Color32,
    pub border_strong: Color32,

    // SEMANTIC: Interactive states
    pub interactive_hover: Color32,
    pub interactive_pressed: Color32,
}

const THEME_LIGHT_ID: &str = "notedeck_color_theme_light";
const THEME_DARK_ID: &str = "notedeck_color_theme_dark";

impl ColorTheme {
    /// Store light and dark themes in egui context for later retrieval.
    pub fn store_themes(ctx: &egui::Context, light: ColorTheme, dark: ColorTheme) {
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique(THEME_LIGHT_ID), light);
            d.insert_temp(egui::Id::unique(THEME_DARK_ID), dark);
        });
    }

    /// Retrieve the active ColorTheme from egui context.
    /// Falls back to reconstructing the theme if not stored.
    pub fn current(ctx: &egui::Context) -> ColorTheme {
        let is_dark = ctx.global_style().visuals.dark_mode;
        let id = if is_dark {
            THEME_DARK_ID
        } else {
            THEME_LIGHT_ID
        };
        ctx.data(|d| d.get_temp(egui::Id::unique(id)))
            .unwrap_or_else(|| {
                if is_dark {
                    dark_color_theme()
                } else {
                    light_color_theme()
                }
            })
    }
}

const WIDGET_CORNER_RADIUS: CornerRadius = CornerRadius::same(8);

/// The selection hue at full strength, for a selected row, a status pill or a
/// tint a caller mixes itself. `visuals.selection.bg_fill` is a see-through
/// tint for egui's text selection (see [`SELECTION_TINT`]); filling with it
/// would draw a faint wash, and tinting it again a fainter one.
pub fn selection_fill(visuals: &Visuals) -> Color32 {
    let [r, g, b, _] = visuals.selection.bg_fill.to_srgba_unmultiplied();
    Color32::from_rgb(r, g, b)
}

pub fn create_themed_visuals(theme: ColorTheme, default: Visuals) -> Visuals {
    Visuals {
        hyperlink_color: theme.hyperlink_color,
        override_text_color: Some(theme.text_color),
        panel_fill: theme.panel_fill,
        selection: Selection {
            bg_fill: theme.selection_color.gamma_multiply(SELECTION_TINT),
            stroke: Stroke {
                width: 1.0,
                color: theme.selection_text_color,
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
        faint_bg_color: theme.faint_bg_color,
        error_fg_color: theme.err_fg_color,
        image_loading_spinners: false,
        ..default
    }
}

/// Notedeck's one dark theme: a black ground (kind to OLED screens), with
/// popups, inputs and surfaces drawn as gray layers above it.
pub fn dark_color_theme() -> ColorTheme {
    ColorTheme {
        // VISUALS
        panel_fill: Color32::BLACK,
        extreme_bg_color: SEMI_DARKER_BG,
        text_color: Color32::WHITE,
        err_fg_color: RED_700,
        warn_fg_color: ORANGE_700,
        hyperlink_color: PURPLE,
        selection_color: PURPLE_ALT,
        selection_text_color: LAVENDER,
        // egui's own 5 was tuned for its gray ground and is all but invisible
        // on black; 9 is the same perceived step above black (ΔL* ≈ 2.4).
        faint_bg_color: Color32::from_additive_luminance(9),

        // WINDOW
        window_fill: DARK_ISH_BG,
        window_stroke_color: DARK_BG,

        // NONINTERACTIVE WIDGET
        noninteractive_bg_fill: DARK_ISH_BG,
        noninteractive_weak_bg_fill: DARKER_BG,
        noninteractive_bg_stroke_color: SEMI_DARKER_BG,
        noninteractive_fg_stroke_color: GRAY_SECONDARY,

        // INACTIVE WIDGET
        inactive_bg_stroke_color: SEMI_DARKER_BG,
        inactive_bg_fill: Color32::from_rgb(0x25, 0x25, 0x25),
        inactive_weak_bg_fill: SEMI_DARK_BG,

        // SEMANTIC: Surfaces
        // The primary surface is the ground, as in the light theme.
        surface_primary: Color32::BLACK,
        surface_secondary: DARK_BG,
        surface_elevated: SEMI_DARK_BG,

        // SEMANTIC: Text
        text_primary: Color32::WHITE,
        text_secondary: LIGHT_GRAY,
        text_muted: GRAY_SECONDARY,

        // SEMANTIC: Actions
        accent: PURPLE,
        destructive: RED_700,
        warning: ORANGE_700,
        success: Color32::from_rgb(0x4C, 0xAF, 0x50),

        // SEMANTIC: Borders
        border_default: SEMI_DARKER_BG,
        border_strong: EVEN_DARKER_GRAY,

        // SEMANTIC: Interactive states
        interactive_hover: Color32::from_rgb(0x46, 0x46, 0x46),
        interactive_pressed: Color32::from_rgb(0x50, 0x50, 0x50),
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
        selection_text_color: DEEP_PURPLE,
        faint_bg_color: Color32::from_additive_luminance(5),

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

        // SEMANTIC: Surfaces
        surface_primary: Color32::WHITE,
        surface_secondary: LIGHTER_GRAY,
        surface_elevated: Color32::WHITE,

        // SEMANTIC: Text
        text_primary: BLACK,
        text_secondary: EVEN_DARKER_GRAY,
        text_muted: GRAY_SECONDARY,

        // SEMANTIC: Actions
        accent: PURPLE,
        destructive: RED_700,
        warning: ORANGE_700,
        success: Color32::from_rgb(0x2E, 0x7D, 0x32),

        // SEMANTIC: Borders
        border_default: LIGHT_GRAY,
        border_strong: DARKER_GRAY,

        // SEMANTIC: Interactive states
        interactive_hover: Color32::from_rgb(0xDC, 0xDC, 0xDC),
        interactive_pressed: Color32::from_rgb(0xCC, 0xCC, 0xCC),
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
        // egui only drag-scrolls on touch screens by default; keep
        // click-and-drag scrolling with a mouse on desktop too.
        drag_to_scroll: DragScroll::Always,
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

pub fn light_mode() -> Visuals {
    create_themed_visuals(crate::theme::light_color_theme(), Visuals::light())
}

/// The egui visuals of [`dark_color_theme`].
pub fn dark_mode() -> Visuals {
    create_themed_visuals(dark_color_theme(), Visuals::dark())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an opaque color.
    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    /// WCAG contrast ratio between two opaque colors.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// A premultiplied `over` laid on an opaque `under`, as the painter blends it.
    fn composite(over: Color32, under: Color32) -> Color32 {
        let keep = 1.0 - over.a() as f32 / 255.0;
        let mix = |o: u8, u: u8| (o as f32 + u as f32 * keep).round() as u8;
        Color32::from_rgb(
            mix(over.r(), under.r()),
            mix(over.g(), under.g()),
            mix(over.b(), under.b()),
        )
    }

    /// egui ≥ 0.32 repaints selected glyphs in `selection.stroke.color` on top
    /// of `selection.bg_fill`. When both were the same opaque purple, selecting
    /// text hid it. In every theme the highlight must be see-through and the
    /// selected text must read on it.
    #[test]
    fn selected_text_reads_on_its_highlight() {
        let themes = [("dark", dark_color_theme()), ("light", light_color_theme())];
        for (name, theme) in themes {
            let visuals = create_themed_visuals(theme, Visuals::default());
            let highlight = visuals.selection.bg_fill;
            assert!(highlight.a() < 255, "{name}: highlight is opaque");

            let behind = composite(highlight, theme.panel_fill);
            let ratio = contrast(visuals.selection.stroke.color, behind);
            assert!(
                ratio >= 4.5,
                "{name}: selected text has contrast {ratio:.2} on its highlight"
            );
        }
    }

    /// Perceived lightness (CIE L*, 0–100) of an opaque color.
    fn lightness(c: Color32) -> f32 {
        let y = luminance(c);
        if y <= 0.008_856 {
            903.3 * y
        } else {
            116.0 * y.cbrt() - 16.0
        }
    }

    /// The primary surface is the ground in every theme. Panes, pages and
    /// knockout rings fill with `surface_primary` expecting to sit flush with
    /// the panel; a gray one left over from the old gray dark ground drew
    /// them as gray islands on black.
    #[test]
    fn primary_surface_is_the_ground() {
        for (name, theme) in [("dark", dark_color_theme()), ("light", light_color_theme())] {
            assert_eq!(theme.surface_primary, theme.panel_fill, "{name}");
        }
    }

    /// egui's faint accent must still show on the black ground: at least the
    /// step above it that egui's default (5) gave on the old #1F1F1F ground.
    #[test]
    fn faint_accent_shows_on_the_dark_ground() {
        let theme = dark_color_theme();
        let visuals = create_themed_visuals(theme, Visuals::dark());
        let lifted = composite(visuals.faint_bg_color, theme.panel_fill);
        let step = lightness(lifted) - lightness(theme.panel_fill);

        let gray = Color32::from_gray(0x1F);
        let old = composite(Color32::from_additive_luminance(5), gray);
        let old_step = lightness(old) - lightness(gray);
        assert!(
            step >= old_step,
            "faint accent lifts black by ΔL* {step:.2}, the gray ground got {old_step:.2}"
        );
    }

    /// A caller filling a row with the selection color gets the theme's hue
    /// at full strength, not the see-through text-selection tint.
    #[test]
    fn selection_fill_is_the_opaque_hue() {
        let visuals = create_themed_visuals(dark_color_theme(), Visuals::default());
        let fill = selection_fill(&visuals);
        assert_eq!(fill.a(), 255);
        let near = |a: u8, b: u8| a.abs_diff(b) <= 2;
        assert!(
            near(fill.r(), PURPLE_ALT.r())
                && near(fill.g(), PURPLE_ALT.g())
                && near(fill.b(), PURPLE_ALT.b()),
            "{fill:?} is not PURPLE_ALT"
        );
    }
}
