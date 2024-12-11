use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, Rounding, Shadow, Stroke, Visuals,
};

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

const WIDGET_ROUNDING: Rounding = Rounding::same(8.0);

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
        window_rounding: Rounding::same(8.0),
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
        image_loading_spinners: false,
        ..default
    }
}
