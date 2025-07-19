use crate::{
    markdown::Markdown,
    ui::{edge_ui, node_ui},
};
use egui::{Pos2, Rect};
use jsoncanvas::JsonCanvas;
use notedeck::{AppAction, AppContext};

mod markdown;
mod ui;

pub struct Notebook {
    canvas: JsonCanvas,
    markdown: Markdown,
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
            markdown: Markdown::default(),
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
                let _resp = node_ui(ui, &mut self.markdown, node);
            }

            // render edges
            for (_edge_id, edge) in self.canvas.get_edges().iter() {
                let _resp = edge_ui(ui, self.canvas.get_nodes(), edge);
            }
        });

        None
    }
}

fn demo_canvas() -> JsonCanvas {
    let demo_json: String = include_str!("../demo.canvas").to_string();

    let canvas: JsonCanvas = demo_json.parse().unwrap_or_else(|_| JsonCanvas::default());
    canvas
}
