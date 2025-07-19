use egui::{Align, Label, Pos2, Rect, Shape, Stroke, TextWrapMode, epaint::CubicBezierShape, vec2};
use jsoncanvas::{
    FileNode, GroupNode, JsonCanvas, LinkNode, Node, NodeId, TextNode,
    edge::{Edge, Side},
    node::GenericNode,
};
use notedeck::{AppAction, AppContext};
use std::collections::HashMap;
use std::ops::Neg;

pub struct Notebook {
    canvas: JsonCanvas,
    scene_rect: Rect,
    loaded: bool,
}

impl Notebook {
    pub fn new() -> Self {
        Notebook::default()
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Notebook {
            canvas: demo_canvas(),
            scene_rect: Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
            loaded: false,
        }
    }
}

impl notedeck::App for Notebook {
    fn update(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
        //let app_action: Option<AppAction> = None;

        if !self.loaded {
            self.scene_rect = ui.available_rect_before_wrap();
            self.loaded = true;
        }

        egui::Scene::new().show(ui, &mut self.scene_rect, |ui| {
            // render nodes
            for (_node_id, node) in self.canvas.get_nodes().iter() {
                let _resp = node_ui(ui, node);
            }

            // render edges
            for (_edge_id, edge) in self.canvas.get_edges().iter() {
                let _resp = edge_ui(ui, self.canvas.get_nodes(), edge);
            }
        });

        None
    }
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

fn edge_ui(
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

    let color = ui.visuals().noninteractive().bg_stroke.color;
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

fn node_ui(ui: &mut egui::Ui, node: &Node) -> egui::Response {
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
                ui.with_layout(egui::Layout::left_to_right(Align::Min), |ui| {
                    ui.add(Label::new(node.text()).wrap_mode(TextWrapMode::Wrap))
                })
            })
            .inner
            .response
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

    ui.put(pos, |ui: &mut egui::Ui| {
        egui::Frame::default()
            .fill(ui.visuals().noninteractive().weak_bg_fill)
            .inner_margin(egui::Margin::same(16))
            .corner_radius(egui::CornerRadius::same(10))
            .stroke(egui::Stroke::new(
                2.0,
                ui.visuals().noninteractive().bg_stroke.color,
            ))
            .show(ui, |ui| {
                let rect = ui.available_rect_before_wrap();
                ui.allocate_at_least(ui.available_size(), egui::Sense::click());
                ui.put(rect, |ui: &mut egui::Ui| contents(ui));
            })
            .response
    })
}

fn demo_canvas() -> JsonCanvas {
    let demo_json: String = include_str!("../demo.canvas").to_string();

    let canvas: JsonCanvas = demo_json.parse().unwrap_or_else(|_| JsonCanvas::default());
    canvas
}
