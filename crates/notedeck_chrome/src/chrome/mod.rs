// Entry point for wasm
//#[cfg(target_arch = "wasm32")]
//use wasm_bindgen::prelude::*;
use crate::app::NotedeckApp;
use crate::ChromeOptions;
use eframe::CreationContext;
use egui::{CornerRadius, Layout, Margin};
use egui_extras::{Size, StripBuilder};
use egui_nav::RouteResponse;
use egui_nav::{NavAction, NavDrawer, NavUiType};
use notedeck::ui::is_compiled_as_mobile;
use notedeck::AppResponse;
use notedeck::DrawerRouter;
use notedeck::Error;
use notedeck::{
    nav_frame, App, AppContext, ChromeNavEntry, NavStack, NavStackEvent, Notedeck, NotedeckOptions,
};
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
mod header;
mod keyboard;
mod nav;
mod roster;
mod sidebar;
mod toolbar;
#[cfg(feature = "auto-update")]
mod update;

use actions::chrome_handle_app_action;
pub use actions::ChromePanelAction;
use header::chrome_app_tabs;
use keyboard::{keyboard_visibility, virtual_keyboard_ui, AnimState};
use nav::seed_global_nav;
use sidebar::{milestone_name, topdown_sidebar, SidebarOptions};
use toolbar::{chrome_toolbar, toolbar_visibility_height, ChromeToolbarAction};
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
