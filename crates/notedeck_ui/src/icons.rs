use egui::{pos2, vec2, Color32, CursorIcon, Pos2, Stroke, Widget};

use crate::AnimationHelper;

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

fn toolbar_icon_color(ui: &egui::Ui, is_active: bool) -> Color32 {
    if is_active {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    }
}

/// Painter-drawn bell icon for notifications (filled when active)
pub fn notifications_button(
    ui: &mut egui::Ui,
    size: f32,
    is_active: bool,
    unseen_indicator: bool,
) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
    let helper = AnimationHelper::new(ui, "notifications-button", vec2(max_size, max_size));
    let rect = helper.get_animation_rect();
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let s = helper.scale_1d_pos(size);
    let color = toolbar_icon_color(ui, is_active);
    let stroke_width = helper.scale_1d_pos(1.5);

    draw_bell(&painter, center, s, color, stroke_width, is_active);

    if unseen_indicator {
        let indicator_rect = rect.shrink((max_size - s) / 2.0);
        paint_unseen_indicator(ui, indicator_rect, helper.scale_1d_pos(3.0));
    }

    helper.take_animation_response()
}

fn draw_bell(
    painter: &egui::Painter,
    center: Pos2,
    s: f32,
    color: Color32,
    stroke_width: f32,
    filled: bool,
) {
    let bell_top = center.y - s * 0.4;
    let bell_bottom = center.y + s * 0.25;
    let dome_center = pos2(center.x, center.y - s * 0.1);
    let dome_radius = s * 0.3;
    let flare_half_w = s * 0.42;

    let n_arc = 12;
    let mut pts: Vec<Pos2> = Vec::with_capacity(n_arc + 4);

    for i in 0..=n_arc {
        let t = std::f32::consts::PI + (std::f32::consts::PI * i as f32 / n_arc as f32);
        pts.push(pos2(
            dome_center.x + dome_radius * t.cos(),
            dome_center.y + dome_radius * t.sin(),
        ));
    }
    pts.push(pos2(center.x + flare_half_w, bell_bottom));
    pts.push(pos2(center.x - flare_half_w, bell_bottom));

    if filled {
        painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else {
        let stroke = Stroke::new(stroke_width, color);
        let n = pts.len();
        for i in 0..n {
            painter.line_segment([pts[i], pts[(i + 1) % n]], stroke);
        }
    }

    // Clapper
    let clapper_center = pos2(center.x, bell_bottom + s * 0.12);
    let clapper_radius = s * 0.08;
    if filled {
        painter.circle_filled(clapper_center, clapper_radius, color);
    } else {
        painter.circle_stroke(
            clapper_center,
            clapper_radius,
            Stroke::new(stroke_width, color),
        );
    }

    // Nub on top
    painter.circle_filled(pos2(center.x, bell_top), s * 0.05, color);
}

/// Painter-drawn envelope icon for chat/messages (filled when active)
pub fn chat_button(ui: &mut egui::Ui, size: f32, is_active: bool) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
    let helper = AnimationHelper::new(ui, "chat-button", vec2(max_size, max_size));
    let rect = helper.get_animation_rect();
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let s = helper.scale_1d_pos(size);
    let color = toolbar_icon_color(ui, is_active);
    let stroke_width = helper.scale_1d_pos(1.5);

    draw_envelope(&painter, ui, center, s, color, stroke_width, is_active);

    helper.take_animation_response()
}

fn draw_envelope(
    painter: &egui::Painter,
    ui: &egui::Ui,
    center: Pos2,
    s: f32,
    color: Color32,
    stroke_width: f32,
    filled: bool,
) {
    let half_w = s * 0.5;
    let half_h = s * 0.35;
    let env_rect = egui::Rect::from_center_size(center, vec2(half_w * 2.0, half_h * 2.0));
    let rounding = s * 0.08;
    let flap_tip = pos2(center.x, center.y + s * 0.05);

    if filled {
        painter.rect_filled(env_rect, rounding, color);
        let bg = if ui.visuals().dark_mode {
            ui.visuals().window_fill
        } else {
            Color32::WHITE
        };
        let flap = vec![
            pos2(env_rect.left(), env_rect.top()),
            flap_tip,
            pos2(env_rect.right(), env_rect.top()),
        ];
        painter.add(egui::Shape::convex_polygon(flap, bg, Stroke::NONE));
    } else {
        let stroke = Stroke::new(stroke_width, color);
        painter.rect_stroke(env_rect, rounding, stroke, egui::StrokeKind::Inside);
        painter.line_segment([pos2(env_rect.left(), env_rect.top()), flap_tip], stroke);
        painter.line_segment([pos2(env_rect.right(), env_rect.top()), flap_tip], stroke);
    }
}

/// Painter-drawn home icon (house outline, filled when active)
pub fn home_button(ui: &mut egui::Ui, size: f32, is_active: bool) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
    let helper = AnimationHelper::new(ui, "home-button", vec2(max_size, max_size));
    let rect = helper.get_animation_rect();
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let s = helper.scale_1d_pos(size);
    let color = toolbar_icon_color(ui, is_active);
    let stroke_width = helper.scale_1d_pos(1.5);

    draw_house(&painter, ui, center, s, color, stroke_width, is_active);

    helper.take_animation_response()
}

