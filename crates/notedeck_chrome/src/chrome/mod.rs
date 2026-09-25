// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use crate::ChromeOptions;
use eframe::CreationContext;
use egui::{Color32, CornerRadius, Layout, Margin, Rect, ThemePreference, Ui};
use egui_extras::{Size, StripBuilder};
use egui_nav::RouteResponse;
use egui_nav::{NavAction, NavDrawer, NavUiType};
use nostrdb::Transaction;
use notedeck::ui::is_compiled_as_mobile;
use notedeck::AppResponse;
use notedeck::DrawerRouter;
use notedeck::Error;
use notedeck::{
    nav_frame, App, AppAction, AppContext, AppId, ChromeNavEntry, NavRequest, NavStack,
    NavStackEvent, Notedeck, NotedeckOptions, WalletType,
};
use notedeck_columns::{timeline::TimelineKind, Damus};
use oot_bitset::{bitset_clear, bitset_get, bitset_set};

#[cfg(feature = "dave")]
use notedeck_dave::Dave;

#[cfg(feature = "messages")]
use notedeck_messages::MessagesApp;

#[cfg(feature = "dashboard")]
use notedeck_dashboard::Dashboard;

#[cfg(feature = "horizon")]
use notedeck_horizon::Horizon;

#[cfg(feature = "clndash")]
use notedeck_ui::expanding_button;

use std::collections::HashMap;
use std::rc::Rc;

mod debug;
mod header;
mod keyboard;
mod sidebar;
#[cfg(feature = "auto-update")]
mod update;

use header::chrome_app_tabs;
use keyboard::{keyboard_visibility, virtual_keyboard_ui, AnimState};
use sidebar::{milestone_name, topdown_sidebar, SidebarOptions};
#[cfg(feature = "auto-update")]
use update::poll_updater;

/// Upper bound on the number of apps the running-app bitset can track. One bit
/// per app index; see [`Chrome::tools_snapshot`].
const MAX_APPS: usize = 256;

pub struct Chrome {
    active: i32,
    options: ChromeOptions,
    apps: Vec<NotedeckApp>,

    /// Which apps have been opened (activated), one bit per app index. Only
    /// opened apps receive `update()` calls each frame.
    opened: [u16; MAX_APPS / 16],

    /// The last-focused egui widget id for each app (keyed by app index).
    /// egui drops keyboard focus when a widget isn't rendered for a frame, so
    /// switching apps (Ctrl+Tab / tab click) would otherwise lose focus until
    /// the user clicks back in. We remember each app's focus and re-request it
    /// on switch-back. Generic — no per-app code needed.
    app_focus: HashMap<usize, egui::Id>,

    /// The active app index from the previous frame, used to detect switches.
    prev_active: i32,

    /// Running-app mask (`all_active || opened[i]`, one bit per app index) the
    /// agent-tool registry was last built from. When it changes we re-aggregate
    /// the running apps' tools; see the `App::take_tool_update` impl.
    tools_snapshot: [u16; MAX_APPS / 16],

    /// The state of the soft keyboard animation
    soft_kb_anim_state: AnimState,

    pub repaint_causes: HashMap<egui::RepaintCause, u64>,
    nav: DrawerRouter,

    /// The single, browser-style global navigation history spanning every app,
    /// where back/forward can cross app boundaries. The chrome is its sole
    /// mutator: apps request navigation via
    /// [`AppContext::navigator`](notedeck::AppContext) and the chrome drains and
    /// applies those requests each frame (see [`Chrome::apply_nav_requests`]).
    ///
    /// The stack top names the *active* app, so `active` is derived from it (see
    /// [`Chrome::sync_active_from_nav`]) and rendered through
    /// [`nav_frame`](notedeck::nav_frame) so app→app switches animate. Seeded
    /// with the initial app's route at construction, so it is never `None` in
    /// practice — the `Option` only lets the field be built before the first
    /// route is known.
    global_nav: Option<NavStack<ChromeNavEntry>>,

    #[cfg(feature = "auto-update")]
    updater: notedeck::updater::Updater,
}

#[derive(Clone)]
enum ChromeRoute {
    Chrome,
    App,
}

pub enum ChromePanelAction {
    Support,
    Settings,
    Account,
    Wallet,
    SaveTheme(ThemePreference),
    Profile(nostrdb_net::Pubkey),
    #[cfg(feature = "auto-update")]
    ApplyUpdate,
    #[cfg(feature = "auto-update")]
    DismissUpdate,
}

impl ChromePanelAction {
    fn columns_navigate(ctx: &mut AppContext, chrome: &mut Chrome, route: notedeck_columns::Route) {
        chrome.switch_to_columns();

        if let Some(c) = chrome.get_columns_app().and_then(|columns| {
            columns
                .decks_cache
                .selected_column_mut(ctx.i18n, ctx.accounts)
        }) {
            if c.router().routes().iter().any(|r| r == &route) {
                // return if we are already routing to accounts
                c.router_mut().go_back();
            } else {
                c.router_mut().route_to(route);
                //c..route_to(Route::relays());
            }
        };
    }

    #[profiling::function]
    fn process(&self, ctx: &mut AppContext, chrome: &mut Chrome, ui: &mut egui::Ui) {
        match self {
            Self::SaveTheme(theme) => {
                ui.ctx().set_theme(*theme);
                ctx.settings.set_theme(*theme);
            }

            Self::Support => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Support);
            }

            Self::Account => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::accounts());
            }

            Self::Settings => {
                Self::columns_navigate(ctx, chrome, notedeck_columns::Route::Settings);
            }

            Self::Wallet => {
                Self::columns_navigate(
                    ctx,
                    chrome,
                    notedeck_columns::Route::Wallet(WalletType::Auto),
                );
            }
            Self::Profile(pk) => {
                columns_route_to_profile(pk, chrome, ctx, ui);
            }

            #[cfg(feature = "auto-update")]
            Self::ApplyUpdate => {
                if let Err(e) = chrome.updater.apply_and_restart() {
                    tracing::error!("failed to apply update: {e}");
                }
            }

            #[cfg(feature = "auto-update")]
            Self::DismissUpdate => {
                chrome.updater.dismiss();
            }
        }
    }
}

/// Some people have been running notedeck in debug, let's catch that!
fn stop_debug_mode(options: NotedeckOptions) {
    if !options.contains(NotedeckOptions::Tests)
        && cfg!(debug_assertions)
        && !options.contains(NotedeckOptions::Debug)
    {
        println!("--- WELCOME TO DAMUS NOTEDECK! ---");
        println!(
            "It looks like are running notedeck in debug mode, unless you are a developer, this is not likely what you want."
        );
        println!("If you are a developer, run `cargo run -- --debug` to skip this message.");
        println!("For everyone else, try again with `cargo run --release`. Enjoy!");
        println!("---------------------------------");
        panic!();
    }
}

/// Register every app's startup-scoped registry contributions with the host: the
/// inline [`KindRenderer`](notedeck::KindRenderer)s (back half) and the
/// [`ReferenceParser`](notedeck::ReferenceParser)s (front half) of inline
/// references. Both are registered up front for *all* apps so a reference
/// resolves and renders even for apps the user never opened.
fn setup_app_registries(notedeck: &mut Notedeck, apps: &[NotedeckApp]) {
    for renderer in apps.iter().flat_map(|app| app.kind_renderers()) {
        notedeck.register_kind_renderer(renderer);
    }
    for parser in apps.iter().flat_map(|app| app.reference_parsers()) {
        notedeck.register_reference_parser(parser);
    }
}

/// Seed the chrome-global navigation history with the initial app's route
/// (slot 0). [`NavStack::new`] panics on an empty stack, so the chrome is born
/// with exactly this one entry. The route token is a placeholder `Rc::new(())`:
/// no app reads its own token yet (that lands with Columns' `render_nav`
/// override in a later subissue) and the chrome never inspects it.
fn seed_global_nav() -> NavStack<ChromeNavEntry> {
    NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))])
}

