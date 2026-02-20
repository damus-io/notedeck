//! Room canvas rendering for nostrverse

use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

use super::room_state::{
    NostrverseAction, NostrverseState, ObjectShape, Room, RoomObject, RoomShape, RoomUser,
};

/// Response from rendering the nostrverse view
pub struct NostrverseResponse {
    pub response: Response,
    pub action: Option<NostrverseAction>,
}

/// Colors for rendering
mod colors {
    use egui::Color32;

    pub const ROOM_FILL: Color32 = Color32::from_rgb(30, 35, 45);
    pub const ROOM_BORDER: Color32 = Color32::from_rgb(80, 90, 110);
    pub const GRID_LINE: Color32 = Color32::from_rgb(45, 50, 60);
    pub const OBJECT_FILL: Color32 = Color32::from_rgb(70, 130, 180);
    pub const OBJECT_BORDER: Color32 = Color32::from_rgb(100, 160, 210);
    pub const OBJECT_SELECTED: Color32 = Color32::from_rgb(255, 200, 100);
    pub const OBJECT_DRAGGING: Color32 = Color32::from_rgb(100, 255, 150); // Green glow while dragging
    pub const PRESENCE: Color32 = Color32::from_rgb(100, 200, 100);
    pub const LABEL_TEXT: Color32 = Color32::from_rgb(200, 200, 210);

    // User avatar colors
    pub const USER_BORDER: Color32 = Color32::WHITE;
    pub const USER_SELF_BORDER: Color32 = Color32::from_rgb(255, 215, 0); // Gold for self
    pub const AGENT_BORDER: Color32 = Color32::from_rgb(0, 255, 200); // Cyan for AI agents
    pub const AGENT_FILL: Color32 = Color32::from_rgb(100, 200, 255); // Electric blue
}

/// Render the nostrverse room view
pub fn show_room_view(ui: &mut Ui, state: &mut NostrverseState) -> NostrverseResponse {
    let available_size = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(available_size, Sense::click_and_drag());

    let painter = ui.painter_at(rect);
    let canvas_center = rect.center();
    let mut action: Option<NostrverseAction> = None;

    // Handle drag start - check if we're starting a drag on an object
    if response.drag_started()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let world_pos = state.screen_to_world(pos.to_vec2(), canvas_center.to_vec2());
        if let Some(obj_id) = find_object_at(&state.objects, world_pos) {
            // Find the object's current position
            if let Some(obj) = state.objects.iter().find(|o| o.id == obj_id) {
                state.dragging_object = Some((obj_id.clone(), obj.position));
                state.selected_object = Some(obj_id);
            }
        }
    }

    // Handle drag - move object or pan camera
    if response.dragged() {
        let delta = response.drag_delta() / (state.zoom * 20.0);

        if let Some((obj_id, _)) = state.dragging_object.clone() {
            // Move the dragged object
            if let Some(obj) = state.get_object_mut(&obj_id) {
                obj.position += delta;
            }
        } else {
            // Pan the camera
            state.camera_offset -= delta;
        }
    }

    // Handle drag end - publish move event if object was dragged
    if response.drag_stopped()
        && let Some((obj_id, original_pos)) = state.dragging_object.take()
    {
        // Check if object actually moved
        if let Some(obj) = state.objects.iter().find(|o| o.id == obj_id)
            && (obj.position - original_pos).length() > 0.01
        {
            // Object was moved - trigger action to publish update
            action = Some(NostrverseAction::MoveObject {
                id: obj_id,
                position: obj.position,
            });
        }
    }

    // Handle zoom (scroll)
    if let Some(hover_pos) = response.hover_pos() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            // Zoom toward mouse position
            let before_zoom = state.screen_to_world(hover_pos.to_vec2(), canvas_center.to_vec2());
            state.zoom_by(scroll * 0.01);
            let after_zoom = state.screen_to_world(hover_pos.to_vec2(), canvas_center.to_vec2());
            state.camera_offset += before_zoom - after_zoom;
        }
    }

    // Draw room background
    painter.rect_filled(rect, 0.0, colors::ROOM_FILL);

    // Draw room boundary if loaded
    if let Some(room) = &state.room {
        draw_room_boundary(&painter, room, state, canvas_center.to_vec2());
        draw_grid(&painter, room, state, canvas_center.to_vec2(), rect);
    }

    // Draw objects
    for obj in &state.objects {
        let is_selected = state
            .selected_object
            .as_ref()
            .map(|id| id == &obj.id)
            .unwrap_or(false);
        let is_dragging = state
            .dragging_object
            .as_ref()
            .map(|(id, _)| id == &obj.id)
            .unwrap_or(false);
        draw_object(
            &painter,
            obj,
            state,
            canvas_center.to_vec2(),
            is_selected,
            is_dragging,
        );
    }

    // Draw presences (legacy)
    for presence in &state.presences {
        draw_presence(&painter, presence, state, canvas_center.to_vec2());
    }

    // Draw users (new style with avatars)
    for user in &state.users {
        draw_user(&painter, user, state, canvas_center.to_vec2());
    }

    // Handle click selection (only if not dragging)
    if response.clicked()
        && state.dragging_object.is_none()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let world_pos = state.screen_to_world(pos.to_vec2(), canvas_center.to_vec2());
        state.selected_object = find_object_at(&state.objects, world_pos);
    }

    // Draw info overlay
    draw_info_overlay(&painter, state, rect);

    NostrverseResponse { response, action }
}

