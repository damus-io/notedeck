use notedeck::{AppContext, AppResponse};

use notedeck_columns::Damus;

#[cfg(feature = "dave")]
use notedeck_dave::Dave;

#[cfg(feature = "clndash")]
use notedeck_clndash::ClnDash;

#[cfg(feature = "messages")]
use notedeck_messages::MessagesApp;

#[cfg(feature = "dashboard")]
use notedeck_dashboard::Dashboard;

#[cfg(feature = "notebook")]
use notedeck_notebook::Notebook;

#[cfg(feature = "headway")]
use notedeck_headway::Headway;

#[cfg(feature = "nostrverse")]
use notedeck_nostrverse::NostrverseApp;

#[allow(clippy::large_enum_variant)]
pub enum NotedeckApp {
    #[cfg(feature = "dave")]
    Dave(Box<Dave>),
    Columns(Box<Damus>),

    #[cfg(feature = "notebook")]
    Notebook(Box<Notebook>),

    #[cfg(feature = "headway")]
    Headway(Box<Headway>),

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
    fn update(&mut self, ctx: &mut AppContext, egui_ctx: &egui::Context) {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.update(ctx, egui_ctx),
            NotedeckApp::Columns(columns) => columns.update(ctx, egui_ctx),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.update(ctx, egui_ctx),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.update(ctx, egui_ctx),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.update(ctx, egui_ctx),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.update(ctx, egui_ctx),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.update(ctx, egui_ctx),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.update(ctx, egui_ctx),

            NotedeckApp::Other(_name, other) => other.update(ctx, egui_ctx),
        }
    }

    #[profiling::function]
    fn render(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.render(ctx, ui),
            NotedeckApp::Columns(columns) => columns.render(ctx, ui),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.render(ctx, ui),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.render(ctx, ui),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.render(ctx, ui),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.render(ctx, ui),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.render(ctx, ui),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.render(ctx, ui),

            NotedeckApp::Other(_name, other) => other.render(ctx, ui),
        }
    }

    fn tab_notifications(&self, ctx: &AppContext<'_>) -> notedeck::TabNotifications {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.tab_notifications(ctx),
            NotedeckApp::Columns(columns) => columns.tab_notifications(ctx),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.tab_notifications(ctx),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.tab_notifications(ctx),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.tab_notifications(ctx),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.tab_notifications(ctx),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.tab_notifications(ctx),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.tab_notifications(ctx),

            NotedeckApp::Other(_name, other) => other.tab_notifications(ctx),
        }
    }
}