fn draw_house(
    painter: &egui::Painter,
    ui: &egui::Ui,
    center: Pos2,
    s: f32,
    color: Color32,
    stroke_width: f32,
    filled: bool,
) {
    let roof_top = pos2(center.x, center.y - s * 0.45);
    let roof_left = pos2(center.x - s * 0.5, center.y - s * 0.02);
    let roof_right = pos2(center.x + s * 0.5, center.y - s * 0.02);

    let body_top = center.y - s * 0.02;
    let body_bottom = center.y + s * 0.4;
    let body_left = center.x - s * 0.38;
    let body_right = center.x + s * 0.38;

    let door_w = if filled { s * 0.2 } else { s * 0.15 };
    let door_h = if filled { s * 0.28 } else { s * 0.25 };

    if filled {
        let roof = vec![roof_top, roof_left, roof_right];
        painter.add(egui::Shape::convex_polygon(roof, color, Stroke::NONE));
        let body = vec![
            pos2(body_left, body_top),
            pos2(body_left, body_bottom),
            pos2(body_right, body_bottom),
            pos2(body_right, body_top),
        ];
        painter.add(egui::Shape::convex_polygon(body, color, Stroke::NONE));
        // Door cutout
        let bg = if ui.visuals().dark_mode {
            ui.visuals().window_fill
        } else {
            Color32::WHITE
        };
        let door = vec![
            pos2(center.x - door_w, body_bottom),
            pos2(center.x - door_w, body_bottom - door_h),
            pos2(center.x + door_w, body_bottom - door_h),
            pos2(center.x + door_w, body_bottom),
        ];
        painter.add(egui::Shape::convex_polygon(door, bg, Stroke::NONE));
    } else {
        let stroke = Stroke::new(stroke_width, color);
        // Roof
        painter.line_segment([roof_top, roof_left], stroke);
        painter.line_segment([roof_top, roof_right], stroke);
        // Roof base connecting to walls
        painter.line_segment([roof_left, pos2(body_left, body_top)], stroke);
        painter.line_segment([roof_right, pos2(body_right, body_top)], stroke);
        // Walls
        painter.line_segment(
            [pos2(body_left, body_top), pos2(body_left, body_bottom)],
            stroke,
        );
        painter.line_segment(
            [pos2(body_left, body_bottom), pos2(body_right, body_bottom)],
            stroke,
        );
        painter.line_segment(
            [pos2(body_right, body_bottom), pos2(body_right, body_top)],
            stroke,
        );
        // Door outline
        painter.line_segment(
            [
                pos2(center.x - door_w, body_bottom),
                pos2(center.x - door_w, body_bottom - door_h),
            ],
            stroke,
        );
        painter.line_segment(
            [
                pos2(center.x - door_w, body_bottom - door_h),
                pos2(center.x + door_w, body_bottom - door_h),
            ],
            stroke,
        );
        painter.line_segment(
            [
                pos2(center.x + door_w, body_bottom - door_h),
                pos2(center.x + door_w, body_bottom),
            ],
            stroke,
        );
    }
}

pub fn search_button(_color: Color32, line_width: f32, is_active: bool) -> impl Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
        let lw = if is_active {
            line_width + 0.5
        } else {
            line_width
        };
        let helper = AnimationHelper::new(ui, "search-button", vec2(max_size, max_size));
        let painter = ui.painter_at(helper.get_animation_rect());

        let cur_lw = helper.scale_1d_pos(lw);
        let min_outer_circle_radius = helper.scale_radius(15.0);
        let cur_outer_circle_radius = helper.scale_1d_pos(min_outer_circle_radius);
        let cur_handle_length = helper.scale_1d_pos(7.0);
        let circle_center = helper.scale_from_center(-2.0, -2.0);

        let handle_vec = vec2(
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        );
        let handle_pos_1 = circle_center + (handle_vec * (cur_outer_circle_radius - 3.0));
        let handle_pos_2 =
            circle_center + (handle_vec * (cur_outer_circle_radius + cur_handle_length));

        let icon_color = toolbar_icon_color(ui, is_active);
        let stroke = Stroke::new(cur_lw, icon_color);
        let fill = if is_active {
            icon_color
        } else {
            Color32::TRANSPARENT
        };

        painter.line_segment([handle_pos_1, handle_pos_2], stroke);
        painter.circle(circle_center, min_outer_circle_radius, fill, stroke);

        helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand)
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

/// Image-based expanding button used for side panel icons.
pub fn expanding_button(
    name: &'static str,
    img_size: f32,
    light_img: egui::Image,
    dark_img: egui::Image,
    ui: &mut egui::Ui,
    unseen_indicator: bool,
) -> egui::Response {
    let max_size = ICON_WIDTH * ICON_EXPANSION_MULTIPLE;
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
