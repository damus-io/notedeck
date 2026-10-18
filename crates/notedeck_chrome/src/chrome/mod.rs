// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use crate::keymap::macros::MacroRun;
use crate::keymap::KeyEngine;
use crate::ChromeOptions;
use eframe::CreationContext;
use notedeck::AppResponse;
use notedeck::DrawerRouter;
use notedeck::Error;
use notedeck::{App, Notedeck, NotedeckOptions};
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

#[cfg(feature = "cashflow")]
use notedeck_cashflow::CashflowApp;

#[cfg(feature = "clndash")]
use notedeck_ui::expanding_button;

use std::collections::HashMap;

mod actions;
#[cfg(all(test, feature = "headway", not(debug_assertions)))]
mod columns_input_tests;
mod command_bar;
mod command_search;
mod command_tools;
mod debug;
mod focus;
mod frame;
mod header;
#[cfg(all(test, feature = "headway", not(debug_assertions)))]
mod headway_nav_tests;
mod ipc;
mod jumps;
mod key_dispatch;
mod key_hints;
mod keyboard;
mod keymap_editor;
mod nav;
#[cfg(all(test, not(feature = "auto-update")))]
mod nav_scenarios;
mod open_tool;
mod roster;
mod runnable;
mod sidebar;
mod toolbar;
#[cfg(feature = "auto-update")]
mod update;
mod user_keymap;

pub use actions::ChromePanelAction;
use actions::PendingOpen;
pub use command_bar::{open_shortcut, CommandBar, OPEN_COMMAND_BAR};
use focus::AppFocus;
pub use jumps::{jump_tool_available, JumpList, JUMP_BACK, JUMP_FORWARD, JUMP_LIST_CAP};
pub use key_hints::{key_hints_tool_available, HIDE_KEY_HINTS, TOGGLE_KEY_HINTS};
use keyboard::AnimState;
pub use keymap_editor::{EDIT_KEYMAP, KEYMAP_TEMPLATE};
use nav::AppNav;
pub use nav::{NAV_BACK, NAV_FORWARD};
pub use open_tool::OPEN;
pub use runnable::{RunnableTool, ToolPlace};
#[cfg(feature = "auto-update")]
use update::poll_updater;
use user_keymap::UserKeymap;

/// Upper bound on the number of apps the running-app bitset can track. One bit
/// per app index; see [`Chrome::tools_snapshot`].
const MAX_APPS: usize = 256;

/// What the chrome's own tools are filed under in a command bar
/// ([`notedeck::ToolSpec::category`]).
const TOOL_CATEGORY: &str = "Chrome";

pub struct Chrome {
    active: i32,
    options: ChromeOptions,
    apps: Vec<NotedeckApp>,

    /// Which apps have been opened (activated), one bit per app index. Only
    /// opened apps receive `update()` calls each frame.
    opened: [u16; MAX_APPS / 16],

    /// What each app had focused when it last rendered, put back when it
    /// becomes active again (see [`AppFocus`]).
    app_focus: AppFocus,

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

    /// Each app's navigation, by slot: its own browser-style back / forward
    /// stack, and the app a cross-app open came from (see [`AppNav`]). The
    /// chrome is the sole mutator: apps request navigation via
    /// [`AppContext::navigator`](notedeck::AppContext) and the chrome drains
    /// and applies those requests to the active app's stack each frame (see
    /// [`Chrome::apply_nav_requests`]). The active app's stack is drawn
    /// through [`nav_frame`](notedeck::nav_frame), so slides inside an app
    /// animate; an app switch is instant. One per entry of `apps`, pushed by
    /// [`Chrome::add_app`].
    app_navs: Vec<AppNav>,

    /// Test-only: land every flagged slide at once instead of drawing it (see
    /// [`Chrome::settle_nav`]). Off in the app.
    settle_nav: bool,

    /// An [`AppAction::Open`](notedeck::AppAction::Open) whose reference didn't
    /// resolve on the frame it was raised, retried on the frames after it (see
    /// [`PendingOpen`]). At most one: a newer unresolved open replaces it.
    pending_open: Option<PendingOpen>,

    /// Resolves bare key presses against the active app's key scopes and the
    /// keymap, before the app renders (see [`Chrome::dispatch_keys`]).
    keys: KeyEngine,

    /// Which apps have their which-key strip pinned open by `?`, one bit per
    /// app index (see [`Chrome::key_hints_pinned`]).
    key_hints: [u16; MAX_APPS / 16],