impl Chrome {
    /// Create a new chrome with the default app setup, driven by an eframe
    /// [`CreationContext`] (windowed/GUI runtime).
    pub fn new_with_apps(
        cc: &CreationContext,
        app_args: &[String],
        notedeck: &mut Notedeck,
    ) -> Result<Self, Error> {
        Self::new_with_render_state(cc.wgpu_render_state.as_ref(), app_args, notedeck)
    }

    /// Create a chrome for headless runtime mode — no eframe
    /// [`CreationContext`] and no wgpu render state.
    ///
    /// Builds the exact same app roster as
    /// [`new_with_apps`](Self::new_with_apps) (respecting `--no-columns-app` and
    /// the same cargo-feature app gating, so a headless build runs exactly the
    /// apps compiled in), just with a `None` render state so the GPU-backed
    /// avatars/renderers fall back to their non-GPU paths (e.g. Dave's
    /// `dave_button`).
    ///
    /// Needs no `egui::Context`: the one thing construction used one for was to
    /// hand Dave's IPC listener something to wake, and that takes the host's
    /// [`Waker`](notedeck::Waker) now.
    pub fn new_headless(app_args: &[String], notedeck: &mut Notedeck) -> Result<Self, Error> {
        Self::new_with_render_state(None, app_args, notedeck)
    }