fn draw_room_boundary(
    painter: &egui::Painter,
    room: &Room,
    state: &NostrverseState,
    canvas_center: Vec2,
) {
    let half_size = Vec2::new(room.width / 2.0, room.height / 2.0);
    let top_left = state.world_to_screen(-half_size, canvas_center);
    let bottom_right = state.world_to_screen(half_size, canvas_center);

    let room_rect = Rect::from_two_pos(
        Pos2::new(top_left.x, top_left.y),
        Pos2::new(bottom_right.x, bottom_right.y),
    );

    match room.shape {
        RoomShape::Rectangle => {
            painter.rect_stroke(
                room_rect,
                2.0,
                Stroke::new(2.0, colors::ROOM_BORDER),
                egui::StrokeKind::Middle,
            );
        }
        RoomShape::Circle => {
            let center = room_rect.center();
            let radius = room_rect.width().min(room_rect.height()) / 2.0;
            painter.circle_stroke(center, radius, Stroke::new(2.0, colors::ROOM_BORDER));
        }
        RoomShape::Custom => {
            painter.rect_stroke(
                room_rect,
                2.0,
                Stroke::new(2.0, colors::ROOM_BORDER),
                egui::StrokeKind::Middle,
            );
        }
    }
}

fn draw_grid(
    painter: &egui::Painter,
    room: &Room,
    state: &NostrverseState,
    canvas_center: Vec2,
    _clip_rect: Rect,
) {
    let grid_size = 1.0; // 1 unit grid
    let half_w = room.width / 2.0;
    let half_h = room.height / 2.0;

    // Vertical lines
    let mut x = -half_w;
    while x <= half_w {
        let top = state.world_to_screen(Vec2::new(x, -half_h), canvas_center);
        let bottom = state.world_to_screen(Vec2::new(x, half_h), canvas_center);
        painter.line_segment(
            [Pos2::new(top.x, top.y), Pos2::new(bottom.x, bottom.y)],
            Stroke::new(1.0, colors::GRID_LINE),
        );
        x += grid_size;
    }

    // Horizontal lines
    let mut y = -half_h;
    while y <= half_h {
        let left = state.world_to_screen(Vec2::new(-half_w, y), canvas_center);
        let right = state.world_to_screen(Vec2::new(half_w, y), canvas_center);
        painter.line_segment(
            [Pos2::new(left.x, left.y), Pos2::new(right.x, right.y)],
            Stroke::new(1.0, colors::GRID_LINE),
        );
        y += grid_size;
    }
}

