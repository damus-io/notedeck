use egui::{Align, Label, Pos2, Rect, TextWrapMode};
use jsoncanvas::{FileNode, GroupNode, JsonCanvas, LinkNode, Node, TextNode, node::*};
use notedeck::{AppAction, AppContext};

pub struct Notebook {
    canvas: JsonCanvas,
    scene_rect: Rect,
    loaded: bool,
}

impl Notebook {
    pub fn new() -> Self {
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
            for (_node_id, node) in self.canvas.get_nodes().iter() {
                let _resp = node_ui(ui, node);
            }
        });

        None
    }
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
        ui.with_layout(egui::Layout::left_to_right(Align::Min), |ui| {
            ui.add(Label::new(node.text()).wrap_mode(TextWrapMode::Wrap))
        });
    })
}

fn file_node_ui(ui: &mut egui::Ui, node: &FileNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| {
        ui.label("file node");
    })
}

fn link_node_ui(ui: &mut egui::Ui, node: &LinkNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| {
        ui.label("link node");
    })
}

fn group_node_ui(ui: &mut egui::Ui, node: &GroupNode) -> egui::Response {
    node_box_ui(ui, node.node(), |ui| {
        ui.label("group node");
    })
}

fn node_box_ui(
    ui: &mut egui::Ui,
    node: &GenericNode,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let x = node.x as f32;
    let y = node.y as f32;
    let width = node.width as f32;
    let height = node.height as f32;

    let min = Pos2::new(x, y);
    let max = Pos2::new(x + width, y + height);

    ui.put(Rect::from_min_max(min, max), |ui: &mut egui::Ui| {
        egui::Frame::default()
            .fill(ui.visuals().noninteractive().weak_bg_fill)
            .inner_margin(egui::Margin::same(4))
            .corner_radius(egui::CornerRadius::same(10))
            .stroke(egui::Stroke::new(
                1.0,
                ui.visuals().noninteractive().bg_stroke.color,
            ))
            .show(ui, contents)
            .response
    })
}

fn demo_canvas() -> JsonCanvas {
    let demo_json: String = include_str!("../demo.canvas").to_string();

    let canvas: JsonCanvas = demo_json.parse().unwrap_or_else(|_| JsonCanvas::default());
    canvas
}