    /// Shared constructor backing [`new_with_apps`](Self::new_with_apps) and
    /// [`new_headless`](Self::new_headless). `render_state` is `None` in the
    /// headless case, which is exactly what the GPU-backed apps already tolerate.
    fn new_with_render_state(
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        app_args: &[String],
        notedeck: &mut Notedeck,
    ) -> Result<Self, Error> {
        // `render_state` is only consumed by the GPU-backed apps (Dave,
        // Nostrverse); silence the unused warning when neither is compiled in.
        #[cfg(not(any(feature = "dave", feature = "nostrverse")))]
        {
            let _ = render_state;
        }
        let notedeck_options = notedeck.options();
        stop_debug_mode(notedeck_options);

        // Named (not a borrowed temporary) so we can drop it below to release
        // the `&mut notedeck` borrow and register kind-renderers afterwards.
        let mut notedeck_ref = notedeck.notedeck_ref();
        let app_ref = &mut notedeck_ref;
        #[cfg(feature = "dave")]
        let dave = Dave::new(
            render_state,
            app_ref.app_ctx.ndb.clone(),
            app_ref.app_ctx.waker.clone(),
            app_ref.app_ctx.path,
        );
        #[cfg(feature = "wasm")]
        let wasm_dir = app_ref
            .app_ctx
            .path
            .path(notedeck::DataPathType::Cache)
            .join("wasm_apps");

        let mut chrome = Chrome {
            active: 0,
            options: ChromeOptions::default(),
            apps: Vec::new(),
            opened: [0u16; MAX_APPS / 16],
            app_focus: HashMap::new(),
            prev_active: 0,
            tools_snapshot: [0u16; MAX_APPS / 16],
            soft_kb_anim_state: AnimState::default(),
            repaint_causes: HashMap::new(),
            nav: DrawerRouter::default(),
            global_nav: Some(seed_global_nav()),
            #[cfg(feature = "auto-update")]
            updater: notedeck::updater::Updater::new(
                app_ref.app_ctx.path,
                &app_ref.app_ctx.ndb,
                app_ref.app_ctx.waker.clone(),
                notedeck::updater::nostr::DEFAULT_RELEASE_PUBKEY,
                notedeck::updater::nostr::ReleaseChannel::from_setting(
                    app_ref.app_ctx.settings.release_channel(),
                ),
            ),
        };

        if !app_args.iter().any(|arg| arg == "--no-columns-app") {
            let columns = Damus::new(&mut app_ref.app_ctx, app_args);
            app_ref.internals.check_args(columns.unrecognized_args())?;
            chrome.add_app(NotedeckApp::Columns(Box::new(columns)));
        }

        #[cfg(feature = "dave")]
        chrome.add_app(NotedeckApp::Dave(Box::new(dave)));

        #[cfg(feature = "messages")]
        chrome.add_app(NotedeckApp::Messages(Box::new(MessagesApp::new())));

        #[cfg(feature = "dashboard")]
        chrome.add_app(NotedeckApp::Dashboard(Box::new(Dashboard::default())));

        #[cfg(feature = "horizon")]
        chrome.add_app(NotedeckApp::Horizon(Box::new(Horizon::default())));

        #[cfg(feature = "notebook")]
        chrome.add_app(NotedeckApp::Notebook(Box::default()));

        #[cfg(feature = "headway")]
        chrome.add_app(NotedeckApp::Headway(Box::default()));

        #[cfg(feature = "clndash")]
        chrome.add_app(NotedeckApp::ClnDash(Box::default()));

        #[cfg(feature = "nostrverse")]
        chrome.add_app(NotedeckApp::Nostrverse(Box::new(
            notedeck_nostrverse::NostrverseApp::demo(render_state),
        )));

        #[cfg(feature = "wasm")]
        {
            tracing::info!("looking for WASM apps in: {}", wasm_dir.display());
            if wasm_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&wasm_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|e| e == "wasm") {
                            match notedeck_wasm::WasmApp::from_file(&path) {
                                Ok(app) => {
                                    let name = app.name().to_string();
                                    tracing::info!(
                                        "loaded WASM app '{}': {}",
                                        name,
                                        path.display()
                                    );
                                    chrome.add_app(NotedeckApp::Other(name, Box::new(app)));
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "failed to load WASM app {}: {e}",
                                        path.display()
                                    );
                                }
                            }
                        }
                    }
                }
            } else {
                tracing::info!("WASM apps directory not found: {}", wasm_dir.display());
            }
        }

        if notedeck_options.contains(NotedeckOptions::AllAppsActive) {
            chrome.options.set(ChromeOptions::AllAppsActive, true);
        }

        chrome.set_active(0);

        app_ref.app_ctx.sound.play(notedeck::SoundEffect::Startup);

        // Release the `&mut notedeck` borrow so we can register registries below.
        drop(notedeck_ref);

        setup_app_registries(notedeck, &chrome.apps);

        Ok(chrome)
    }

    /// Create a Chrome for snapshot tests — no eframe CreationContext needed.
    #[cfg(feature = "auto-update")]
    pub fn new_test(ctx: &mut notedeck::AppContext, args: &[String]) -> Self {
        let damus = Damus::new(ctx, args);
        let mut chrome = Chrome {
            active: 0,
            options: ChromeOptions::default(),
            apps: Vec::new(),
            opened: [0u16; MAX_APPS / 16],
            app_focus: HashMap::new(),
            prev_active: 0,
            tools_snapshot: [0u16; MAX_APPS / 16],
            soft_kb_anim_state: AnimState::default(),
            repaint_causes: HashMap::new(),
            nav: DrawerRouter::default(),
            global_nav: Some(seed_global_nav()),
            updater: notedeck::updater::Updater::new(
                ctx.path,
                &ctx.ndb,
                ctx.waker.clone(),
                notedeck::updater::nostr::DEFAULT_RELEASE_PUBKEY,
                notedeck::updater::nostr::ReleaseChannel::from_setting(
                    ctx.settings.release_channel(),
                ),
            ),
        };
        chrome.add_app(NotedeckApp::Columns(Box::new(damus)));
        chrome.set_active(0);
        chrome
    }

    /// Override the release signing pubkey and resubscribe to ndb.
    #[cfg(all(feature = "auto-update", feature = "snapshot-testing"))]
    pub fn set_release_pubkey(&mut self, ndb: &mut nostrdb::Ndb, pubkey: [u8; 32]) {
        self.updater.set_release_pubkey(ndb, pubkey);
    }

    /// Force the updater into ReadyToInstall state (for snapshot tests).
    #[cfg(all(feature = "auto-update", feature = "snapshot-testing"))]
    pub fn force_update_ready(&mut self, version: String) {
        self.updater.force_ready(version);
    }

    pub fn toggle(&mut self) {
        if self.nav.drawer_focused {
            self.nav.close();
        } else {
            self.nav.open();
        }
    }

    /// Chrome-level keybindings — consumed before apps render,
    /// so apps can never intercept them.
    fn handle_chrome_keybindings(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11)) {
            self.toggle();
        }

        // Ctrl+W (Cmd+W on macOS) closes the active app's tab.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            self.close_active_app();
        }
    }

    /// Fallback keybindings — only fire if no app consumed the key.
    fn handle_fallback_keybindings(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.toggle();
        }
    }

    pub fn add_app(&mut self, app: NotedeckApp) {
        self.apps.push(app);
        // `opened` is a fixed bitset — the new app's bit is already clear.
    }

    /// Whether the app at index `i` has been opened.
    fn is_opened(&self, i: usize) -> bool {
        i < MAX_APPS && bitset_get(&self.opened, i as u16)
    }

    /// Mark the app at index `i` as opened.
    fn set_opened(&mut self, i: usize) {
        if i < MAX_APPS {
            bitset_set(&mut self.opened, i as u16);
        }
    }

    /// Mark the app at index `i` as closed.
    fn clear_opened(&mut self, i: usize) {
        if i < MAX_APPS {
            bitset_clear(&mut self.opened, i as u16);
        }
    }

    /// The number of currently-opened apps.
    fn opened_count(&self) -> usize {
        (0..self.apps.len()).filter(|&i| self.is_opened(i)).count()
    }

    fn get_columns_app(&mut self) -> Option<&mut Damus> {
        for app in &mut self.apps {
            if let NotedeckApp::Columns(cols) = app {
                return Some(cols);
            }
        }

        None
    }

    fn switch_to_columns(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Columns(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    #[cfg(feature = "dave")]
    fn get_dave_app(&mut self) -> Option<&mut Dave> {
        for app in &mut self.apps {
            if let NotedeckApp::Dave(dave) = app {
                return Some(dave);
            }
        }
        None
    }

    #[cfg(feature = "dave")]
    fn switch_to_dave(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Dave(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    #[cfg(feature = "messages")]
    fn switch_to_messages(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Messages(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    /// The Headway app's slot — its [`AppId`], which Headway itself never
    /// learns — or `None` when it isn't in the roster.
    #[cfg(feature = "headway")]
    fn headway_slot(&self) -> Option<usize> {
        self.apps
            .iter()
            .position(|app| matches!(app, NotedeckApp::Headway(_)))
    }

    #[cfg(feature = "notebook")]
    fn get_notebook_app(&mut self) -> Option<&mut notedeck_notebook::Notebook> {
        for app in &mut self.apps {
            if let NotedeckApp::Notebook(notebook) = app {
                return Some(notebook);
            }
        }
        None
    }

    #[cfg(feature = "notebook")]
    fn switch_to_notebook(&mut self) {
        for i in 0..self.apps.len() {
            if let NotedeckApp::Notebook(_) = self.apps[i] {
                self.set_active(i as i32);
            }
        }
    }

    fn process_toolbar_action(&mut self, action: ChromeToolbarAction, ctx: &mut AppContext) {
        match action {
            ChromeToolbarAction::Home => {
                self.switch_to_columns();
                if let Some(columns) = self.get_columns_app() {
                    columns.navigate_home(ctx);
                }
            }
            #[cfg(feature = "messages")]
            ChromeToolbarAction::Chat => {
                self.switch_to_messages();
            }
            ChromeToolbarAction::Search => {
                self.switch_to_columns();
                if let Some(columns) = self.get_columns_app() {
                    columns.navigate_search(ctx);
                }
            }
            ChromeToolbarAction::Notifications => {
                self.switch_to_columns();
                if let Some(columns) = self.get_columns_app() {
                    columns.navigate_notifications(ctx);
                }
            }
        }
    }

    /// Returns which ChromeToolbarAction is currently "active" based on
    /// the active app and its route. Used to highlight the current tab.
    fn active_toolbar_tab(&self, accounts: &notedeck::Accounts) -> Option<ChromeToolbarAction> {
        let active_app = &self.apps[self.active as usize];
        match active_app {
            #[cfg(feature = "messages")]
            NotedeckApp::Messages(_) => Some(ChromeToolbarAction::Chat),
            NotedeckApp::Columns(columns) => match columns.active_toolbar_tab(accounts) {
                Some(0) => Some(ChromeToolbarAction::Home),
                Some(1) => Some(ChromeToolbarAction::Search),
                Some(2) => Some(ChromeToolbarAction::Notifications),
                _ => None,
            },
            _ => None,
        }
    }

    /// Switch the active app to `app`.
    ///
    /// Every app-switch path funnels through here, so this is where the switch
    /// is recorded in the global history: unless `app` already owns the top
    /// entry (guarding a redundant duplicate), a new [`ChromeNavEntry`] is
    /// pushed via [`NavStack::route_to`], which sets the `navigating` flag so
    /// [`nav_frame`](notedeck::nav_frame) animates the transition. `active` and
    /// the `opened` bitset are updated to match — they stay authoritative for
    /// `update()`-gating, the tab strip, and focus-restore, and always agree
    /// with the stack top (see [`Chrome::sync_active_from_nav`]).
    pub fn set_active(&mut self, app: i32) {
        if let Some(nav) = self.global_nav.as_mut() {
            if nav.top().app != AppId(app as usize) {
                nav.route_to(ChromeNavEntry::new(AppId(app as usize), Rc::new(())));
            }
        }
        self.active = app;
        self.set_opened(app as usize);
    }

    /// Land a cross-app open as ONE global-history entry: push `token` tagged
    /// with the target app's `slot`, then re-derive `active` from the new top.
    ///
    /// This deliberately bypasses [`set_active`](Chrome::set_active). That
    /// records the switch as its own untyped `()` app-switch entry, which the
    /// target renders as its root; the app then routes to the opened note on
    /// top of it, so a cross-app open would land TWO entries and take two back
    /// presses to return. Here the routed entry IS the switch.
    ///
    /// Split out from [`open_note_in_app`](Chrome::open_note_in_app) so the
    /// one-entry invariant is testable without an [`AppContext`].
    #[cfg(any(feature = "headway", test))]
    fn push_app_route(&mut self, slot: usize, token: Rc<dyn std::any::Any>) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.route_to(ChromeNavEntry::new(AppId(slot), token));
        }
        self.active = slot as i32;
        self.set_opened(slot);
    }

    /// Ask the app in `slot` for the route that opening `note_id` should land
    /// on (see [`notedeck::App::open_note_route`]) and push it as one entry via
    /// [`push_app_route`](Chrome::push_app_route).
    ///
    /// Returns `false` — pushing nothing — when the slot is empty or the app
    /// has no route for the note, so the caller can fall back to a plain
    /// [`set_active`](Chrome::set_active).
    #[cfg(feature = "headway")]
    fn open_note_in_app(
        &mut self,
        ctx: &mut AppContext,
        slot: usize,
        note_id: nostrdb_net::NoteId,
    ) -> bool {
        let Some(token) = self
            .apps
            .get_mut(slot)
            .and_then(|app| app.open_note_route(ctx, note_id))
        else {
            return false;
        };
        self.push_app_route(slot, token);
        true
    }

    /// Re-derive `active` (and its `opened` bit) from the global history's top
    /// entry. Called after every stack mutation the chrome doesn't drive through
    /// [`set_active`](Chrome::set_active) — the per-frame [`nav_frame`] reconcile
    /// (which may pop on a completed back) and the [`Navigator`](notedeck::Navigator)
    /// drain — so the active app tracks whichever app owns the current route,
    /// including after a global back/forward crossed an app boundary.
    fn sync_active_from_nav(&mut self) {
        let Some(nav) = self.global_nav.as_ref() else {
            return;
        };
        let slot = nav.top().app.slot();
        self.active = slot as i32;
        self.set_opened(slot);
    }

    /// Apply the navigation `requests` an app enqueued this frame (drained from
    /// [`AppContext::navigator`](notedeck::AppContext)) to the global history,
    /// then re-derive the active app from the new top.
    ///
    /// This is the chrome's half of the [`Navigator`](notedeck::Navigator)
    /// contract: apps never touch the authoritative stack, they queue intent and
    /// the chrome applies it here. Empty in practice until an app starts pushing
    /// routes (a later subissue), but wired now so it is ready and testable.
    fn apply_nav_requests(&mut self, requests: Vec<NavRequest>) {
        if requests.is_empty() {
            return;
        }

        // The active-owned requests (`PushToActive`/`ReplaceActive`) don't name
        // their app — the enqueuing app doesn't know its own slot — so the chrome
        // completes them here by tagging the token with the active slot. This
        // runs during the same frame's render as the enqueue and before
        // `sync_active_from_nav`, so `active` still names the app that raised the
        // request (a plain app-switch funnels through `set_active`, not here).
        let active = AppId(self.active as usize);

        if let Some(nav) = self.global_nav.as_mut() {
            for request in requests {
                match request {
                    NavRequest::Push(entry) => nav.route_to(entry),
                    NavRequest::Replace(entry) => nav.route_to_replaced(entry),
                    NavRequest::PushToActive(entry) => nav.route_to(entry.tag(active)),
                    NavRequest::ReplaceActive(entry) => nav.route_to_replaced(entry.tag(active)),
                    NavRequest::Back => {
                        nav.go_back();
                    }
                    NavRequest::Forward => {
                        nav.go_forward();
                    }
                }
            }
        }

        self.sync_active_from_nav();
    }

    /// Step one entry back in the global history, then re-derive the active app.
    ///
    /// The back is deferred/animated: [`NavStack::go_back`] only flags the
    /// transition, and [`nav_frame`](notedeck::nav_frame) reconciles the pop once
    /// the slide completes — so `active` doesn't change until the following frame
    /// (the outgoing app stays rendered during the slide). No-op at the root.
    /// Shared by the header chevron, `Alt+←`, and the back mouse button.
    fn global_go_back(&mut self) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.go_back();
        }
        self.sync_active_from_nav();
    }

    /// Step one entry forward in the global history, replaying the most recently
    /// popped route, then re-derive the active app. Unlike a back step the push
    /// lands immediately, so `active` updates this frame. No-op with an empty
    /// forward stack. Shared by the header chevron, `Alt+→`, and the forward
    /// mouse button.
    fn global_go_forward(&mut self) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.go_forward();
        }
        self.sync_active_from_nav();
    }

    /// Jump straight to back-stack `index` in the global history (used by the
    /// header history dropdown), then re-derive the active app. Instant, with the
    /// skipped-over routes preserved on the forward stack for redo.
    fn global_go_to(&mut self, index: usize) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.go_to_route(index);
        }
        self.sync_active_from_nav();
    }

    /// Close the active app's tab and switch to the nearest opened app.
    /// No-op if it's the only opened app — at least one app must stay open.
    /// Used by Ctrl+W.
    fn close_active_app(&mut self) {
        let n = self.apps.len();
        if n == 0 {
            return;
        }
        // at least one app must remain open
        if self.opened_count() <= 1 {
            return;
        }
        let active = self.active.clamp(0, n as i32 - 1) as usize;
        self.clear_opened(active);
        // switch to the nearest opened app after `active`, wrapping
        for offset in 1..=n {
            let idx = (active + offset) % n;
            if self.is_opened(idx) {
                self.set_active(idx as i32);
                return;
            }
        }
    }

    /// Cycle the active app to the next (`forward`) or previous opened app,
    /// wrapping around. Used by Ctrl+Tab / Ctrl+Shift+Tab.
    fn cycle_app(&mut self, forward: bool) {
        let n = self.apps.len();
        if n == 0 {
            return;
        }
        let active = self.active.clamp(0, n as i32 - 1) as usize;
        // walk opened slots starting from the one after active, wrapping
        if let Some(idx) = bitset_find(&self.opened, active as u16, forward) {
            self.set_active(idx as i32);
        }
    }

    /// The chrome side panel
    #[profiling::function]
    fn panel(
        &mut self,
        app_ctx: &mut AppContext,
        ui: &mut egui::Ui,
        amt_keyboard_open: f32,
    ) -> Option<ChromePanelAction> {
        let drawer = NavDrawer::new(&ChromeRoute::App, &ChromeRoute::Chrome)
            .navigating(self.nav.navigating)
            .returning(self.nav.returning)
            .drawer_focused(self.nav.drawer_focused)
            .drag(is_compiled_as_mobile())
            .opened_offset(240.0);

        let resp = drawer.show_mut(ui, |ui, route| match route {
            ChromeRoute::Chrome => {
                ui.painter().rect_filled(
                    ui.available_rect_before_wrap(),
                    CornerRadius::ZERO,
                    if ui.visuals().dark_mode {
                        egui::Color32::BLACK
                    } else {
                        egui::Color32::WHITE
                    },
                );
                egui::Frame::new()
                    .inner_margin(Margin::same(notedeck::tokens::SPACING_LG as i8))
                    .show(ui, |ui| {
                        let options = if amt_keyboard_open > 0.0 {
                            SidebarOptions::Compact
                        } else {
                            SidebarOptions::default()
                        };

                        let response = ui
                            .with_layout(Layout::top_down(egui::Align::Min), |ui| {
                                topdown_sidebar(self, app_ctx, ui, options)
                            })
                            .inner;

                        ui.with_layout(Layout::bottom_up(egui::Align::Center), |ui| {
                            ui.add(milestone_name(app_ctx.i18n));
                        });

                        RouteResponse {
                            response,
                            can_take_drag_from: Vec::new(),
                        }
                    })
                    .inner
            }
            ChromeRoute::App => {
                let animate = app_ctx.settings.get_settings_mut().animate_nav_transitions;

                // `nav_frame` needs `&mut global_nav` while the render callback
                // needs `&mut apps` — split-borrow the two fields so both can be
                // lent at once. The app's action is bubbled out via the frame
                // response and routed *after* `nav_frame` returns, since the
                // callback can't reborrow the whole `self` that
                // `chrome_handle_app_action` wants.
                let (app_action, can_take_drag_from, popped) = {
                    let Chrome {
                        global_nav, apps, ..
                    } = &mut *self;
                    let nav = global_nav
                        .as_mut()
                        .expect("global nav is seeded at construction");

                    let frame = nav_frame(
                        ui,
                        egui::Id::new("chrome_global_nav"),
                        nav,
                        animate,
                        |ui, ui_type, frame_nav| match ui_type {
                            NavUiType::Body => {
                                // Paint an opaque panel background before the app
                                // draws. During an egui_nav slide the incoming
                                // route renders in a foreground layer over the
                                // outgoing one; the outgoing app's floating
                                // `egui::Area`s (popups, tooltips, side panels)
                                // escape egui_nav's route clip and would otherwise
                                // show through an app whose `render` doesn't fill
                                // its whole rect (e.g. Dave). This fill occludes
                                // whatever sits beneath the incoming route so every
                                // app switch clips cleanly. It lives here rather
                                // than in the shared, business-logic-free
                                // `nav_frame` (columns paints its own column
                                // backgrounds and has its own nav path).
                                ui.painter().rect_filled(
                                    ui.max_rect(),
                                    CornerRadius::ZERO,
                                    ui.visuals().panel_fill,
                                );
                                let entry =
                                    frame_nav.routes().last().expect("stack always has a top");
                                let resp =
                                    apps[entry.app.slot()].render_nav(app_ctx, ui, &entry.token);
                                RouteResponse {
                                    response: resp.action,
                                    can_take_drag_from: resp.can_take_drag_from,
                                }
                            }

                            // The global-nav header (back button, breadcrumb)
                            // lands with the nav controls in a later subissue.
                            NavUiType::Title => RouteResponse {
                                response: None,
                                can_take_drag_from: Vec::new(),
                            },
                        },
                    );

                    // A completed global-back popped the top entry inside
                    // `nav_frame`. Surface the popped entry (its owning app + the
                    // opaque route token) so we can hand it to that app's
                    // `cleanup_nav` once the split borrows above are released —
                    // the chrome itself never inspects the token. The token is an
                    // `Rc`, so cloning it out is a refcount bump.
                    let popped = match frame.event {
                        Some(NavStackEvent::Popped {
                            route: Some(entry), ..
                        }) => Some((entry.app, entry.token)),
                        _ => None,
                    };

                    (frame.response, frame.can_take_drag_from, popped)
                };

                // Split borrows released — free the popped entry's resources by
                // routing the pop back to the app that owned it (e.g. columns
                // closes a deep-linked thread's subscription).
                if let Some((app, token)) = popped {
                    self.apps[app.slot()].cleanup_nav(app_ctx, &token);
                }

                // Route the bubbled app action.
                if let Some(action) = app_action {
                    chrome_handle_app_action(self, app_ctx, action, ui);
                }

                // Actions raised imperatively mid-render (e.g. a clicked inline
                // widget drawn by another app's KindRenderer) arrive here rather
                // than via the bubbled action; route them the same way.
                for action in app_ctx.app_actions.take() {
                    chrome_handle_app_action(self, app_ctx, action, ui);
                }

                // A completed back may have popped the top inside `nav_frame`;
                // re-derive the active app so it tracks the new top.
                self.sync_active_from_nav();

                RouteResponse {
                    response: None,
                    can_take_drag_from,
                }
            }
        });

        if let Some(action) = resp.action {
            if matches!(action, NavAction::Returned(_)) {
                self.nav.closed();
            } else if let NavAction::Navigating = action {
                self.nav.navigating = false;
            } else if let NavAction::Navigated = action {
                self.nav.opened();
            }
        }

        resp.drawer_response?
    }

    /// Show the side menu or bar, depending on if we're on a narrow
    /// or wide screen.
    ///
    /// The side menu should hover over the screen, while the side bar
    /// is collapsible but persistent on the screen.
    #[profiling::function]
    fn show(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> Option<ChromePanelAction> {
        ui.spacing_mut().item_spacing.x = 0.0;

        let skb_anim =
            keyboard_visibility(ui, ctx, &mut self.options, &mut self.soft_kb_anim_state);

        let virtual_keyboard = self.options.contains(ChromeOptions::VirtualKeyboard);
        let keyboard_height = if self.options.contains(ChromeOptions::KeyboardVisibility) {
            skb_anim.anim_height
        } else {
            0.0
        };

        let is_narrow = notedeck::ui::is_narrow(ui.ctx());
        let toolbar_height = if is_narrow && ctx.settings.welcome_completed() {
            toolbar_visibility_height(skb_anim.skb_rect, ui)
        } else {
            0.0
        };

        // chrome-style app tab strip, desktop/wide only
        let show_app_tabs = !is_narrow && ctx.settings.welcome_completed();

        // Ctrl+Tab / Ctrl+Shift+Tab cycle through opened app tabs
        if show_app_tabs {
            let (mut next, mut prev) = (false, false);
            ui.input_mut(|i| {
                prev = i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::Tab,
                );
                next = i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab);

                // macOS swallows Ctrl+Tab via AppKit's keyboard-interface
                // control before it reaches us, so also accept the native
                // Cmd+Shift+[ / Cmd+Shift+] tab-cycling shortcuts there. The
                // MAC_CMD modifier only matches when the Cmd key is set, which
                // only happens on macOS, so this is inert on other platforms.
                //
                // Because Shift is held, the logical key egui reports is the
                // shifted glyph: `{` (OpenCurlyBracket) and `}`
                // (CloseCurlyBracket), not the bare `[` / `]`.
                prev |= i.consume_key(
                    egui::Modifiers::MAC_CMD | egui::Modifiers::SHIFT,
                    egui::Key::OpenCurlyBracket,
                );
                next |= i.consume_key(
                    egui::Modifiers::MAC_CMD | egui::Modifiers::SHIFT,
                    egui::Key::CloseCurlyBracket,
                );
            });
            if prev {
                self.cycle_app(false);
            } else if next {
                self.cycle_app(true);
            }
        }

        // Global history back/forward via keyboard (Alt+←/→, mirroring the
        // Ctrl+Tab consume above) and the mouse's dedicated back/forward buttons
        // (PointerButton::Extra1/Extra2). Handled unconditionally so it works on
        // narrow layouts too, where the header chevrons aren't drawn. Consuming
        // the keys keeps a focused app from also acting on them.
        {
            let (mut back, mut forward) = (false, false);
            ui.input_mut(|i| {
                back = i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft);
                forward = i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight);
                back |= i.pointer.button_clicked(egui::PointerButton::Extra1);
                forward |= i.pointer.button_clicked(egui::PointerButton::Extra2);
            });
            if back {
                self.global_go_back();
            }
            if forward {
                self.global_go_forward();
            }
        }

        let (unseen_notifications, active_toolbar_tab) = if is_narrow {
            let unseen = self
                .get_columns_app()
                .map(|c| c.has_unseen_notifications(ctx.accounts))
                .unwrap_or(false);
            let active = self.active_toolbar_tab(ctx.accounts);
            (unseen, active)
        } else {
            (false, None)
        };

        // if the soft keyboard is open, shrink the chrome contents
        let mut action: Option<ChromePanelAction> = None;
        let mut toolbar_action: Option<ChromeToolbarAction> = None;
        // build a strip to carve out the soft keyboard inset
        let prev_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = 0.0;
        StripBuilder::new(ui)
            .size(Size::remainder())
            .size(Size::exact(toolbar_height))
            .size(Size::exact(keyboard_height))
            .vertical(|mut strip| {
                // the actual content, shifted up because of the soft keyboard
                strip.cell(|ui| {
                    ui.spacing_mut().item_spacing = prev_spacing;
                    if show_app_tabs {
                        chrome_app_tabs(self, ctx, ui);
                    }
                    // If the active app changed this frame (Ctrl+Tab above or a
                    // tab click in chrome_app_tabs), restore that app's last
                    // keyboard focus before rendering it, so the user can type
                    // immediately instead of having to click back in. Skipped on
                    // mobile to avoid popping the virtual keyboard on every switch.
                    if self.active != self.prev_active {
                        if !is_compiled_as_mobile() {
                            if let Some(id) = self.app_focus.get(&(self.active as usize)) {
                                ui.ctx().memory_mut(|m| m.request_focus(*id));
                            } else {
                                // First activation, nothing remembered: ask the
                                // app's autofocus widget (if any) to grab focus,
                                // like browser autofocus.
                                notedeck_ui::request_autofocus(ui.ctx());
                            }
                        }
                        self.prev_active = self.active;
                    }
                    action = self.panel(ctx, ui, keyboard_height);
                });

                // mobile toolbar
                strip.cell(|ui| {
                    if toolbar_height > 0.0 {
                        toolbar_action =
                            chrome_toolbar(ui, unseen_notifications, active_toolbar_tab);
                    }
                });

                // the filler space taken up by the soft keyboard
                strip.cell(|ui| {
                    // keyboard-visibility virtual keyboard
                    if virtual_keyboard && keyboard_height > 0.0 {
                        virtual_keyboard_ui(ui, ui.available_rect_before_wrap())
                    }
                });
            });

        // hovering virtual keyboard
        if virtual_keyboard {
            if let Some(mut kb_rect) = skb_anim.skb_rect {
                let kb_height = if self.options.contains(ChromeOptions::KeyboardVisibility) {
                    keyboard_height
                } else {
                    400.0
                };
                kb_rect.min.y = kb_rect.max.y - kb_height;
                tracing::debug!("hovering virtual kb_height:{keyboard_height} kb_rect:{kb_rect}");
                virtual_keyboard_ui(ui, kb_rect)
            }
        }

        if let Some(tb_action) = toolbar_action {
            self.process_toolbar_action(tb_action, ctx);
        }

        // Remember the active app's currently-focused widget so we can restore
        // it when the user switches away and back. Only overwrite on Some so the
        // last real focus is retained even after focus is transiently dropped.
        if let Some(focused) = ui.ctx().memory(|m| m.focused()) {
            self.app_focus.insert(self.active as usize, focused);
        }

        action
    }
}

