use egui::{pos2, vec2, Color32, CursorIcon, Pos2, Stroke, Widget};

use crate::AnimationHelper;

pub static ICON_WIDTH: f32 = 40.0;
pub static ICON_EXPANSION_MULTIPLE: f32 = notedeck::tokens::ICON_EXPANSION_MULTIPLE;

/// Creates a magnifying glass icon widget
pub fn search_icon(size: f32, height: f32) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        // Use the provided height parameter
        let desired_size = vec2(size, height);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

        // Calculate center position - this ensures the icon is centered in its allocated space
        let center_pos = rect.center();
        let stroke = Stroke::new(
            notedeck::tokens::STROKE_MEDIUM,
            Color32::from_rgb(150, 150, 150),
        );

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

    let response = helper.take_animation_response();
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Notifications"));
    response
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

    let response = helper.take_animation_response();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Messages"));
    response
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

    let corner_inset = rounding + stroke_width;
    let flap_left = pos2(env_rect.left() + corner_inset, env_rect.top());
    let flap_right = pos2(env_rect.right() - corner_inset, env_rect.top());

    if filled {
        let bg = if ui.visuals().dark_mode {
            ui.visuals().window_fill
        } else {
            Color32::WHITE
        };
        let stroke = Stroke::new(stroke_width, bg);
        painter.rect_filled(env_rect, rounding, color);
        painter.line_segment([flap_left, flap_tip], stroke);
        painter.line_segment([flap_right, flap_tip], stroke);
    } else {
        let stroke = Stroke::new(stroke_width, color);
        painter.rect_stroke(env_rect, rounding, stroke, egui::StrokeKind::Inside);
        painter.line_segment([flap_left, flap_tip], stroke);
        painter.line_segment([flap_right, flap_tip], stroke);
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

    let response = helper.take_animation_response();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Home"));
    response
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
            line_width + 1.0
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
        let handle_pos_1 = circle_center + (handle_vec * cur_outer_circle_radius);
        let handle_pos_2 =
            circle_center + (handle_vec * (cur_outer_circle_radius + cur_handle_length));

        let icon_color = toolbar_icon_color(ui, is_active);
        let stroke = Stroke::new(cur_lw, icon_color);

        painter.line_segment([handle_pos_1, handle_pos_2], stroke);
        painter.circle(
            circle_center,
            min_outer_circle_radius,
            Color32::TRANSPARENT,
            stroke,
        );

        let response = helper
            .take_animation_response()
            .on_hover_cursor(CursorIcon::PointingHand);
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Search"));
        response
    }
}

/// Draw an upward arrow icon (used for updates)
pub fn draw_update_icon(
    painter: &egui::Painter,
    center: Pos2,
    s: f32,
    color: Color32,
    stroke_width: f32,
) {
    let stroke = Stroke::new(stroke_width, color);

    // Vertical shaft
    let shaft_top = pos2(center.x, center.y - s * 0.4);
    let shaft_bottom = pos2(center.x, center.y + s * 0.4);
    painter.line_segment([shaft_top, shaft_bottom], stroke);

    // Arrowhead
    let arrow_left = pos2(center.x - s * 0.3, center.y - s * 0.1);
    let arrow_right = pos2(center.x + s * 0.3, center.y - s * 0.1);
    painter.line_segment([shaft_top, arrow_left], stroke);
    painter.line_segment([shaft_top, arrow_right], stroke);
}

/// Function pointer for a vector icon painted into a square region.
type IconDraw = fn(&egui::Painter, Pos2, f32, Color32, f32);

/// Allocate a `size` square and paint a vector `draw` icon into it, centered
/// and left-aligned within its cell (matching the image-based app icons).
fn draw_app_icon(ui: &mut egui::Ui, size: f32, draw: IconDraw) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let color = ui.visuals().text_color();
    draw(&painter, rect.center(), size, color, size * 0.07);
}

/// Midpoint blend of two colors.
fn mix(a: Color32, b: Color32) -> Color32 {
    Color32::from_rgb(
        ((a.r() as u16 + b.r() as u16) / 2) as u8,
        ((a.g() as u16 + b.g() as u16) / 2) as u8,
        ((a.b() as u16 + b.b() as u16) / 2) as u8,
    )
}

