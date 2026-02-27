use egui::{vec2, Color32, CursorIcon, Stroke, Widget};

use crate::{app_images, AnimationHelper};

pub static ICON_WIDTH: f32 = 40.0;
pub static ICON_EXPANSION_MULTIPLE: f32 = 1.2;

/// Creates a magnifying glass icon widget
pub fn search_icon(size: f32, height: f32) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        // Use the provided height parameter
        let desired_size = vec2(size, height);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

        // Calculate center position - this ensures the icon is centered in its allocated space
        let center_pos = rect.center();
        let stroke = Stroke::new(1.5, Color32::from_rgb(150, 150, 150));

        // Draw circle
        let circle_radius = size * 0.35;
        ui.painter()
            .circle(center_pos, circle_radius, Color32::TRANSPARENT, stroke);

        // Draw handle
        let handle_start = center_pos + vec2(circle_radius * 0.7, circle_radius * 0.7);
        let handle_end = handle_start + vec2(size * 0.25, size * 0.25);
        ui.painter()
            .line_segment([handle_start, handle_end], stroke);

        response
    }
}

pub fn notifications_button(
    ui: &mut egui::Ui,
    size: f32,
    unseen_indicator: bool,
) -> egui::Response {
    expanding_button(
        "notifications-button",
        size,
        app_images::notifications_light_image(),
        app_images::notifications_dark_image(),
        ui,
        unseen_indicator,
    )
}

pub fn chat_button(ui: &mut egui::Ui, size: f32) -> egui::Response {
    expanding_button(
        "chat-button",
        size,
        app_images::chat_light_image(),
        app_images::chat_dark_image(),
        ui,
        false,
    )
}

pub fn home_button(ui: &mut egui::Ui, size: f32) -> egui::Response {
    expanding_button(
        "home-button",
        size,
        app_images::home_light_image(),
        app_images::home_dark_image(),
        ui,
        false,
    )
}

pub fn expanding_button(
    name: &'static str,
    img_size: f32,
    light_img: egui::Image,
    dark_img: egui::Image,
    ui: &mut egui::Ui,
    unseen_indicator: bool,
) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE; // max size of the widget
    let img = if ui.visuals().dark_mode {
        dark_img
    } else {
        light_img
    };

    let helper = AnimationHelper::new(ui, name, egui::vec2(max_size, max_size));

    let cur_img_size = helper.scale_1d_pos(img_size);

    let paint_rect = helper
        .get_animation_rect()
        .shrink((max_size - cur_img_size) / 2.0);
    img.paint_at(ui, paint_rect);

    if unseen_indicator {
        paint_unseen_indicator(ui, paint_rect, helper.scale_1d_pos(3.0));
    }

    helper.take_animation_response()
}

pub fn search_button(color: Color32, line_width: f32, is_active: bool) -> impl Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let min_line_width_circle = line_width;
        let min_line_width_handle = line_width;
        let helper = AnimationHelper::new(ui, "search-button", vec2(max_size, max_size));

        let painter = ui.painter_at(helper.get_animation_rect());

        if is_active {
            let circle_radius = max_size / 2.0;
            painter.circle(
                helper.get_animation_rect().center(),
                circle_radius,
                crate::side_panel_active_bg(ui),
                Stroke::NONE,
            );
        }

        let cur_line_width_circle = helper.scale_1d_pos(min_line_width_circle);
        let cur_line_width_handle = helper.scale_1d_pos(min_line_width_handle);
        let min_outer_circle_radius = helper.scale_radius(15.0);
        let cur_outer_circle_radius = helper.scale_1d_pos(min_outer_circle_radius);
        let min_handle_length = 7.0;
        let cur_handle_length = helper.scale_1d_pos(min_handle_length);

        let circle_center = helper.scale_from_center(-2.0, -2.0);

        let handle_vec = vec2(
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        );

        let handle_pos_1 = circle_center + (handle_vec * (cur_outer_circle_radius - 3.0));
        let handle_pos_2 =
            circle_center + (handle_vec * (cur_outer_circle_radius + cur_handle_length));

        let icon_color = if is_active {
            ui.visuals().strong_text_color()
        } else {
            color
        };
        let circle_stroke = Stroke::new(cur_line_width_circle, icon_color);
        let handle_stroke = Stroke::new(cur_line_width_handle, icon_color);

        painter.line_segment([handle_pos_1, handle_pos_2], handle_stroke);
        painter.circle(
            circle_center,
            min_outer_circle_radius,
            ui.style().visuals.widgets.inactive.weak_bg_fill,
            circle_stroke,
        );

        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Open search")
    }
}

fn paint_unseen_indicator(ui: &mut egui::Ui, rect: egui::Rect, radius: f32) {
    let center = rect.center();
    let top_right = rect.right_top();
    let distance = center.distance(top_right);
    let midpoint = {
        let mut cur = center;
        cur.x += distance / 2.0;
        cur.y -= distance / 2.0;
        cur
    };

    let painter = ui.painter_at(rect);
    painter.circle_filled(midpoint, radius, crate::colors::PINK);
}
