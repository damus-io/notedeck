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
    fn update(&mut self, ctx: &mut AppContext) {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.update(ctx),
            NotedeckApp::Columns(columns) => columns.update(ctx),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.update(ctx),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.update(ctx),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.update(ctx),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.update(ctx),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.update(ctx),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.update(ctx),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.update(ctx),

            NotedeckApp::Other(_name, other) => other.update(ctx),
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

    /// Fan a cross-app note open out to the app that owns the note's kind, so it
    /// can mint the route that open should land on (see
    /// [`notedeck::App::open_note_route`]). Apps with no cross-app deep link
    /// inherit the trait default (`None` — a plain app switch).
    fn open_note_route(
        &mut self,
        ctx: &mut AppContext,
        note_id: nostrdb_net::NoteId,
    ) -> Option<Rc<dyn Any>> {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.open_note_route(ctx, note_id),
            NotedeckApp::Columns(columns) => columns.open_note_route(ctx, note_id),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.open_note_route(ctx, note_id),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.open_note_route(ctx, note_id),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.open_note_route(ctx, note_id),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.open_note_route(ctx, note_id),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.open_note_route(ctx, note_id),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.open_note_route(ctx, note_id),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.open_note_route(ctx, note_id),

            NotedeckApp::Other(_name, other) => other.open_note_route(ctx, note_id),
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

    fn tools(&self) -> Vec<notedeck::RegisteredTool> {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => dave.tools(),
            NotedeckApp::Columns(columns) => columns.tools(),

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => notebook.tools(),

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => headway.tools(),

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => clndash.tools(),

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => dms.tools(),

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => db.tools(),

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => horizon.tools(),

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => nostrverse.tools(),

            NotedeckApp::Other(_name, other) => other.tools(),
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

    fn ui_tools(&self) -> Vec<notedeck::ToolSpec> {
        self.as_app().ui_tools()
    }

    fn call_ui_tool(
        &mut self,
        ctx: &mut AppContext<'_>,
        name: &str,
        args: &serde_json::Value,
    ) -> notedeck::ToolOutcome {
        self.as_app_mut().call_ui_tool(ctx, name, args)
    }

    fn ui_tool_available(&self, name: &str) -> bool {
        self.as_app().ui_tool_available(name)
    }

    fn search(
        &mut self,
        ctx: &mut AppContext<'_>,
        query: &notedeck::SearchQuery<'_>,
        out: &mut notedeck::SearchHits,
    ) {
        self.as_app_mut().search(ctx, query, out)
    }

    fn key_scopes(&self) -> &'static [&'static str] {
        self.as_app().key_scopes()
    }

    fn current_ref(&self) -> Option<&str> {
        self.as_app().current_ref()
    }

    fn jump_location(&self) -> Option<notedeck::JumpLocation> {
        self.as_app().jump_location()
    }

    fn restore_jump(&mut self, ctx: &mut AppContext<'_>, location: &dyn notedeck::JumpTarget) {
        self.as_app_mut().restore_jump(ctx, location)
    }
}

impl NotedeckApp {
    /// The wrapped app as a plain [`App`](notedeck::App), for methods that
    /// forward unchanged to whichever app this is.
    fn as_app(&self) -> &dyn notedeck::App {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => &**dave,
            NotedeckApp::Columns(columns) => &**columns,

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => &**notebook,

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => &**headway,

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => &**clndash,

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => &**dms,

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => &**db,

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => &**horizon,

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => &**nostrverse,

            NotedeckApp::Other(_name, other) => &**other,
        }
    }

    /// The wrapped app as a plain mutable [`App`](notedeck::App); see
    /// [`as_app`](Self::as_app).
    fn as_app_mut(&mut self) -> &mut dyn notedeck::App {
        match self {
            #[cfg(feature = "dave")]
            NotedeckApp::Dave(dave) => &mut **dave,
            NotedeckApp::Columns(columns) => &mut **columns,

            #[cfg(feature = "notebook")]
            NotedeckApp::Notebook(notebook) => &mut **notebook,

            #[cfg(feature = "headway")]
            NotedeckApp::Headway(headway) => &mut **headway,

            #[cfg(feature = "clndash")]
            NotedeckApp::ClnDash(clndash) => &mut **clndash,

            #[cfg(feature = "messages")]
            NotedeckApp::Messages(dms) => &mut **dms,

            #[cfg(feature = "dashboard")]
            NotedeckApp::Dashboard(db) => &mut **db,

            #[cfg(feature = "horizon")]
            NotedeckApp::Horizon(horizon) => &mut **horizon,

            #[cfg(feature = "nostrverse")]
            NotedeckApp::Nostrverse(nostrverse) => &mut **nostrverse,

            NotedeckApp::Other(_name, other) => &mut **other,
        }
    }
}
