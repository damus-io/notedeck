use egui::{Rect, Response, RichText, Sense, Window};

#[derive(Default)]
pub struct FixedWindow {
    title: Option<RichText>,
}

#[derive(PartialEq)]
pub enum FixedWindowResponse {
    Opened,
    Closed,
}

impl FixedWindow {
    #[allow(dead_code)]
    pub fn new() -> Self {
        FixedWindow::default()
    }

    pub fn maybe_with_title(maybe_title: Option<RichText>) -> Self {
        Self { title: maybe_title }
    }

    #[allow(dead_code)]
    pub fn with_title(mut self, title: RichText) -> Self {
        self.title = Some(title);
        self
    }

    pub fn show(
        self,
        ui: &mut egui::Ui,
        rect: Rect,
        add_contents: impl FnOnce(&mut egui::Ui) -> Response,
    ) -> FixedWindowResponse {
        let mut is_open = true;

        let use_title_bar = self.title.is_some();
        let title = if let Some(title) = self.title {
            title
        } else {
            RichText::new("")
        };

        Window::new(title)
            .open(&mut is_open)
            .fixed_rect(rect)
            .collapsible(false)
            .movable(false)
            .resizable(false)
            .title_bar(use_title_bar)
            .show(ui.ctx(), |ui| {
                let resp = add_contents(ui);
                ui.allocate_rect(resp.rect, Sense::hover())
            });

        if !is_open {
            FixedWindowResponse::Closed
        } else {
            FixedWindowResponse::Opened
        }
    }
}