    /// The macro a key started, while it waits for its next step to settle
    /// (see [`Chrome::advance_macro`]). One at a time: a newer macro key
    /// replaces it.
    macro_run: Option<MacroRun>,

    /// Each app's jump list, for Ctrl-O / Ctrl-I (see [`jumps`]).
    jumps: jumps::JumpLists,

    /// The Cmd-K command bar, drawn over every app while it is open (see
    /// [`command_bar`]).
    command_bar: CommandBar,

    /// The selected account's keymap overrides, merged over the defaults
    /// into `keys` (see [`Chrome::poll_user_keymap`]).
    user_keymap: UserKeymap,

    /// notedeck's socket, once [`Chrome::serve_ipc`] opens it (see [`ipc`]).
    ipc: ipc::IpcServer,

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
    /// Needs no `egui::Context`. Serves no socket: the caller opts in with
    /// [`serve_ipc`](Self::serve_ipc).
    pub fn new_headless(app_args: &[String], notedeck: &mut Notedeck) -> Result<Self, Error> {
        Self::new_with_render_state(None, app_args, notedeck)
    }

    /// Shared constructor backing [`new_with_apps`](Self::new_with_apps),
    /// [`new_headless`](Self::new_headless) and a [`shot`](crate::shot)'s
    /// offscreen window. `render_state` is `None` in the headless case, which
    /// is exactly what the GPU-backed apps already tolerate.
    pub(crate) fn new_with_render_state(
        render_state: Option<&eframe::egui_wgpu::RenderState>,
        app_args: &[String],
        notedeck: &mut Notedeck,
    ) -> Result<Self, Error> {
        let notedeck_options = notedeck.options();
        stop_debug_mode(notedeck_options);

        // A wgpu window gets the shared 3D renderer; it builds its GPU state
        // only when an app first draws a model.
        if let Some(rs) = render_state {
            notedeck.set_renderer3d(notedeck::Renderer3d::new(rs.clone()));
        }

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

        let mut chrome = Chrome::bare(&app_ref.app_ctx);

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

        #[cfg(feature = "cashflow")]
        chrome.add_app(NotedeckApp::Cashflow(Box::new(CashflowApp::from_env())));

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
        chrome.log_keymap_problems(&notedeck.app_context());

        Ok(chrome)
    }

    /// A chrome with no apps yet, on slot 0, with the
    /// built-in keymap loaded. Every constructor starts here. `ctx` only
    /// feeds the updater.
    #[cfg_attr(not(feature = "auto-update"), allow(unused_variables))]
    fn bare(ctx: &notedeck::AppContext) -> Self {
        Chrome {
            active: 0,
            options: ChromeOptions::default(),
            apps: Vec::new(),
            opened: [0u16; MAX_APPS / 16],
            app_focus: AppFocus::default(),
            prev_active: 0,
            tools_snapshot: [0u16; MAX_APPS / 16],
            soft_kb_anim_state: AnimState::default(),
            repaint_causes: HashMap::new(),
            nav: DrawerRouter::default(),
            app_navs: Vec::new(),
            settle_nav: false,
            pending_open: None,
            keys: KeyEngine::with_default_keymap(),
            key_hints: [0u16; MAX_APPS / 16],
            jumps: Default::default(),
            macro_run: None,
            command_bar: CommandBar::default(),
            user_keymap: UserKeymap::default(),
            ipc: ipc::IpcServer::default(),
            #[cfg(feature = "auto-update")]
            updater: notedeck::updater::Updater::new(
                ctx.path,
                ctx.ndb,
                ctx.waker.clone(),
                notedeck::updater::nostr::DEFAULT_RELEASE_PUBKEY,
                notedeck::updater::nostr::ReleaseChannel::from_setting(
                    ctx.settings.release_channel(),
                ),
            ),
        }
    }

    /// A headless chrome running exactly `apps`, the first of them in front.
    /// For tests that drive one app through real chrome frames.
    pub fn new_headless_with_apps(
        notedeck: &mut Notedeck,
        apps: impl IntoIterator<Item = NotedeckApp>,
    ) -> Self {
        let mut chrome = Chrome::bare(&notedeck.app_context());
        for app in apps {
            chrome.add_app(app);
        }
        chrome.set_active(0);
        setup_app_registries(notedeck, &chrome.apps);
        chrome.log_keymap_problems(&notedeck.app_context());
        chrome
    }