impl notedeck::App for Chrome {
    /// Re-derive the host's agent-tool registry from the currently-running apps
    /// whenever that set changes, so a backend (dave, `notedeck --mcp`) only
    /// sees tools whose app is live and syncing its data. Called by the host on
    /// the top app each frame (see `App::take_tool_update`); returns `None` when
    /// the running set is unchanged so no per-frame allocation happens.
    fn take_tool_update(&mut self) -> Option<Vec<notedeck::RegisteredTool>> {
        let all_active = self.options.contains(ChromeOptions::AllAppsActive);
        let mut running = [0u16; MAX_APPS / 16];
        for i in 0..self.apps.len().min(MAX_APPS) {
            if all_active || bitset_get(&self.opened, i as u16) {
                bitset_set(&mut running, i as u16);
            }
        }

        if running == self.tools_snapshot {
            return None;
        }
        self.tools_snapshot = running;

        Some(
            self.apps
                .iter()
                .enumerate()
                .filter(|(i, _)| bitset_get(&running, *i as u16))
                .flat_map(|(_, app)| app.tools())
                .collect(),
        )
    }

    fn update(&mut self, ctx: &mut notedeck::AppContext) {
        ctx.sound.update();

        #[cfg(feature = "auto-update")]
        poll_updater(&mut self.updater, ctx);

        // Update opened apps every frame so background processing
        // (relay pools, subscriptions, etc.) stays alive.
        // Apps that haven't been opened yet are skipped unless
        // --all-apps-active is set.
        let all_active = self.options.contains(ChromeOptions::AllAppsActive);
        for (i, app) in self.apps.iter_mut().enumerate() {
            if all_active || bitset_get(&self.opened, i as u16) {
                app.update(ctx);
            }
        }
    }

