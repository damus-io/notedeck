use crate::anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE};
use egui::{emath::GuiRounding, Pos2, Stroke};
use notedeck::NotedeckTextStyle;

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

        let size = galley.rect.expand2(egui::vec2(16.0, 8.0)).size();
        let mut button = egui::Button::new(galley).corner_radius(8.0);

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
    if ui.visuals().dark_mode {
        egui::Color32::from_rgb(70, 70, 70)
    } else {
        egui::Color32::from_rgb(220, 220, 220)
    }
}

/// Get appropriate tint color for side panel icons to ensure visibility
pub fn side_panel_icon_tint(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    }
}