fn draw_object(
    painter: &egui::Painter,
    obj: &RoomObject,
    state: &NostrverseState,
    canvas_center: Vec2,
    selected: bool,
    dragging: bool,
) {
    let pos = state.world_to_screen(obj.position, canvas_center);
    let size = obj.size * state.zoom * 20.0;
    let half_size = size / 2.0;

    let obj_rect = Rect::from_center_size(Pos2::new(pos.x, pos.y), size);

    // Visual feedback for different states
    let (border_color, border_width, fill_alpha) = if dragging {
        (colors::OBJECT_DRAGGING, 3.0, 180) // Semi-transparent while dragging
    } else if selected {
        (colors::OBJECT_SELECTED, 3.0, 255)
    } else {
        (colors::OBJECT_BORDER, 1.0, 255)
    };

    let fill_color = Color32::from_rgba_unmultiplied(
        colors::OBJECT_FILL.r(),
        colors::OBJECT_FILL.g(),
        colors::OBJECT_FILL.b(),
        fill_alpha,
    );

    match &obj.shape {
        ObjectShape::Rectangle => {
            painter.rect_filled(obj_rect, 2.0, fill_color);
            painter.rect_stroke(
                obj_rect,
                2.0,
                Stroke::new(border_width, border_color),
                egui::StrokeKind::Middle,
            );
        }
        ObjectShape::Circle => {
            let center = obj_rect.center();
            let radius = half_size.x.min(half_size.y);
            painter.circle_filled(center, radius, fill_color);
            painter.circle_stroke(center, radius, Stroke::new(border_width, border_color));
        }
        ObjectShape::Triangle => {
            let center = obj_rect.center();
            let points = [
                Pos2::new(center.x, center.y - half_size.y),
                Pos2::new(center.x - half_size.x, center.y + half_size.y),
                Pos2::new(center.x + half_size.x, center.y + half_size.y),
            ];
            painter.add(egui::Shape::convex_polygon(
                points.to_vec(),
                fill_color,
                Stroke::new(border_width, border_color),
            ));
        }
        ObjectShape::Icon(_) => {
            // Fall back to rectangle for now
            painter.rect_filled(obj_rect, 2.0, fill_color);
            painter.rect_stroke(
                obj_rect,
                2.0,
                Stroke::new(border_width, border_color),
                egui::StrokeKind::Middle,
            );
        }
    }

    // Draw label
    let label_pos = Pos2::new(pos.x, pos.y + half_size.y + 10.0);
    painter.text(
        label_pos,
        egui::Align2::CENTER_TOP,
        &obj.name,
        egui::FontId::proportional(12.0),
        colors::LABEL_TEXT,
    );
}

fn draw_presence(
    painter: &egui::Painter,
    presence: &super::room_state::Presence,
    state: &NostrverseState,
    canvas_center: Vec2,
) {
    let pos = state.world_to_screen(presence.position, canvas_center);
    let radius = 8.0 * state.zoom;

    painter.circle_filled(Pos2::new(pos.x, pos.y), radius, colors::PRESENCE);
    painter.circle_stroke(
        Pos2::new(pos.x, pos.y),
        radius,
        Stroke::new(2.0, Color32::WHITE),
    );

    // TODO: Draw name/npub abbreviation
}

fn draw_user(
    painter: &egui::Painter,
    user: &RoomUser,
    state: &NostrverseState,
    canvas_center: Vec2,
) {
    let pos = state.world_to_screen(user.position, canvas_center);
    let radius = 16.0 * state.zoom;
    let center = Pos2::new(pos.x, pos.y);

    let fill_color = if user.is_agent {
        colors::AGENT_FILL
    } else {
        user.derive_color()
    };

    let border_color = if user.is_self {
        colors::USER_SELF_BORDER
    } else if user.is_agent {
        colors::AGENT_BORDER
    } else {
        colors::USER_BORDER
    };

    let border_width = if user.is_self { 3.0 } else { 2.0 };

    if user.is_agent {
        // Draw hexagon for AI agents
        draw_hexagon(
            painter,
            center,
            radius,
            fill_color,
            border_color,
            border_width,
        );
        // Draw lightning bolt icon
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            "⚡",
            egui::FontId::proportional(radius * 0.8),
            Color32::WHITE,
        );
    } else {
        // Draw circle for humans
        painter.circle_filled(center, radius, fill_color);
        painter.circle_stroke(center, radius, Stroke::new(border_width, border_color));
        // Draw initial
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            user.initial().to_string(),
            egui::FontId::proportional(radius * 0.9),
            Color32::WHITE,
        );
    }

    // Draw display name below avatar
    let name_pos = Pos2::new(pos.x, pos.y + radius + 8.0);
    let display_text = if user.is_self {
        format!("{} (you)", user.display_name)
    } else if user.is_agent {
        format!("{} 🤖", user.display_name)
    } else {
        user.display_name.clone()
    };

    painter.text(
        name_pos,
        egui::Align2::CENTER_TOP,
        &display_text,
        egui::FontId::proportional(11.0),
        colors::LABEL_TEXT,
    );
}