    /// Create a Chrome for snapshot tests — no eframe CreationContext needed.
    #[cfg(feature = "auto-update")]
    pub fn new_test(ctx: &mut notedeck::AppContext, args: &[String]) -> Self {
        let damus = Damus::new(ctx, args);
        let mut chrome = Chrome::bare(ctx);
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
    fn handle_chrome_keybindings(
        &mut self,
        app_ctx: &mut notedeck::AppContext,
        ctx: &egui::Context,
    ) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11)) {
            self.toggle();
        }

        // Cmd-K on macOS, Ctrl-Space elsewhere, opens or closes the
        // command bar.
        let open = command_bar::open_shortcut(ctx.os());
        if ctx.input_mut(|i| command_bar::consume_exact(i, open.modifiers, open.logical_key)) {
            self.toggle_command_bar(app_ctx, ctx);
        }

        // Ctrl+W (Cmd+W on macOS) closes the active app's tab.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            let dropped = self.close_active_app();
            self.cleanup_entries(app_ctx, dropped);
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
    /// Re-derive the host's tool registry from the currently-running apps
    /// whenever that set changes, so a backend (dave, `notedeck --mcp`) only
    /// sees tools whose app is live and syncing its data. Each app's UI tools
    /// are tagged with its slot, so a call to one reaches the right app (see
    /// [`Chrome::call_ui_tool`]). Called by the host on the top app each frame
    /// (see `App::take_tool_update`); returns `None` when the running set is
    /// unchanged so no per-frame allocation happens.
    fn take_tool_update(&mut self) -> Option<notedeck::ToolUpdate> {
        let mut running = [0u16; MAX_APPS / 16];
        for i in 0..self.apps.len().min(MAX_APPS) {
            if self.is_running(i) {
                bitset_set(&mut running, i as u16);
            }
        }

        if running == self.tools_snapshot {
            return None;
        }
        self.tools_snapshot = running;

        let running_apps = || {
            self.apps
                .iter()
                .enumerate()
                .filter(move |(i, _)| bitset_get(&running, *i as u16))
        };
        // The chrome's own UI tools go first, so no app can take their names.
        let chrome_tools = key_hints::chrome_ui_tools()
            .into_iter()
            .chain(keymap_editor::keymap_editor_tools())
            .chain(jumps::jump_tools())
            .chain(nav::nav_tools())
            .chain(command_bar::command_bar_tools())
            .chain(open_tool::open_tools())
            .map(|spec| notedeck::UiToolEntry::shell(spec.filed_under(TOOL_CATEGORY)));
        Some(notedeck::ToolUpdate {
            tools: running_apps()
                .flat_map(|(i, app)| {
                    app.tools()
                        .into_iter()
                        .map(move |tool| tool.owned_by(notedeck::AppId(i)))
                })
                .collect(),
            ui_tools: chrome_tools
                .chain(running_apps().flat_map(|(i, app)| {
                    app.ui_tools()
                        .into_iter()
                        .map(move |spec| notedeck::UiToolEntry::new(spec, notedeck::AppId(i)))
                }))
                .collect(),
        })
    }

    fn update(&mut self, ctx: &mut notedeck::AppContext) {
        ctx.sound.update();

        #[cfg(feature = "auto-update")]
        poll_updater(&mut self.updater, ctx);

        self.poll_user_keymap(ctx);

        // Before the apps update, so an app a request starts runs this tick.
        #[cfg(unix)]
        self.poll_ipc(ctx);

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
        self.handle_chrome_keybindings(ctx, ui.ctx());
        self.command_bar_keys(ctx, ui);

        // Intents socket requests raised in `update`.
        #[cfg(unix)]
        self.apply_ipc_intents(ctx, ui);

        if let Some(action) = self.show(ctx, ui) {
            action.process(ctx, self, ui);
            self.nav.close();
        }

        // The command bar floats over everything drawn above.
        self.command_bar_ui(ctx, ui);

        // Apply any navigation requests the active app enqueued while
        // rendering to its stack.
        self.drain_nav_requests(ctx);

        // A test that settles nav lands what this frame flagged now, so the
        // stack it reads between frames is already where the slide would end.
        let settled = self.settle_active_nav();
        self.cleanup_entries(ctx, settled);

        // With this frame's navigation applied, a macro waiting on it may
        // run its next step.
        self.advance_macro(ctx, ui);

        // Fallback keybindings — only fire if no app consumed the key.
        self.handle_fallback_keybindings(ui.ctx());

        // TODO: unify this constant with the columns side panel width. ui crate?
        AppResponse::none()
    }
}
