use egui::{vec2, Color32, Stroke, Widget};
use notedeck_ui::anim::AnimationHelper;

static ICON_WIDTH: f32 = 40.0;
static ICON_EXPANSION_MULTIPLE: f32 = 1.2;

pub fn compose_note_button(dark_mode: bool) -> impl Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget

        let min_outer_circle_diameter = 40.0;
        let min_plus_sign_size = 14.0; // length of the plus sign
        let min_line_width = 2.25; // width of the plus sign

        let helper = AnimationHelper::new(ui, "note-compose-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());

        let use_background_radius = helper.scale_radius(min_outer_circle_diameter);
        let use_line_width = helper.scale_1d_pos(min_line_width);
        let use_edge_circle_radius = helper.scale_radius(min_line_width);

        let text_color = ui.visuals().text_color();
        let bg_color = ui.visuals().extreme_bg_color;

        painter.circle_filled(helper.center(), use_background_radius, text_color);

        let min_half_plus_sign_size = min_plus_sign_size / 2.0;
        let north_edge = helper.scale_from_center(0.0, min_half_plus_sign_size);
        let south_edge = helper.scale_from_center(0.0, -min_half_plus_sign_size);
        let west_edge = helper.scale_from_center(-min_half_plus_sign_size, 0.0);
        let east_edge = helper.scale_from_center(min_half_plus_sign_size, 0.0);

        painter.line_segment(
            [north_edge, south_edge],
            Stroke::new(use_line_width, bg_color),
        );
        painter.line_segment(
            [west_edge, east_edge],
            Stroke::new(use_line_width, bg_color),
        );
        painter.circle_filled(north_edge, use_edge_circle_radius, bg_color);
        painter.circle_filled(south_edge, use_edge_circle_radius, bg_color);
        painter.circle_filled(west_edge, use_edge_circle_radius, bg_color);
        painter.circle_filled(east_edge, use_edge_circle_radius, bg_color);

        helper
            .take_animation_response()
            .on_hover_text("New note")
    }
}