    fn render(&mut self, ctx: &mut notedeck::AppContext, ui: &mut egui::Ui) -> AppResponse {
        #[cfg(feature = "tracy")]
        {
            ui.ctx().request_repaint();
        }

        // Chrome-level keybindings — consumed before apps render,
        // so apps can never intercept them.
        self.handle_chrome_keybindings(ui.ctx());

        if let Some(action) = self.show(ctx, ui) {
            action.process(ctx, self, ui);
            self.nav.close();
        }

        // Apply any navigation requests apps enqueued while rendering to the
        // global history, then re-derive the active app. Empty until an app
        // starts pushing routes (a later subissue), but wired now so it's ready.
        let nav_requests = ctx.navigator.take();
        self.apply_nav_requests(nav_requests);

        // Fallback keybindings — only fire if no app consumed the key.
        self.handle_fallback_keybindings(ui.ctx());

        // TODO: unify this constant with the columns side panel width. ui crate?
        AppResponse::none()
    }
}

const TOOLBAR_HEIGHT: f32 = 48.0;

#[derive(Debug, Eq, PartialEq)]
enum ChromeToolbarAction {
    Home,
    #[cfg(feature = "messages")]
    Chat,
    Search,
    Notifications,
}

/// Compute the animated toolbar height, auto-hiding on scroll and
/// when the soft keyboard is open.
fn toolbar_visibility_height(skb_rect: Option<Rect>, ui: &mut Ui) -> f32 {
    let toolbar_visible_id = egui::Id::new("chrome_toolbar_visible");

    let scroll_delta = scroll_delta(ui.ctx());
    let velocity_threshold = 1.0;

    if scroll_delta > velocity_threshold {
        ui.ctx()
            .data_mut(|d| d.insert_temp(toolbar_visible_id, true));
    } else if scroll_delta < -velocity_threshold {
        ui.ctx()
            .data_mut(|d| d.insert_temp(toolbar_visible_id, false));
    }

    let toolbar_visible = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(toolbar_visible_id))
        .unwrap_or(true);

    let toolbar_anim = ui
        .ctx()
        .animate_bool_responsive(toolbar_visible_id.with("anim"), toolbar_visible);

    if skb_rect.is_none() {
        TOOLBAR_HEIGHT * toolbar_anim
    } else {
        0.0
    }
}

