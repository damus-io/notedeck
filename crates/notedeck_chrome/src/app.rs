use notedeck::{AppContext, AppResponse};

use notedeck_columns::Damus;
use notedeck_dave::Dave;

#[cfg(feature = "calendar")]
use notedeck_calendar::CalendarApp;

#[cfg(feature = "clndash")]
use notedeck_clndash::ClnDash;

#[cfg(feature = "messages")]
use notedeck_messages::MessagesApp;

#[cfg(feature = "notebook")]
use notedeck_notebook::Notebook;

#[allow(clippy::large_enum_variant)]
pub enum NotedeckApp {
    Dave(Box<Dave>),
    Columns(Box<Damus>),
    #[cfg(feature = "calendar")]
    Calendar(Box<CalendarApp>),
    #[cfg(feature = "notebook")]
    Notebook(Box<Notebook>),
    #[cfg(feature = "clndash")]
    ClnDash(Box<ClnDash>),
    #[cfg(feature = "messages")]
    Messages(Box<MessagesApp>),
    Other(Box<dyn notedeck::App>),
}

impl notedeck::App for NotedeckApp {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse {
        match self {
            NotedeckApp::Dave(dave) => dave.update(ctx, ui),
            NotedeckApp::Columns(columns) => columns.update(ctx, ui),

            #[cfg(feature = "calendar")]
            NotedeckApp::Calendar(calendar) => calendar.update(ctx, ui),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.update(ctx, ui),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.update(ctx, ui),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.update(ctx, ui),

            NotedeckApp::Other(other) => other.update(ctx, ui),
        }
    }
}
