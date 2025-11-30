use notedeck::{App, AppContext, AppResponse};

#[derive(Default)]
pub struct MessagesApp {}

impl MessagesApp {
    pub fn new() -> Self {
        Self {}
    }

    fn ui(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) {}
}

impl App for MessagesApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.ui(ctx, ui);
        AppResponse::none()
    }
}
