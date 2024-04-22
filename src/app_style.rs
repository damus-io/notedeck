use crate::colors::{
    desktop_dark_color_theme, light_color_theme, mobile_dark_color_theme, ColorTheme,
};
use egui::{
    epaint::Shadow,
    style::{Interaction, Selection, WidgetVisuals, Widgets},
    Button, Context, FontFamily, FontId, Rounding, Stroke, Style, TextStyle, Ui, Visuals,
};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

const WIDGET_ROUNDING: Rounding = Rounding::same(8.0);

pub fn light_mode() -> Visuals {
    create_themed_visuals(light_color_theme(), Visuals::light())
}

pub fn dark_mode(mobile: bool) -> Visuals {
    create_themed_visuals(
        if mobile {
            mobile_dark_color_theme()
        } else {
            desktop_dark_color_theme()
        },
        Visuals::dark(),
    )
}

pub fn user_requested_visuals_change(
    mobile: bool,
    cur_darkmode: bool,
    ui: &mut Ui,
) -> Option<Visuals> {
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
        return Some(dark_mode(mobile));
    }
    None
}

/// Create custom text sizes for any FontSizes
pub fn create_custom_style(ctx: &Context, font_size: fn(&NotedeckTextStyle) -> f32) -> Style {
    let mut style = (*ctx.style()).clone();

    style.text_styles = NotedeckTextStyle::iter()
        .map(|text_style| {
            (
                text_style.text_style(),
                FontId::new(font_size(&text_style), text_style.font_family()),
            )
        })
        .collect();

    style.interaction = Interaction {
        tooltip_delay: 0.0,
        ..Interaction::default()
    };

    style
}

pub fn desktop_font_size(text_style: &NotedeckTextStyle) -> f32 {
    match text_style {
        NotedeckTextStyle::Heading => 48.0,
        NotedeckTextStyle::Heading2 => 24.0,
        NotedeckTextStyle::Heading3 => 20.0,
        NotedeckTextStyle::Body => 16.0,
        NotedeckTextStyle::Monospace => 13.0,
        NotedeckTextStyle::Button => 13.0,
        NotedeckTextStyle::Small => 12.0,
    }
}

pub fn mobile_font_size(text_style: &NotedeckTextStyle) -> f32 {
    // TODO: tweak text sizes for optimal mobile viewing
    match text_style {
        NotedeckTextStyle::Heading => 48.0,
        NotedeckTextStyle::Heading2 => 24.0,
        NotedeckTextStyle::Heading3 => 20.0,
        NotedeckTextStyle::Body => 13.0,
        NotedeckTextStyle::Monospace => 13.0,
        NotedeckTextStyle::Button => 13.0,
        NotedeckTextStyle::Small => 12.0,
    }
}

#[derive(EnumIter)]
pub enum NotedeckTextStyle {
    Heading,
    Heading2,
    Heading3,
    Body,
    Monospace,
    Button,
    Small,
}

impl NotedeckTextStyle {
    pub fn text_style(&self) -> TextStyle {
        match self {
            Self::Heading => TextStyle::Heading,
            Self::Heading2 => TextStyle::Name("Heading2".into()),
            Self::Heading3 => TextStyle::Name("Heading3".into()),
            Self::Body => TextStyle::Body,
            Self::Monospace => TextStyle::Monospace,
            Self::Button => TextStyle::Button,
            Self::Small => TextStyle::Small,
        }
    }

    pub fn font_family(&self) -> FontFamily {
        match self {
            Self::Heading => FontFamily::Proportional,
            Self::Heading2 => FontFamily::Proportional,
            Self::Heading3 => FontFamily::Proportional,
            Self::Body => FontFamily::Proportional,
            Self::Monospace => FontFamily::Monospace,
            Self::Button => FontFamily::Proportional,
            Self::Small => FontFamily::Proportional,
        }
    }
}

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