/// Detect vertical scroll intent from mouse wheel, trackpad, or touch drag.
fn scroll_delta(ctx: &egui::Context) -> f32 {
    ctx.input(|i| {
        let sd = i.smooth_scroll_delta.y;
        if sd.abs() > 0.5 {
            return sd;
        }
        if i.pointer.is_decidedly_dragging() {
            return i.pointer.velocity().y;
        }
        0.0
    })
}

/// Render the Chrome mobile toolbar (Home, Chat, Search, Notifications).
fn chrome_toolbar(
    ui: &mut Ui,
    unseen_notifications: bool,
    active_tab: Option<ChromeToolbarAction>,
) -> Option<ChromeToolbarAction> {
    use egui_tabs::{TabColor, Tabs};
    use notedeck_ui::icons::{home_button, notifications_button, search_button};

    let rect = ui.available_rect_before_wrap();
    let bg = if ui.visuals().dark_mode {
        Color32::BLACK
    } else {
        notedeck_ui::colors::ALMOST_WHITE
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );

    let has_chat = cfg!(feature = "messages");
    let mut next_index = 0;
    let home_index = next_index;
    next_index += 1;
    let chat_index = if has_chat {
        let i = next_index;
        next_index += 1;
        Some(i)
    } else {
        None
    };
    let search_index = next_index;
    next_index += 1;
    let notif_index = next_index;
    let tab_count = notif_index + 1;

    let rs = Tabs::new(tab_count)
        .selected(0)
        .hover_bg(TabColor::none())
        .selected_fg(TabColor::none())
        .selected_bg(TabColor::none())
        .height(TOOLBAR_HEIGHT)
        .layout(Layout::centered_and_justified(egui::Direction::TopDown))
        .show(ui, |ui, state| {
            let index = state.index();
            let btn_size: f32 = 20.0;

            if index == home_index {
                let active = active_tab == Some(ChromeToolbarAction::Home);
                if home_button(ui, btn_size, active).clicked() {
                    return Some(ChromeToolbarAction::Home);
                }
            } else if Some(index) == chat_index {
                #[cfg(feature = "messages")]
                {
                    let active = active_tab == Some(ChromeToolbarAction::Chat);
                    if notedeck_ui::icons::chat_button(ui, btn_size, active).clicked() {
                        return Some(ChromeToolbarAction::Chat);
                    }
                }
            } else if index == search_index {
                let active = active_tab == Some(ChromeToolbarAction::Search);
                if ui
                    .add(search_button(ui.visuals().text_color(), 2.0, active))
                    .clicked()
                {
                    return Some(ChromeToolbarAction::Search);
                }
            } else if index == notif_index {
                let active = active_tab == Some(ChromeToolbarAction::Notifications);
                if notifications_button(ui, btn_size, active, unseen_notifications).clicked() {
                    return Some(ChromeToolbarAction::Notifications);
                }
            }

            None
        })
        .inner();

    for maybe_r in rs {
        if maybe_r.inner.is_some() {
            return maybe_r.inner;
        }
    }

    None
}

