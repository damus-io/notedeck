use crate::ui::{node_rect, notebook_ui};
use egui::{Pos2, Rect};
use jsoncanvas::{JsonCanvas, NodeId};
use notedeck::{AppContext, AppResponse};
use std::collections::HashMap;

mod ui;

pub struct Notebook {
    canvas: JsonCanvas,
    scene_rect: Rect,
    loaded: bool,
    /// Per-node position overrides applied by dragging. The canvas remains the
    /// source of truth for the declared positions; this layers moves on top.
    positions: HashMap<NodeId, Pos2>,
    /// Currently selected node, if any.
    selected: Option<NodeId>,
}

impl Notebook {
    pub fn new() -> Self {
        Notebook::default()
    }

    /// Build a notebook displaying the given canvas.
    pub fn from_canvas(canvas: JsonCanvas) -> Self {
        Notebook {
            canvas,
            scene_rect: Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
            loaded: false,
            positions: HashMap::new(),
            selected: None,
        }
    }

    /// The node's current rect, accounting for any drag override.
    pub(crate) fn node_rect(&self, id: &NodeId, node: &jsoncanvas::Node) -> Rect {
        let default = node_rect(node.node());
        match self.positions.get(id) {
            Some(pos) => Rect::from_min_size(*pos, default.size()),
            None => default,
        }
    }

    /// The node's current top-left position (after any drag override).
    pub fn node_position(&self, id: &NodeId) -> Option<Pos2> {
        let node = self.canvas.get_nodes().get(id)?;
        Some(self.node_rect(id, node).min)
    }

    /// The currently selected node, if any.
    pub fn selected(&self) -> Option<&NodeId> {
        self.selected.as_ref()
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Notebook::from_canvas(demo_canvas())
    }
}

impl notedeck::App for Notebook {
    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        notebook_ui(self, ui);
        AppResponse::none()
    }
}

fn demo_canvas() -> JsonCanvas {
    let demo_json: String = include_str!("../demo.canvas").to_string();

    let canvas: JsonCanvas = demo_json.parse().unwrap_or_else(|_| JsonCanvas::default());
    canvas
}