/// Draw a hexagon shape (for AI agent avatars)
fn draw_hexagon(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    fill: Color32,
    stroke_color: Color32,
    stroke_width: f32,
) {
    let mut points = Vec::with_capacity(6);
    for i in 0..6 {
        let angle = std::f32::consts::PI / 3.0 * i as f32 - std::f32::consts::PI / 6.0;
        points.push(Pos2::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ));
    }
    painter.add(egui::Shape::convex_polygon(
        points,
        fill,
        Stroke::new(stroke_width, stroke_color),
    ));
}

fn find_object_at(objects: &[RoomObject], world_pos: Vec2) -> Option<String> {
    for obj in objects.iter().rev() {
        let half_size = obj.size / 2.0;
        let min = obj.position - half_size;
        let max = obj.position + half_size;

        if world_pos.x >= min.x
            && world_pos.x <= max.x
            && world_pos.y >= min.y
            && world_pos.y <= max.y
        {
            return Some(obj.id.clone());
        }
    }
    None
}

fn draw_info_overlay(painter: &egui::Painter, state: &NostrverseState, rect: Rect) {
    let room_name = state
        .room
        .as_ref()
        .map(|r| r.name.as_str())
        .unwrap_or("Loading...");

    let info_text = format!(
        "{} | Zoom: {:.0}% | Objects: {}",
        room_name,
        state.zoom * 100.0,
        state.objects.len()
    );

    painter.text(
        Pos2::new(rect.left() + 10.0, rect.top() + 10.0),
        egui::Align2::LEFT_TOP,
        info_text,
        egui::FontId::proportional(14.0),
        Color32::from_rgba_unmultiplied(200, 200, 210, 180),
    );
}

/// Render the object inspection panel (side panel when object is selected)
pub fn render_inspection_panel(
    ui: &mut Ui,
    state: &mut NostrverseState,
) -> Option<NostrverseAction> {
    let selected_id = state.selected_object.as_ref()?;
    let obj = state.objects.iter().find(|o| &o.id == selected_id)?;

    let mut action = None;

    egui::Frame::default()
        .fill(Color32::from_rgba_unmultiplied(30, 35, 45, 240))
        .inner_margin(12.0)
        .outer_margin(8.0)
        .corner_radius(8.0)
        .stroke(Stroke::new(1.0, colors::ROOM_BORDER))
        .show(ui, |ui| {
            ui.set_min_width(180.0);

            // Header with close button
            ui.horizontal(|ui| {
                ui.strong("🔍 Object Inspector");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").clicked() {
                        action = Some(NostrverseAction::SelectObject(None));
                    }
                });
            });

            ui.separator();

            // Object details
            ui.label(format!("📛 Name: {}", obj.name));

            let shape_str = match &obj.shape {
                ObjectShape::Rectangle => "Rectangle",
                ObjectShape::Circle => "Circle",
                ObjectShape::Triangle => "Triangle",
                ObjectShape::Icon(icon) => icon.as_str(),
            };
            ui.label(format!("🔷 Shape: {}", shape_str));

            ui.label(format!(
                "📍 Position: ({:.1}, {:.1})",
                obj.position.x, obj.position.y
            ));
            ui.label(format!("📐 Size: {:.1} × {:.1}", obj.size.x, obj.size.y));

            ui.separator();

            // ID (truncated for display)
            let id_display = if obj.id.len() > 16 {
                format!("{}...", &obj.id[..16])
            } else {
                obj.id.clone()
            };
            ui.small(format!("ID: {}", id_display));
        });

    action
}
