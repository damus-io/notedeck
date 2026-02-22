use notedeck::{AppContext, AppResponse};

use notedeck_columns::Damus;
use notedeck_dave::Dave;

#[cfg(feature = "clndash")]
use notedeck_clndash::ClnDash;

#[cfg(feature = "messages")]
use notedeck_messages::MessagesApp;

#[cfg(feature = "dashboard")]
use notedeck_dashboard::Dashboard;

#[cfg(feature = "notebook")]
use notedeck_notebook::Notebook;

#[cfg(feature = "nostrverse")]
use notedeck_nostrverse::NostrverseApp;

#[allow(clippy::large_enum_variant)]
pub enum NotedeckApp {
    Dave(Box<Dave>),
    Columns(Box<Damus>),

    #[cfg(feature = "notebook")]
    Notebook(Box<Notebook>),

    #[cfg(feature = "clndash")]
    ClnDash(Box<ClnDash>),

    #[cfg(feature = "messages")]
    Messages(Box<MessagesApp>),

    #[cfg(feature = "dashboard")]
    Dashboard(Box<Dashboard>),

    #[cfg(feature = "nostrverse")]
    Nostrverse(Box<NostrverseApp>),
    Other(String, Box<dyn notedeck::App>),
}

impl notedeck::App for NotedeckApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse {
        match self {
            NotedeckApp::Dave(dave) => dave.update(ctx, ui),
            NotedeckApp::Columns(columns) => columns.update(ctx, ui),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.update(ctx, ui),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.update(ctx, ui),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.update(ctx, ui),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.update(ctx, ui),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.update(ctx, ui),

            NotedeckApp::Other(_name, other) => other.update(ctx, ui),
        }
    }
}
