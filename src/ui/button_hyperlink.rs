use egui::{Button, Response, Ui, Widget};

pub struct ButtonHyperlink<'a> {
    url: String,
    button: Button<'a>,
    new_tab: bool,
}

impl<'a> ButtonHyperlink<'a> {
    pub fn new(button: Button<'a>, url: impl ToString) -> Self {
        let url = url.to_string();
        Self {
            url: url.clone(),
            button,
            new_tab: false,
        }
    }

    pub fn open_in_new_tab(mut self, new_tab: bool) -> Self {
        self.new_tab = new_tab;
        self
    }
}

impl<'a> Widget for ButtonHyperlink<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        let response = ui.add(self.button);

        if response.clicked() {
            let modifiers = ui.ctx().input(|i| i.modifiers);
            ui.ctx().open_url(egui::OpenUrl {
                url: self.url.clone(),
                new_tab: self.new_tab || modifiers.any(),
            });
        }
        if response.middle_clicked() {
            ui.ctx().open_url(egui::OpenUrl {
                url: self.url.clone(),
                new_tab: true,
            });
        }

        if ui.style().url_in_tooltip {
            response.on_hover_text(self.url)
        } else {
            response
        }
    }
}
