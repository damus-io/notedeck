use std::any::Any;
use std::rc::Rc;

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

#[cfg(feature = "horizon")]
use notedeck_horizon::Horizon;

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

    #[cfg(feature = "horizon")]
    Horizon(Box<Horizon>),

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

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.update(ctx, egui_ctx),

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

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.render(ctx, ui),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.render(ctx, ui),

            NotedeckApp::Other(_name, other) => other.render(ctx, ui),
        }
    }

    /// Fan a chrome global-nav entry out to the app that owns it, handing over
    /// the opaque route `token` so that app can downcast it and draw the specific
    /// view it names (see [`notedeck::App::render_nav`]). Non-navigating apps
    /// inherit the trait default, which discards the token and renders the whole
    /// app.
    #[profiling::function]
    fn render_nav(
        &mut self,
        ctx: &mut AppContext,
        ui: &mut egui::Ui,
        token: &Rc<dyn Any>,
    ) -> AppResponse {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.render_nav(ctx, ui, token),
            NotedeckApp::Columns(columns) => columns.render_nav(ctx, ui, token),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.render_nav(ctx, ui, token),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.render_nav(ctx, ui, token),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.render_nav(ctx, ui, token),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.render_nav(ctx, ui, token),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.render_nav(ctx, ui, token),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.render_nav(ctx, ui, token),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.render_nav(ctx, ui, token),

            NotedeckApp::Other(_name, other) => other.render_nav(ctx, ui, token),
        }
    }

    /// Fan a global-nav entry's title request out to the app that owns it, so the
    /// chrome history dropdown can label the entry with the specific view its
    /// `token` names (see [`notedeck::App::nav_title`]). Non-navigating apps
    /// inherit the trait default (`None`) and the chrome falls back to their label.
    fn nav_title(&self, token: &Rc<dyn Any>) -> Option<String> {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.nav_title(token),
            NotedeckApp::Columns(columns) => columns.nav_title(token),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.nav_title(token),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.nav_title(token),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.nav_title(token),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.nav_title(token),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.nav_title(token),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.nav_title(token),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.nav_title(token),

            NotedeckApp::Other(_name, other) => other.nav_title(token),
        }
    }

    /// Fan a completed global-nav pop out to the app that owned the popped entry,
    /// handing back the same route `token` so it can free that route's resources
    /// (see [`notedeck::App::cleanup_nav`]). Non-navigating apps inherit the
    /// trait default (a no-op).
    fn cleanup_nav(&mut self, ctx: &mut AppContext, token: &Rc<dyn Any>) {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.cleanup_nav(ctx, token),
            NotedeckApp::Columns(columns) => columns.cleanup_nav(ctx, token),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.cleanup_nav(ctx, token),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.cleanup_nav(ctx, token),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.cleanup_nav(ctx, token),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.cleanup_nav(ctx, token),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.cleanup_nav(ctx, token),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.cleanup_nav(ctx, token),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.cleanup_nav(ctx, token),

            NotedeckApp::Other(_name, other) => other.cleanup_nav(ctx, token),
        }
    }

    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.kind_renderers(),
            NotedeckApp::Columns(columns) => columns.kind_renderers(),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.kind_renderers(),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.kind_renderers(),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.kind_renderers(),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.kind_renderers(),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.kind_renderers(),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.kind_renderers(),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.kind_renderers(),

            NotedeckApp::Other(_name, other) => other.kind_renderers(),
        }
    }

    fn reference_parsers(&self) -> Vec<Box<dyn notedeck::ReferenceParser>> {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.reference_parsers(),
            NotedeckApp::Columns(columns) => columns.reference_parsers(),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.reference_parsers(),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.reference_parsers(),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.reference_parsers(),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.reference_parsers(),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.reference_parsers(),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.reference_parsers(),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.reference_parsers(),

            NotedeckApp::Other(_name, other) => other.reference_parsers(),
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

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.tab_notifications(ctx),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.tab_notifications(ctx),

            NotedeckApp::Other(_name, other) => other.tab_notifications(ctx),
        }
    }
}
