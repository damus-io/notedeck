use crate::colors::{dark_color_theme, light_color_theme, ColorTheme, DarkTheme, LightTheme};
use egui::{
    epaint::Shadow,
    style::{WidgetVisuals, Widgets},
    Button, Context, Rounding, Stroke, Style, Ui, Visuals,
};

const WIDGET_ROUNDING: Rounding = Rounding::same(8.0);

pub fn light_mode() -> Visuals {
    create_themed_visuals(light_color_theme(), Visuals::light())
}

pub fn dark_mode() -> Visuals {
    create_themed_visuals(dark_color_theme(), Visuals::dark())
}

pub fn user_requested_visuals_change(cur_darkmode: bool, ui: &mut Ui) -> Option<Visuals> {
    if cur_darkmode {
        if ui
            .add(Button::new("☀").frame(false))
            .on_hover_text("Switch to light mode")
            .clicked()
        {
            return Some(light_mode());
        }
    } else if ui
        .add(Button::new("🌙").frame(false))
        .on_hover_text("Switch to dark mode")
        .clicked()
    {
        return Some(dark_mode());
    }
    None
}

pub fn create_themed_visuals(theme: ColorTheme, default: Visuals) -> Visuals {
    Visuals {
        hyperlink_color: theme.hyperlink_color,
        override_text_color: Some(theme.text_color),
        panel_fill: theme.panel_fill,
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
                rounding: WIDGET_ROUNDING,
                ..default.widgets.noninteractive
            },
            inactive: WidgetVisuals {
                bg_fill: theme.inactive_bg_fill,
                weak_bg_fill: theme.inactive_weak_bg_fill,
                bg_stroke: Stroke {
                    width: 1.0,
                    color: theme.inactive_bg_stroke_color,
                },
                rounding: WIDGET_ROUNDING,
                ..default.widgets.inactive
            },
            hovered: WidgetVisuals {
                rounding: WIDGET_ROUNDING,
                ..default.widgets.hovered
            },
            active: WidgetVisuals {
                rounding: WIDGET_ROUNDING,
                ..default.widgets.active
            },
            open: WidgetVisuals {
                ..default.widgets.open
            },
        },
        extreme_bg_color: theme.extreme_bg_color,
        error_fg_color: theme.err_fg_color,
        window_rounding: Rounding::same(32.0),
        window_fill: theme.window_fill,
        window_shadow: Shadow {
            offset: [0.0, 8.0].into(),
            blur: 24.0,
            spread: 0.0,
            color: egui::Color32::from_rgba_unmultiplied(0x6D, 0x6D, 0x6D, 0x14),
        },
        window_stroke: Stroke {
            width: 1.0,
            color: theme.window_stroke_color,
        },
        ..default
    }
}
