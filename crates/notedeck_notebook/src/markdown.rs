use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

/// Some state needed for our markdown rendering
pub struct Markdown {
    cache: CommonMarkCache,
}

impl Default for Markdown {
    fn default() -> Self {
        Markdown {
            cache: CommonMarkCache::default(),
        }
    }
}

impl Markdown {
    pub fn show(&mut self, ui: &mut egui::Ui, markdown: &str) -> egui::Response {
        CommonMarkViewer::new()
            .show(ui, &mut self.cache, markdown)
            .response
    }
}
