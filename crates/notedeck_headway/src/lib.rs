use notedeck::{App, AppContext, AppResponse};

/// A Linear/Trello-style issue & todo tracker app for notedeck.
#[derive(Default)]
pub struct Headway {}

impl Headway {
    pub fn new() -> Self {
        Self::default()
    }
}

impl App for Headway {
    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        ui.centered_and_justified(|ui| {
            ui.label("Headway — coming soon");
        });
        AppResponse::default()
    }
}
