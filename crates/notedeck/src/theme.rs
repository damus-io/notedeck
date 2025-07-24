use egui::{
    Color32, CornerRadius, Stroke, Visuals,
    style::{Selection, WidgetVisuals, Widgets},
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
