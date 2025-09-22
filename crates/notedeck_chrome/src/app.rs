use notedeck::{AppContext, AppResponse};
use notedeck_clndash::ClnDash;
use notedeck_columns::Damus;
use notedeck_dave::Dave;
use notedeck_notebook::Notebook;

#[allow(clippy::large_enum_variant)]
pub enum NotedeckApp {
    Dave(Box<Dave>),
    Columns(Box<Damus>),
    Notebook(Box<Notebook>),
    ClnDash(Box<ClnDash>),
    Other(Box<dyn notedeck::App>),
}

impl notedeck::App for NotedeckApp {
    fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse {
        match self {
            NotedeckApp::Dave(dave) => dave.update(ctx, ui),
            NotedeckApp::Columns(columns) => columns.update(ctx, ui),
            NotedeckApp::Notebook(notebook) => notebook.update(ctx, ui),
            NotedeckApp::ClnDash(clndash) => clndash.update(ctx, ui),
            NotedeckApp::Other(other) => other.update(ctx, ui),
        }
    }
}