/// Fill `rect` with a diagonal gradient from `c1` (top-left) to `c2`
/// (bottom-right).
fn gradient_rect(painter: &egui::Painter, rect: egui::Rect, c1: Color32, c2: Color32) {
    use egui::epaint::{Mesh, Vertex, WHITE_UV};
    let mid = mix(c1, c2);
    let mut mesh = Mesh::default();
    let v = |pos: Pos2, color: Color32| Vertex {
        pos,
        uv: WHITE_UV,
        color,
    };
    mesh.vertices.push(v(rect.left_top(), c1));
    mesh.vertices.push(v(rect.right_top(), mid));
    mesh.vertices.push(v(rect.right_bottom(), c2));
    mesh.vertices.push(v(rect.left_bottom(), mid));
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

/// Painter-drawn notebook icon: an amber cover with a darker spine and lines.
fn draw_notebook(
    painter: &egui::Painter,
    center: Pos2,
    s: f32,
    _color: Color32,
    stroke_width: f32,
) {
    let cover_hi = Color32::from_rgb(0xFB, 0xBF, 0x24); // amber-400
    let cover_lo = Color32::from_rgb(0xF5, 0x9E, 0x0B); // amber-500
    let ink = Color32::from_rgb(0x9A, 0x3C, 0x12); // warm brown

    let rect = egui::Rect::from_center_size(center, vec2(s * 0.66, s * 0.9));
    gradient_rect(painter, rect, cover_hi, cover_lo);

    let stroke = Stroke::new(stroke_width, ink);

    // Binding spine
    let spine_x = rect.left() + s * 0.18;
    painter.line_segment(
        [pos2(spine_x, rect.top()), pos2(spine_x, rect.bottom())],
        stroke,
    );

    // Ruled lines on the page
    let line_left = spine_x + s * 0.08;
    let line_right = rect.right() - s * 0.12;
    for i in -1..=1 {
        let y = center.y + i as f32 * s * 0.21;
        painter.line_segment([pos2(line_left, y), pos2(line_right, y)], stroke);
    }
}

/// Painter-drawn headway icon: emerald bars under a rising trend arrow.
fn draw_headway(painter: &egui::Painter, center: Pos2, s: f32, color: Color32, stroke_width: f32) {
    let bar_hi = Color32::from_rgb(0x6E, 0xE7, 0xB7); // emerald-300
    let bar_lo = Color32::from_rgb(0x05, 0x96, 0x69); // emerald-600

    // Ascending bars
    let baseline = center.y + s * 0.4;
    let bar_w = s * 0.2;
    let gap = s * 0.11;
    let total_w = bar_w * 3.0 + gap * 2.0;
    let start_x = center.x - total_w / 2.0;
    let heights = [s * 0.32, s * 0.52, s * 0.74];
    for (i, &bh) in heights.iter().enumerate() {
        let x0 = start_x + i as f32 * (bar_w + gap);
        let bar = egui::Rect::from_min_max(pos2(x0, baseline - bh), pos2(x0 + bar_w, baseline));
        gradient_rect(painter, bar, bar_hi, bar_lo);
    }

    // Rising trend arrow, drawn in the theme's text color so it stays legible
    let stroke = Stroke::new(stroke_width, color);
    let a_start = pos2(center.x - s * 0.45, center.y);
    let a_end = pos2(center.x + s * 0.45, center.y - s * 0.46);
    painter.line_segment([a_start, a_end], stroke);
    let head = s * 0.17;
    painter.line_segment([a_end, a_end + vec2(-head, 0.0)], stroke);
    painter.line_segment([a_end, a_end + vec2(0.0, head)], stroke);
}

/// Painter-drawn horizon icon: a rising sun over a horizon line — sunrise
/// orange disc with a soft highlight and two receding ground lines.
fn draw_horizon(painter: &egui::Painter, center: Pos2, s: f32, color: Color32, stroke_width: f32) {
    let sun_hi = Color32::from_rgb(0xFD, 0xBA, 0x74); // orange-300
    let sun_lo = Color32::from_rgb(0xF9, 0x73, 0x16); // orange-500

    let sun_center = pos2(center.x, center.y - s * 0.06);
    let r = s * 0.24;
    painter.circle_filled(sun_center, r, sun_lo);
    painter.circle_filled(sun_center - vec2(0.0, r * 0.32), r * 0.6, sun_hi);

    // Three short rays fanning up from the sun.
    let ray = Stroke::new(stroke_width, sun_lo);
    for &dx in &[-0.34_f32, 0.0, 0.34] {
        let dir = vec2(dx, -1.0).normalized();
        let from = sun_center + dir * (r * 1.25);
        let to = sun_center + dir * (r * 1.7);
        painter.line_segment([from, to], ray);
    }

    // Horizon lines, drawn in the theme color so they read on any background.
    let horizon_y = center.y + s * 0.26;
    painter.line_segment(
        [
            pos2(center.x - s * 0.42, horizon_y),
            pos2(center.x + s * 0.42, horizon_y),
        ],
        Stroke::new(stroke_width * 1.3, color),
    );
    painter.line_segment(
        [
            pos2(center.x - s * 0.26, horizon_y + s * 0.13),
            pos2(center.x + s * 0.26, horizon_y + s * 0.13),
        ],
        Stroke::new(stroke_width, color.gamma_multiply(0.5)),
    );
}

/// Painter-drawn dashboard icon: an asymmetric grid of gradient widget panels.
fn draw_dashboard(
    painter: &egui::Painter,
    center: Pos2,
    s: f32,
    _color: Color32,
    _stroke_width: f32,
) {
    let indigo_hi = Color32::from_rgb(0x81, 0x8C, 0xF8); // indigo-400
    let indigo_lo = Color32::from_rgb(0x4F, 0x46, 0xE5); // indigo-600
    let sky_hi = Color32::from_rgb(0x38, 0xBD, 0xF8); // sky-400
    let sky_lo = Color32::from_rgb(0x0E, 0xA5, 0xE9); // sky-500

    let half = s * 0.42;
    let left = center.x - half;
    let right = center.x + half;
    let top = center.y - half;
    let bottom = center.y + half;
    let gap = s * 0.09;

    // Tall panel on the left
    let split_x = left + (right - left) * 0.42;
    gradient_rect(
        painter,
        egui::Rect::from_min_max(pos2(left, top), pos2(split_x, bottom)),
        indigo_hi,
        indigo_lo,
    );

    // Two stacked panels on the right
    let rx = split_x + gap;
    gradient_rect(
        painter,
        egui::Rect::from_min_max(pos2(rx, top), pos2(right, center.y - gap / 2.0)),
        sky_hi,
        sky_lo,
    );
    gradient_rect(
        painter,
        egui::Rect::from_min_max(pos2(rx, center.y + gap / 2.0), pos2(right, bottom)),
        indigo_hi,
        indigo_lo,
    );
}

/// Painter-drawn messages icon: a violet speech bubble with text lines.
fn draw_messages(
    painter: &egui::Painter,
    center: Pos2,
    s: f32,
    _color: Color32,
    stroke_width: f32,
) {
    let bubble_hi = Color32::from_rgb(0xA7, 0x8B, 0xFA); // violet-400
    let bubble_lo = Color32::from_rgb(0x7C, 0x3A, 0xED); // violet-600

    // Bubble body, lifted slightly to make room for the tail.
    let body =
        egui::Rect::from_center_size(pos2(center.x, center.y - s * 0.07), vec2(s * 0.82, s * 0.6));
    gradient_rect(painter, body, bubble_hi, bubble_lo);

    // Tail pointing down-left from the bottom of the bubble.
    let tail = vec![
        pos2(body.left() + s * 0.16, body.bottom() - stroke_width),
        pos2(body.left() + s * 0.36, body.bottom() - stroke_width),
        pos2(body.left() + s * 0.12, body.bottom() + s * 0.22),
    ];
    painter.add(egui::Shape::convex_polygon(tail, bubble_lo, Stroke::NONE));

    // Text lines inside the bubble (last one shorter), drawn in a soft white.
    let ink = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xD0);
    let stroke = Stroke::new(stroke_width, ink);
    let line_left = body.left() + s * 0.15;
    let line_right = body.right() - s * 0.15;
    for i in -1..=1 {
        let y = body.center().y + i as f32 * s * 0.16;
        let right = if i == 1 {
            line_left + (line_right - line_left) * 0.55
        } else {
            line_right
        };
        painter.line_segment([pos2(line_left, y), pos2(right, y)], stroke);
    }
}

/// Fixed-size Dashboard app icon (sidebar drawer and chrome tab strip).
pub fn dashboard_icon(ui: &mut egui::Ui, size: f32) {
    draw_app_icon(ui, size, draw_dashboard);
}

/// Fixed-size Messages app icon (sidebar drawer and chrome tab strip).
pub fn messages_icon(ui: &mut egui::Ui, size: f32) {
    draw_app_icon(ui, size, draw_messages);
}

/// Fixed-size Notebook app icon (sidebar drawer and chrome tab strip).
pub fn notebook_icon(ui: &mut egui::Ui, size: f32) {
    draw_app_icon(ui, size, draw_notebook);
}

/// Fixed-size Headway app icon (sidebar drawer and chrome tab strip).
pub fn headway_icon(ui: &mut egui::Ui, size: f32) {
    draw_app_icon(ui, size, draw_headway);
}

/// Fixed-size Horizon app icon (sidebar drawer and chrome tab strip).
pub fn horizon_icon(ui: &mut egui::Ui, size: f32) {
    draw_app_icon(ui, size, draw_horizon);
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

    let response = helper.take_animation_response();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    response
}
