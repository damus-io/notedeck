use egui::{Color32, Pos2, Rect, Shape, Stroke, epaint::CubicBezierShape, vec2};
use jsoncanvas::{
    FileNode, GroupNode, LinkNode, Node, NodeId, TextNode,
    color::{Color, PresetColor},
    edge::{Edge, Side},
    node::GenericNode,
};
use std::collections::HashMap;
use std::ops::Neg;

/// Resolve a JSONCanvas color (preset palette index or hex) to an egui color.
///
/// Preset values follow Obsidian's canvas palette ordering.
fn canvas_color(color: &Color) -> Color32 {
    match color {
        Color::Preset(preset) => match preset {
            PresetColor::Red => Color32::from_rgb(0xE0, 0x31, 0x31),
            PresetColor::Orange => Color32::from_rgb(0xE6, 0x77, 0x00),
            PresetColor::Yellow => Color32::from_rgb(0xE0, 0xAC, 0x00),
            PresetColor::Green => Color32::from_rgb(0x2C, 0xA0, 0x2C),
            PresetColor::Cyan => Color32::from_rgb(0x00, 0xA0, 0xBE),
            PresetColor::Purple => Color32::from_rgb(0x96, 0x50, 0xC8),
        },
        Color::Color(hex) => Color32::from_rgb(hex.r, hex.g, hex.b),
    }
}

/// Linear blend from `base` toward `accent` by `t` (0..=1).
fn blend(base: Color32, accent: Color32, t: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        lerp(base.r(), accent.r()),
        lerp(base.g(), accent.g()),
        lerp(base.b(), accent.b()),
    )
}

fn node_rect(node: &GenericNode) -> Rect {
    let x = node.x as f32;
    let y = node.y as f32;
    let width = node.width as f32;
    let height = node.height as f32;

    let min = Pos2::new(x, y);
    let max = Pos2::new(x + width, y + height);

    Rect::from_min_max(min, max)
}

fn side_point(side: &Side, node: &GenericNode) -> Pos2 {
    let rect = node_rect(node);

    match side {
        Side::Top => rect.center_top(),
        Side::Left => rect.left_center(),
        Side::Right => rect.right_center(),
        Side::Bottom => rect.center_bottom(),
    }
}

/// a unit vector pointing outward from the given side
fn side_tangent(side: &Side) -> egui::Vec2 {
    match side {
        Side::Top => vec2(0.0, -1.0),
        Side::Bottom => vec2(0.0, 1.0),
        Side::Left => vec2(-1.0, 0.0),
        Side::Right => vec2(1.0, 0.0),
    }
}

pub fn edge_ui(
    ui: &mut egui::Ui,
    nodes: &HashMap<NodeId, Node>,
    edge: &Edge,
) -> Option<egui::Response> {
    let from_node = nodes.get(edge.from_node())?;
    let to_node = nodes.get(edge.to_node())?;
    let to_side = edge.to_side()?;
    let from_side = edge.from_side()?;

    // anchor from-side
    let p0 = side_point(from_side, from_node.node());

    // anchor b
    let to_anchor = side_point(to_side, to_node.node());

    // to-point is slightly offset to accomidate arrow
    let p3 = to_anchor + side_tangent(to_side) * 2.0;

    // bend debug
    //let bend = debug_slider(ui, ui.id().with("bend"), p3, 0.25, 0.0..=1.0);
    let bend = 0.28;

    // How far to pull the tangents.
    // ¼ of the distance between anchors feels very “Obsidian”.
    let d = (p3 - p0).length() * bend;

    // c1 = anchor A + (outward tangent) * d
    let c1 = p0 + side_tangent(from_side) * d;

    // c2 = anchor B + (inward tangent)  * d
    let c2 = p3 - side_tangent(to_side).neg() * d;

    let color = edge
        .color()
        .map(canvas_color)
        .unwrap_or_else(|| ui.visuals().noninteractive().bg_stroke.color);
    let stroke = egui::Stroke::new(4.0, color);
    let bezier = CubicBezierShape::from_points_stroke([p0, c1, c2, p3], false, color, stroke);

    ui.painter().add(Shape::CubicBezier(bezier));
    arrow_ui(ui, to_side, to_anchor, color);

    None
}

