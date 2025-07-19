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
        let size = ui.available_size();
        CommonMarkViewer::new()
            .default_width(Some(size.x as usize))
            .show(ui, &mut self.cache, markdown)
            .response
    }
}
