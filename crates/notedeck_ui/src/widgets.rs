use crate::anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE};
use crate::icons::search_icon;
use egui::{emath::GuiRounding, Align, CornerRadius, Pos2, RichText, Stroke, TextEdit};
use notedeck::tokens::{RADIUS_MD, RADIUS_PILL, SPACING_SM, STROKE_THIN};
use notedeck::{ColorTheme, NotedeckTextStyle};

pub fn x_button(rect: egui::Rect) -> impl egui::Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_width = rect.width();
        let helper = AnimationHelper::new_from_rect(ui, "user_search_close", rect);

        let fill_color = ui.visuals().text_color();

        let radius = max_width / (2.0 * ICON_EXPANSION_MULTIPLE);

        let painter = ui.painter();
        let ppp = ui.ctx().pixels_per_point();
        let nw_edge = helper
            .scale_pos_from_center(Pos2::new(-radius, radius))
            .round_to_pixel_center(ppp);
        let se_edge = helper
            .scale_pos_from_center(Pos2::new(radius, -radius))
            .round_to_pixel_center(ppp);
        let sw_edge = helper
            .scale_pos_from_center(Pos2::new(-radius, -radius))
            .round_to_pixel_center(ppp);
        let ne_edge = helper
            .scale_pos_from_center(Pos2::new(radius, radius))
            .round_to_pixel_center(ppp);

        let line_width = helper.scale_1d_pos(2.0);

        painter.line_segment([nw_edge, se_edge], Stroke::new(line_width, fill_color));
        painter.line_segment([ne_edge, sw_edge], Stroke::new(line_width, fill_color));

        helper.take_animation_response()
    }
}

/// Button styled in the Notedeck theme
pub fn styled_button_toggleable(
    text: &str,
    fill_color: egui::Color32,
    enabled: bool,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        let text_color = if ui.visuals().dark_mode {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };

        let galley = painter.layout(
            text.to_owned(),
            NotedeckTextStyle::Button.get_font_id(ui.ctx()),
            text_color,
            ui.available_width(),
        );

        let size = galley.rect.expand2(egui::vec2(16.0, SPACING_SM)).size();
        let mut button = egui::Button::new(galley).corner_radius(RADIUS_MD);

        if !enabled {
            button = button
                .sense(egui::Sense::focusable_noninteractive())
                .fill(ui.visuals().noninteractive().bg_fill)
                .stroke(ui.visuals().noninteractive().bg_stroke);
        } else {
            button = button.fill(fill_color);
        }

        let mut resp = ui.add_sized(size, button);

        if !enabled {
            resp = resp.on_hover_cursor(egui::CursorIcon::NotAllowed);
        }

        resp
    }
}

/// Get appropriate background color for active side panel icon button
pub fn side_panel_active_bg(ui: &egui::Ui) -> egui::Color32 {
    ColorTheme::current(ui.ctx()).interactive_hover
}

/// Get appropriate tint color for side panel icons to ensure visibility
pub fn side_panel_icon_tint(ui: &egui::Ui) -> egui::Color32 {
    ColorTheme::current(ui.ctx()).text_primary
}

/// Returns a styled Frame for search input boxes with rounded corners.
pub fn search_input_frame(ctx: &egui::Context) -> egui::Frame {
    let theme = ColorTheme::current(ctx);
    egui::Frame {
        inner_margin: egui::Margin::symmetric(SPACING_SM as i8, 0),
        outer_margin: egui::Margin::ZERO,
        corner_radius: CornerRadius::same(RADIUS_PILL as u8),
        shadow: Default::default(),
        fill: theme.surface_secondary,
        stroke: Stroke::new(STROKE_THIN, theme.border_default),
    }
}

/// The standard height for search input boxes.
pub const SEARCH_INPUT_HEIGHT: f32 = 34.0;

/// A styled search input box with rounded corners and search icon.
pub fn search_input_box<'a>(query: &'a mut String, hint_text: &'a str) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        ui.horizontal(|ui| {
            search_input_frame(ui.ctx())
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(SPACING_SM, 0.0);

                        ui.add(search_icon(notedeck::tokens::ICON_SM, SEARCH_INPUT_HEIGHT));

                        let response = ui.add_sized(
                            [ui.available_width(), SEARCH_INPUT_HEIGHT],
                            TextEdit::singleline(query)
                                .hint_text(RichText::new(hint_text).weak())
                                .margin(egui::vec2(0.0, 8.0))
                                .frame(false),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, hint_text)
                        });
                        response
                    })
                    .inner
                })
                .inner
        })
        .response
    }
}
