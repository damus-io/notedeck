// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use crate::ChromeOptions;
use eframe::CreationContext;
use notedeck::AppResponse;
use notedeck::DrawerRouter;
use notedeck::Error;
use notedeck::{App, ChromeNavEntry, NavStack, Notedeck, NotedeckOptions};
use notedeck_columns::Damus;
use oot_bitset::{bitset_get, bitset_set};

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

mod actions;
mod debug;
mod frame;
mod header;
#[cfg(all(test, feature = "headway", not(debug_assertions)))]
mod headway_nav_tests;
mod keyboard;
mod nav;
mod roster;
mod sidebar;
mod toolbar;
#[cfg(feature = "auto-update")]
mod update;

pub use actions::ChromePanelAction;
use actions::PendingOpen;
use keyboard::AnimState;
use nav::{seed_global_nav, PendingPrune};
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

    /// An [`AppAction::Open`](notedeck::AppAction::Open) whose reference didn't
    /// resolve on the frame it was raised, retried on the frames after it (see
    /// [`PendingOpen`]). At most one: a newer unresolved open replaces it.
    pending_open: Option<PendingOpen>,

    /// Prunes ([`NavRequest::RemoveActive`](notedeck::NavRequest)) raised
    /// while a `global_nav` slide was in flight, applied in order once it
    /// lands (see [`Chrome::apply_nav_requests`]).
    pending_prunes: Vec<PendingPrune>,

    /// True when `global_nav`'s egui-nav transition was still moving at the
    /// end of this frame's [`nav_frame`](notedeck::nav_frame): a slide, a
    /// drag, or a released drag springing back (see
    /// [`NavFrameResponse::in_flight`](notedeck::NavFrameResponse)). A
    /// drag-back sets neither of the stack's own transition flags, so this is
    /// what holds a prune over one.
    global_nav_in_flight: bool,

    #[cfg(feature = "auto-update")]
    updater: notedeck::updater::Updater,
}

#[derive(Clone)]
enum ChromeRoute {
    Chrome,
    App,
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
            pending_open: None,
            pending_prunes: Vec::new(),
            global_nav_in_flight: false,
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
            pending_open: None,
            pending_prunes: Vec::new(),
            global_nav_in_flight: false,
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
        // global history, then re-derive the active app.
        self.drain_nav_requests(ctx);

        // Fallback keybindings — only fire if no app consumed the key.
        self.handle_fallback_keybindings(ui.ctx());

        // TODO: unify this constant with the columns side panel width. ui crate?
        AppResponse::none()
    }
}