/// Whether `note_id` refers to a headway board/issue event, so a click on its
/// inline widget routes to the Headway app instead of the timeline.
#[cfg(feature = "headway")]
fn is_headway_note(ctx: &mut AppContext, note_id: nostrdb_net::NoteId) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    ctx.ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|note| notedeck_headway::is_headway_kind(note.kind()))
        .unwrap_or(false)
}

/// Open an inline headway board/issue click in the Headway app as ONE global
/// history entry, so a single back press returns to the source app.
///
/// Headway mints the route itself (switching its active board eagerly); if it
/// can't — e.g. the note hasn't resolved to a board yet — fall back to a plain
/// app switch so the user still lands in Headway, on its board root.
#[cfg(feature = "headway")]
fn open_headway_note(chrome: &mut Chrome, ctx: &mut AppContext, note_id: nostrdb_net::NoteId) {
    let Some(slot) = chrome.headway_slot() else {
        return;
    };
    if chrome.open_note_in_app(ctx, slot, note_id) {
        return;
    }
    chrome.set_active(slot as i32);
}

/// Whether `note_id` refers to an agentium session-state event, so a click on its
/// inline widget routes to the Dave app instead of the timeline.
#[cfg(feature = "dave")]
fn is_agentium_note(ctx: &mut AppContext, note_id: nostrdb_net::NoteId) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    ctx.ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|note| notedeck_dave::is_agentium_kind(note.kind()))
        .unwrap_or(false)
}

/// Whether `note_id` refers to a notebook node event, so a click on its inline
/// widget routes to the Notebook app instead of the timeline.
#[cfg(feature = "notebook")]
fn is_notebook_note(ctx: &mut AppContext, note_id: nostrdb_net::NoteId) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    ctx.ndb
        .get_note_by_id(&txn, note_id.bytes())
        .map(|note| notedeck_notebook::is_notebook_kind(note.kind()))
        .unwrap_or(false)
}

fn chrome_handle_app_action(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    action: AppAction,
    ui: &mut egui::Ui,
) {
    match action {
        AppAction::ToggleChrome => {
            chrome.toggle();
        }

        AppAction::Note(note_action) => {
            // Intercept SummarizeThread — route to Dave instead of Columns
            #[cfg(feature = "dave")]
            if let notedeck::NoteAction::Context(ref context) = note_action {
                if let notedeck::NoteContextSelection::SummarizeThread(note_id) = context.action {
                    chrome.switch_to_dave();
                    if let Some(dave) = chrome.get_dave_app() {
                        dave.summarize_thread(note_id);
                    }
                    return;
                }
            }

            // Intercept a click on an inline agentium session widget — open it in
            // the Dave app rather than the timeline (see `is_agentium_kind`).
            #[cfg(feature = "dave")]
            if let notedeck::NoteAction::Note { note_id, .. } = &note_action {
                if is_agentium_note(ctx, *note_id) {
                    chrome.switch_to_dave();
                    if let Some(dave) = chrome.get_dave_app() {
                        dave.open(*note_id);
                    }
                    return;
                }
            }

            // Intercept a click on an inline notebook node widget — open it in the
            // Notebook app rather than the timeline (see `is_notebook_kind`).
            #[cfg(feature = "notebook")]
            if let notedeck::NoteAction::Note { note_id, .. } = &note_action {
                if is_notebook_note(ctx, *note_id) {
                    chrome.switch_to_notebook();
                    if let Some(notebook) = chrome.get_notebook_app() {
                        notebook.open(*note_id);
                    }
                    return;
                }
            }

            // Intercept a click on an inline headway board/issue widget — open it
            // in the Headway app rather than the timeline (see `is_headway_kind`).
            #[cfg(feature = "headway")]
            if let notedeck::NoteAction::Note { note_id, .. } = &note_action {
                if is_headway_note(ctx, *note_id) {
                    open_headway_note(chrome, ctx, *note_id);
                    return;
                }
            }

            chrome.switch_to_columns();
            let Some(columns) = chrome.get_columns_app() else {
                return;
            };

            let txn = Transaction::new(ctx.ndb).unwrap();

            let cols = columns
                .decks_cache
                .active_columns_mut(ctx.i18n, ctx.accounts)
                .unwrap();
            let m_action = notedeck_columns::actionbar::execute_and_process_note_action(
                note_action,
                ctx.ndb,
                cols,
                0,
                &mut columns.timeline_cache,
                &mut columns.threads,
                ctx.note_cache,
                &mut ctx.remote,
                &txn,
                ctx.unknown_ids,
                ctx.accounts,
                ctx.global_wallet,
                ctx.zaps,
                ctx.img_cache,
                &mut columns.view_state,
                ctx.media_jobs.sender(),
                ui,
                ctx.settings.columns_use_outbox_relays(),
            );

            if let Some(action) = m_action {
                let col = cols.selected_mut();

                action.process_router_action(&mut col.router, &mut col.sheet_router, ctx.sound);
            }
        }
    }
}

fn columns_route_to_profile(
    pk: &nostrdb_net::Pubkey,
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    ui: &mut egui::Ui,
) {
    chrome.switch_to_columns();
    let Some(columns) = chrome.get_columns_app() else {
        return;
    };

    let cols = columns
        .decks_cache
        .active_columns_mut(ctx.i18n, ctx.accounts)
        .unwrap();

    let router = cols.get_selected_router();
    if router.routes().iter().any(|r| {
        matches!(
            r,
            notedeck_columns::Route::Timeline(TimelineKind::Profile(_))
        )
    }) {
        router.go_back();
        return;
    }

    let txn = Transaction::new(ctx.ndb).unwrap();
    let m_action = notedeck_columns::actionbar::execute_and_process_note_action(
        notedeck::NoteAction::Profile(*pk),
        ctx.ndb,
        cols,
        0,
        &mut columns.timeline_cache,
        &mut columns.threads,
        ctx.note_cache,
        &mut ctx.remote,
        &txn,
        ctx.unknown_ids,
        ctx.accounts,
        ctx.global_wallet,
        ctx.zaps,
        ctx.img_cache,
        &mut columns.view_state,
        ctx.media_jobs.sender(),
        ui,
        ctx.settings.columns_use_outbox_relays(),
    );

    if let Some(action) = m_action {
        let col = cols.selected_mut();

        action.process_router_action(&mut col.router, &mut col.sheet_router, ctx.sound);
    }
}

/// The next set flag after `flag`, walking forwards (or backwards when
/// `forward` is false) and wrapping around the end of the bitset. `flag` itself
/// is only returned when it is the sole set flag, since the walk starts one step
/// away and comes all the way back. Returns `None` when nothing is set.
fn bitset_find<const N: usize>(set: &[u16; N], flag: u16, forward: bool) -> Option<u16> {
    let flag_count = (N * 16) as u32;
    let start = u32::from(flag);

    assert!(start < flag_count, "flag is outside the bitset");

    for offset in 1..=flag_count {
        let candidate = if forward {
            (start + offset) % flag_count
        } else {
            (start + flag_count - offset) % flag_count
        } as u16;

        if bitset_get(set, candidate) {
            return Some(candidate);
        }
    }

    None
}

#[cfg(test)]
mod tab_cycle_tests {
    use super::{bitset_find, MAX_APPS};
    use oot_bitset::bitset_set;

    const FORWARD: bool = true;
    const BACK: bool = false;

    fn bitset(flags: &[u16]) -> [u16; MAX_APPS / 16] {
        let mut set = [0u16; MAX_APPS / 16];
        for &f in flags {
            bitset_set(&mut set, f);
        }
        set
    }