/// Paint a tiny triangular “arrow”.
///
/// * `ui`    – the egui `Ui` you’re painting in
/// * `side`  – which edge of the box we’re attaching to
/// * `point` – the exact spot on that edge the arrow’s tip should touch
/// * `fill`  – colour to fill the arrow with (usually your popup’s background)
pub fn arrow_ui(ui: &mut egui::Ui, side: &Side, point: Pos2, fill: egui::Color32) {
    let len: f32 = 12.0; // distance from tip to base
    let width: f32 = 16.0; // length of the base
    let stroke: f32 = 1.0; // length of the base

    let verts = match side {
        Side::Top => [
            point,                                           // tip
            Pos2::new(point.x - width * 0.5, point.y - len), // base‑left (above)
            Pos2::new(point.x + width * 0.5, point.y - len), // base‑right (above)
        ],
        Side::Bottom => [
            point,
            Pos2::new(point.x + width * 0.5, point.y + len), // below
            Pos2::new(point.x - width * 0.5, point.y + len),
        ],
        Side::Left => [
            point,
            Pos2::new(point.x - len, point.y + width * 0.5), // left
            Pos2::new(point.x - len, point.y - width * 0.5),
        ],
        Side::Right => [
            point,
            Pos2::new(point.x + len, point.y - width * 0.5), // right
            Pos2::new(point.x + len, point.y + width * 0.5),
        ],
    };

    ui.painter().add(egui::Shape::convex_polygon(
        verts.to_vec(),
        fill,
        Stroke::new(stroke, fill), // add a stroke here if you want an outline
    ));
}

pub fn node_ui(ui: &mut egui::Ui, node: &Node) -> egui::Response {
    match node {
        Node::Text(text_node) => text_node_ui(ui, text_node),
        Node::File(file_node) => file_node_ui(ui, file_node),
        Node::Link(link_node) => link_node_ui(ui, link_node),
        Node::Group(group_node) => group_node_ui(ui, group_node),
    }
}

fn text_node_ui(ui: &mut egui::Ui, node: &TextNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| {
        egui::ScrollArea::vertical()
            .show(ui, |ui| {
                notedeck_ui::markdown::render_markdown(node.text(), ui);
                ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
            })
            .inner
    })
}

fn file_node_ui(ui: &mut egui::Ui, node: &FileNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| ui.label("file node"))
}

fn link_node_ui(ui: &mut egui::Ui, node: &LinkNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| ui.label("link node"))
}

fn group_node_ui(ui: &mut egui::Ui, node: &GroupNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| ui.label("group node"))
}

fn node_box_ui(
    ui: &mut egui::Ui,
    node: &GenericNode,
    contents: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    let pos = node_rect(node);

    // Colored nodes get an accent border and a faint accent-tinted fill; plain
    // nodes fall back to the neutral theme colors.
    let base_fill = ui.visuals().noninteractive().weak_bg_fill;
    let base_stroke = ui.visuals().noninteractive().bg_stroke.color;
    let (fill, stroke_color) = match node.color.as_ref().map(canvas_color) {
        Some(accent) => (blend(base_fill, accent, 0.12), accent),
        None => (base_fill, base_stroke),
    };

    ui.put(pos, |ui: &mut egui::Ui| {
        egui::Frame::default()
            .fill(fill)
            .inner_margin(egui::Margin::same(notedeck::tokens::SPACING_LG as i8))
            .corner_radius(egui::CornerRadius::same(notedeck::tokens::RADIUS_LG as u8))
            .stroke(egui::Stroke::new(
                notedeck::tokens::STROKE_THICK,
                stroke_color,
            ))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                ui.allocate_at_least(ui.available_size(), egui::Sense::click());
                ui.put(rect, contents);
            })
            .response
    })
}
