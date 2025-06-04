use notedeck::{AppAction, AppContext};
use notedeck_columns::Damus;
use notedeck_dave::Dave;

#[allow(clippy::large_enum_variant)]
pub enum NotedeckApp {
    Dave(Dave),
    Columns(Damus),
    Other(Box<dyn notedeck::App>),
}

impl notedeck::App for NotedeckApp {
    fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<AppAction> {
        match self {
            NotedeckApp::Dave(dave) => dave.update(ctx, ui),
            NotedeckApp::Columns(columns) => columns.update(ctx, ui),
            NotedeckApp::Other(other) => other.update(ctx, ui),
        }
    }
}