    #[test]
    fn cycles_forward_and_wraps() {
        let set = bitset(&[0, 1, 6]);
        assert_eq!(bitset_find(&set, 0, FORWARD), Some(1));
        assert_eq!(bitset_find(&set, 1, FORWARD), Some(6));
        assert_eq!(bitset_find(&set, 6, FORWARD), Some(0));
    }

    #[test]
    fn cycles_backward_and_wraps() {
        let set = bitset(&[0, 1, 6]);
        assert_eq!(bitset_find(&set, 6, BACK), Some(1));
        assert_eq!(bitset_find(&set, 1, BACK), Some(0));
        assert_eq!(bitset_find(&set, 0, BACK), Some(6));
    }

    #[test]
    fn cycling_from_an_unset_flag_still_finds_neighbours() {
        // the active app is always opened, but a stale `active` must not
        // strand the cycle
        let set = bitset(&[0, 1, 6]);
        assert_eq!(bitset_find(&set, 3, FORWARD), Some(6));
        assert_eq!(bitset_find(&set, 3, BACK), Some(1));
    }

    #[test]
    fn lone_flag_cycles_to_itself_and_empty_finds_nothing() {
        assert_eq!(bitset_find(&bitset(&[2]), 2, FORWARD), Some(2));
        assert_eq!(bitset_find(&bitset(&[]), 0, FORWARD), None);
    }
}

// The global-nav wiring exercised here is independent of the auto-update
// `updater` field, which needs a live egui/ndb context to build. Gating on
// `not(auto-update)` lets these tests construct a bare `Chrome` with no context
// (the field is compiled out) while still running under the default feature set
// CI uses for `notedeck_chrome`. The render half (the `nav_frame` body) is
// exercised by compilation; the reconcile state machine it drives is covered by
// `notedeck::nav`'s own `NavStack` tests.
#[cfg(all(test, not(feature = "auto-update")))]
mod global_nav_tests {
    use super::*;
    use egui_nav::{NavAction, ReturnType};
    use notedeck::ActiveNavEntry;

    /// Build a bare chrome with a seeded global history and no apps — enough to
    /// drive the stack machinery (`set_active`, the `Navigator` drain, and the
    /// active-app derive) without an egui/ndb context. Mirrors the real
    /// constructors' seeding via [`seed_global_nav`].
    fn nav_test_chrome() -> Chrome {
        Chrome {
            active: 0,
            options: ChromeOptions::default(),
            apps: Vec::new(),
            opened: [0u16; MAX_APPS / 16],
            app_focus: HashMap::new(),
            prev_active: 0,
            tools_snapshot: [0u16; MAX_APPS / 16],
            soft_kb_anim_state: AnimState::default(),
            repaint_causes: HashMap::new(),
            nav: DrawerRouter::default(),
            global_nav: Some(seed_global_nav()),
        }
    }

    #[test]
    fn seed_starts_at_app_zero() {
        let chrome = nav_test_chrome();
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 1);
        assert_eq!(nav.top().app, AppId(0));
        assert_eq!(chrome.active, 0);
    }

    #[test]
    fn set_active_pushes_a_route_and_derives_active() {
        let mut chrome = nav_test_chrome();

        chrome.set_active(1);
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 2, "switching apps pushes a global-history entry");
        assert_eq!(nav.top().app, AppId(1));
        assert_eq!(chrome.active, 1);
        assert!(
            nav.navigating(),
            "route_to arms the forward-transition flag"
        );
        assert!(chrome.is_opened(1), "the switched-to app is marked opened");
    }

    #[test]
    fn set_active_guards_the_no_op_switch() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        assert_eq!(chrome.global_nav.as_ref().unwrap().len(), 2);

        // switching to the already-active app must not push a duplicate entry
        chrome.set_active(1);
        assert_eq!(chrome.global_nav.as_ref().unwrap().len(), 2);
        assert_eq!(chrome.active, 1);
    }

    #[test]
    fn drained_push_request_advances_the_stack() {
        let mut chrome = nav_test_chrome();

        chrome.apply_nav_requests(vec![NavRequest::Push(ChromeNavEntry::new(
            AppId(2),
            Rc::new(()),
        ))]);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.top().app, AppId(2));
        assert_eq!(chrome.active, 2, "active derives from the new stack top");
    }

    #[test]
    fn drained_replace_request_drops_the_previous_route() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // [app0, app1]

        chrome.apply_nav_requests(vec![NavRequest::Replace(ChromeNavEntry::new(
            AppId(2),
            Rc::new(()),
        ))]);
        // route_to_replaced defers the drop until the transition completes
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Navigated);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 1, "replace collapses the history to the new top");
        assert_eq!(nav.top().app, AppId(2));
        assert_eq!(chrome.active, 2);
    }

    #[test]
    fn drained_active_push_is_tagged_with_the_active_app() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(3); // active app is now slot 3

        // An app self-pushing carries only its token; the chrome fills in the
        // active slot on drain.
        chrome.apply_nav_requests(vec![NavRequest::PushToActive(ActiveNavEntry::new(
            Rc::new(7u32),
        ))]);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 3, "[app0, app3, app3-route]");
        assert_eq!(
            nav.top().app,
            AppId(3),
            "the self-push inherited the active slot"
        );
        assert_eq!(nav.top().token.downcast_ref::<u32>(), Some(&7));
        assert_eq!(chrome.active, 3);
    }

    #[test]
    fn drained_active_replace_is_tagged_with_the_active_app() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(2); // [app0, app2], active app2

        chrome.apply_nav_requests(vec![NavRequest::ReplaceActive(ActiveNavEntry::new(
            Rc::new(9u32),
        ))]);
        // route_to_replaced defers the drop until the transition completes.
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Navigated);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 1, "replace collapses the history to the new top");
        assert_eq!(
            nav.top().app,
            AppId(2),
            "the replacement kept the active slot"
        );
        assert_eq!(nav.top().token.downcast_ref::<u32>(), Some(&9));
        assert_eq!(chrome.active, 2);
    }

    #[test]
    fn note_route_push_adds_exactly_one_entry() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // the source app, e.g. Dave: [app0, app1]

        // A cross-app open lands the target's routed entry directly — the matched
        // pair to `drained_active_push_is_tagged_with_the_active_app`, which is the
        // switch-then-self-push shape that took two back presses.
        chrome.push_app_route(2, Rc::new(7u32));

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 3, "[app0, app1, app2-route]: one entry, not two");
        assert_eq!(
            nav.top().app,
            AppId(2),
            "tagged with the target, not the source"
        );
        assert_eq!(nav.top().token.downcast_ref::<u32>(), Some(&7));
        assert_eq!(chrome.active, 2);
        assert!(chrome.is_opened(2), "the opened app is marked opened");

        // ONE back returns to the source app.
        chrome.global_go_back();
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Returned(ReturnType::Click));
        chrome.sync_active_from_nav();

        assert_eq!(chrome.global_nav.as_ref().unwrap().top().app, AppId(1));
        assert_eq!(
            chrome.active, 1,
            "one back press crossed back to the source"
        );
    }

    #[test]
    fn global_back_and_forward_cross_app_boundaries() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // [app0, app1], active app1

        // A back navigation only pops once the transition completes; the chrome
        // reconciles that in `nav_frame`, so drive the same reconcile here.
        chrome.apply_nav_requests(vec![NavRequest::Back]);
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Returned(ReturnType::Click));
        chrome.sync_active_from_nav();

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.top().app, AppId(0));
        assert_eq!(chrome.active, 0, "back crossed the boundary to app0");

        // Forward replays app1 and re-derives it as active immediately.
        chrome.apply_nav_requests(vec![NavRequest::Forward]);
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.top().app, AppId(1));
        assert_eq!(chrome.active, 1, "forward crossed back to app1");
    }
}
