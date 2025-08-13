use egui::{vec2, Color32, Stroke};

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
