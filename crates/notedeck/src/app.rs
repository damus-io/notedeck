use crate::account::FALLBACK_PUBKEY;
use crate::i18n::Localization;
use crate::nip05::Nip05Cache;
use crate::persist::{AppSizeHandler, SettingsHandler};
use crate::remote_data::RemoteState;
use crate::runtime::{AppAsyncRuntime, RuntimeThreadBudget};
use crate::wallet::GlobalWallet;
use crate::zaps::{ZapVerifier, Zaps};
use crate::Error;
use crate::NotedeckOptions;
use crate::{
    frame_history::FrameHistory, AccountStorage, Accounts, AppContext, Args, DataPath,
    DataPathType, Directory, Images, NoteAction, NoteCache, UnknownIds, Waker,
};
use crate::{JobCache, JobPool, MediaJobs};
use egui::Margin;
use egui::ThemePreference;
use egui_winit::clipboard::Clipboard;
use enostr::{NormRelayUrl, RelayId};
use nostrdb::{Config, Ndb, Transaction};
use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};
use unic_langid::{LanguageIdentifier, LanguageIdentifierError};

#[cfg(target_os = "android")]
use android_activity::AndroidApp;

pub enum AppAction {
    Note(NoteAction),
    ToggleChrome,
}

/// A frame-local queue of [`AppAction`]s raised imperatively *during* rendering
/// rather than returned up the stack via [`AppResponse`].
///
/// Inline [`KindRenderer`](crate::KindRenderer) widgets are drawn deep inside a
/// host app's UI (a notebook node, a chat message) with no clean return path back
/// to that app's [`AppResponse::action`], so a clicked widget pushes its action
/// here instead. The shell drains it after each app's `render` (see
/// [`take`](Self::take)) and routes every action exactly like `AppResponse::action`.
#[derive(Default)]
pub struct AppActionQueue {
    actions: Vec<AppAction>,
}

impl AppActionQueue {
    /// Queue an action to be routed by the shell after this frame's render.
    pub fn push(&mut self, action: AppAction) {
        self.actions.push(action);
    }

    /// Take the queued actions, leaving the queue empty. Called by the shell once
    /// per frame; the moved-out `Vec` hands off this frame's allocation.
    pub fn take(&mut self) -> Vec<AppAction> {
        std::mem::take(&mut self.actions)
    }
}

/// Notification badge state for an app's chrome tab.
///
/// Apps report this via [`App::tab_notifications`] so the chrome tab strip can
/// render a badge (e.g. unread DMs on Messages, items needing input on Dave).
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabNotifications {
    /// A count to display in the badge. Zero means no badge.
    pub count: u32,
}

impl TabNotifications {
    /// A badge showing `count`. A count of zero renders no badge.
    pub fn count(count: u32) -> Self {
        Self { count }
    }

    /// Whether there's anything to show.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Construction-time configuration for remote bridge behavior.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotedeckRemoteConfig {
    pong_timeout: Option<Duration>,
}

impl NotedeckRemoteConfig {
    /// Override the websocket pong timeout used by the remote bridge.
    pub fn with_pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = Some(timeout);
        self
    }
}

pub trait App {
    /// Background processing — called every frame for ALL apps, including under
    /// `--headless` where nothing renders.
    ///
    /// Everything this needs is on [`AppContext`]: `ctx.wake()` to ask the host
    /// for another pass, `ctx.waker` to clone into a worker, and
    /// `ctx.egui` — `None` headless — for the rare display-coupled read.
    fn update(&mut self, _ctx: &mut AppContext<'_>) {}

    /// UI rendering — called only for the active/visible app.
    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse;

    /// Render one entry of the chrome-owned global navigation history.
    ///
    /// The chrome owns a single browser-style
    /// [`NavStack<ChromeNavEntry>`](crate::NavStack) spanning every app (see
    /// [`crate::navigator`]). When it draws an entry it belongs to *this* app, it
    /// calls `render_nav` with that entry's [`ChromeNavEntry::token`](crate::ChromeNavEntry)
    /// so the app can draw the specific view the token names, rather than its
    /// whole self.
    ///
    /// ## Token contract
    ///
    /// `token` is exactly the `Rc<dyn Any>` this app pushed onto the history (via
    /// [`Navigator::push_route`](crate::Navigator::push_route) or friends). Two
    /// properties matter to the app:
    ///
    /// - **Identity + cheap `Clone`.** The token is an opaque `Rc<dyn Any>` the
    ///   chrome never inspects — it only hands it back here. `Rc` makes cloning it
    ///   (which egui-nav does per transition) a refcount bump, not a deep copy.
    /// - **Downcast to the app's own route type.** The app calls
    ///   [`downcast_ref`](Any::downcast_ref) to recover the route value it stored
    ///   and renders that view. A token it doesn't recognize (a different app's
    ///   type, or a route it no longer serves) must fall back to
    ///   [`render`](Self::render) or a safe default — never panic.
    ///
    /// Like every `*_ui` path, `render_nav` runs **every frame**: downcast and
    /// match on the borrowed token, and do not allocate here (CLAUDE.md rule 18).
    ///
    /// The default implementation ignores the token and renders the whole app, so
    /// a single-view app that never pushes routes needs no token handling at all.
    fn render_nav(
        &mut self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        token: &Rc<dyn Any>,
    ) -> AppResponse {
        let _ = token;
        self.render(ctx, ui)
    }

    /// A short human-readable title for one entry of the chrome-owned global
    /// navigation history, shown in the chrome's history dropdown.
    ///
    /// `token` is the same opaque route token handed to [`render_nav`](Self::render_nav):
    /// the app downcasts it and returns the title of *that specific view* (e.g. a
    /// thread's subject), or `None` to let the chrome fall back to the app's
    /// label. Defaults to `None` — a single-view app that never pushes routes has
    /// no per-route title, so every one of its entries reads as the app itself.
    ///
    /// Like every `*_ui`-adjacent path this may be called per frame while a
    /// dropdown is open; keep it cheap and avoid allocating beyond the returned
    /// title.
    fn nav_title(&self, token: &Rc<dyn Any>) -> Option<String> {
        let _ = token;
        None
    }

    /// Free the resources a popped global-history entry owned.
    ///
    /// The chrome owns the single browser-style
    /// [`NavStack<ChromeNavEntry>`](crate::NavStack); when a back navigation
    /// completes it pops the top entry and, off that
    /// [`NavStackEvent::Popped`](crate::NavStackEvent::Popped), calls
    /// `cleanup_nav` on the app the popped entry belonged to, handing back the
    /// same [`ChromeNavEntry::token`](crate::ChromeNavEntry) that
    /// [`render_nav`](Self::render_nav) drew. This is the app's chance to tear
    /// down whatever that route opened — a thread/timeline subscription, view
    /// state — mirroring how a per-pane nav frees a popped route (see columns'
    /// `cleanup_popped_route`). The chrome stays business-logic-free: it never
    /// inspects the token, it only routes the pop back to the owning app.
    ///
    /// Same [`token`](Self::render_nav) contract: downcast to the app's own route
    /// type and, for a token it doesn't recognize (a plain app-switch entry
    /// carries a `()` token, or a different app's type), do nothing. The default
    /// is a no-op, so an app whose routes own no external resources — or that
    /// never pushes routes — needs no cleanup at all.
    fn cleanup_nav(&mut self, ctx: &mut AppContext<'_>, token: &Rc<dyn Any>) {
        let _ = (ctx, token);
    }

    /// Notification badge state for this app's chrome tab. Defaults to none.
    fn tab_notifications(&self, _ctx: &AppContext<'_>) -> TabNotifications {
        TabNotifications::default()
    }

    /// Renderers this app contributes for nostr events embedded inline (by
    /// kind), e.g. a notebook note referencing one of the app's entities. These
    /// are registered once at startup so references resolve even for apps the
    /// user never opens. Defaults to none.
    fn kind_renderers(&self) -> Vec<Box<dyn crate::KindRenderer>> {
        Vec::new()
    }

    /// Reference parsers this app contributes, one per `scheme:` it owns (e.g.
    /// `headway:`), turning a text token into a resolvable nostr entity. Like
    /// [`kind_renderers`](Self::kind_renderers) — and unlike [`tools`](Self::tools)
    /// — these are registered once at startup for all apps, so a reference
    /// resolves even for an app the user never opened. Defaults to none. See
    /// [`ReferenceParser`](crate::ReferenceParser).
    fn reference_parsers(&self) -> Vec<Box<dyn crate::ReferenceParser>> {
        Vec::new()
    }

    /// Agent tools this app contributes over its nostr-backed data, for AI
    /// backends (Dave's OpenAI loop, a future `notedeck --mcp`). The shell
    /// collects these from its *running* apps (see [`take_tool_update`](Self::take_tool_update)),
    /// so a tool is only advertised while its app is live and syncing its data.
    /// Defaults to none. See [`AppTool`](crate::AppTool)/[`RegisteredTool`](crate::RegisteredTool).
    fn tools(&self) -> Vec<crate::RegisteredTool> {
        Vec::new()
    }

    /// Host hook, invoked each frame **only on the top-level app** (the shell):
    /// return `Some(tools)` to replace the host's agent-tool
    /// [`ToolRegistry`](crate::ToolRegistry) when the contributed set changes, or
    /// `None` to leave it untouched. The body lives in the shell (it diffs its
    /// running apps and aggregates their [`tools`](Self::tools)); this defaulted
    /// declaration exists only so the host can call it across the erased
    /// `dyn App`, exactly like [`render`](Self::render). Because the host only
    /// ever invokes it on the top app, an ordinary app can't replace the registry.
    fn take_tool_update(&mut self) -> Option<Vec<crate::RegisteredTool>> {
        None
    }
}

#[derive(Default)]
pub struct AppResponse {
    pub action: Option<AppAction>,
    pub can_take_drag_from: Vec<egui::Id>,
}

impl AppResponse {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn action(action: Option<AppAction>) -> Self {
        Self {
            action,
            can_take_drag_from: Vec::new(),
        }
    }

    pub fn drag(mut self, can_take_drag_from: Vec<egui::Id>) -> Self {
        self.can_take_drag_from.extend(can_take_drag_from);
        self
    }
}

/// Main notedeck app framework
pub struct Notedeck {
    ndb: Ndb,
    img_cache: Images,
    unknown_ids: UnknownIds,
    remote: RemoteState,
    _app_async_runtime: AppAsyncRuntime,
    note_cache: NoteCache,
    accounts: Accounts,
    global_wallet: GlobalWallet,
    path: DataPath,
    args: Args,
    settings: SettingsHandler,
    app: Option<Rc<RefCell<dyn App>>>,
    app_size: AppSizeHandler,
    unrecognized_args: BTreeSet<String>,
    clipboard: Clipboard,
    zaps: Zaps,
    zap_verifier: ZapVerifier,
    frame_history: FrameHistory,
    job_pool: JobPool,
    media_jobs: MediaJobs,
    nip05_cache: Nip05Cache,
    i18n: Localization,
    sound: crate::SoundManager,
    /// Account-wide private-note (PNS kind-1080) sync over the account's private
    /// relays, pumped from [`tick`](Self::tick) independent of the foregrounded
    /// app. `None` under the test harness (no Tokio runtime, must not open a
    /// dedicated relay pool), matching the embedded `local_relay` gating.
    host_private_sync: Option<crate::HostPrivateSync>,
    /// SNS team roots apps registered for the host to sync (see
    /// [`AppContext::register_team_root`](crate::AppContext::register_team_root)).
    /// Handed to each frame's `AppContext` as a `&mut` field and read by
    /// [`pump_host_private_sync`](Self::pump_host_private_sync).
    private_channels: crate::PrivateChannels,
    /// Read-only, app-contributed registries handed to each frame's
    /// [`AppContext`](crate::AppContext): inline kind renderers (populated at
    /// startup) and agent tools (reset per-frame from the running apps).
    registries: crate::AppRegistries,
    /// Actions raised imperatively during a frame (e.g. a clicked inline
    /// [`KindRenderer`](crate::KindRenderer) widget), drained and routed by the
    /// shell after each app's `render`.
    app_actions: AppActionQueue,
    /// Frame-local navigation requests apps enqueue this frame (see
    /// [`Navigator`](crate::Navigator)), drained and applied to the chrome-owned
    /// global history after render.
    navigator: crate::Navigator,

    /// Embedded localhost nostr relay, when enabled. Held so it shuts down with
    /// the app (its `Drop` stops the accept loop). Gated behind the `local-relay`
    /// feature, which is disabled for Android builds.
    #[allow(dead_code)]
    local_relay: Option<nostrdb_net::relay::server::RelayHandle>,

    /// This host's egui context, or `None` under `--headless`, where there is no
    /// window to read input from, send viewport commands to, or animate.
    ///
    /// Handed to each frame's [`AppContext::egui`](crate::AppContext::egui). A
    /// handle rather than the live pass context: `egui::Context` is an `Arc` over
    /// the shared per-viewport state, so the one captured at init reads the same
    /// input and screen rect as the one eframe passes to `update`.
    egui: Option<egui::Context>,

    /// How anything off the render thread asks this host for another pass:
    /// `request_repaint` in the GUI, a `Notify` signal headless.
    ///
    /// Built once at init and handed to every app through
    /// [`AppContext::waker`](crate::AppContext::waker), so an app's background
    /// path can wake the host without knowing which host it is running under.
    waker: Waker,

    /// Monotonic count of [`tick_core`](Self::tick_core) passes, and the clock
    /// the texture caches age their entries against.
    ///
    /// Host-owned rather than read off [`egui::Context::cumulative_pass_nr`]:
    /// it is notedeck's clock for notedeck's caches, and a headless tick has no
    /// window whose pass number would mean anything. Published to the caches
    /// once per pass by [`TexturesCache::begin_pass`](crate::TexturesCache::begin_pass)
    /// so that every read and write within a pass agrees on it.
    pass_nr: u64,

    /// Headless, what [`waker`](Self::waker) signals: the run loop awaits this to
    /// sleep-until-event instead of busy-polling a fixed interval, standing in
    /// for the eframe integration that would otherwise schedule the next frame.
    /// `None` in GUI mode, where a wake is a repaint and eframe drives the
    /// cadence. Exposed to the loop by [`headless_waker`](Self::headless_waker).
    headless_wake: Option<Arc<tokio::sync::Notify>>,

    #[cfg(target_os = "android")]
    android_app: Option<AndroidApp>,
}

impl Drop for Notedeck {
    fn drop(&mut self) {
        self.shutdown_app();
    }
}

/// Our chrome, which is basically nothing
fn main_panel(style: &egui::Style) -> egui::CentralPanel {
    egui::CentralPanel::default().frame(egui::Frame {
        inner_margin: Margin::ZERO,
        fill: style.visuals.panel_fill,
        ..Default::default()
    })
}

/// Run the active app's per-frame work: its background `update` (every frame,
/// for every app) followed — when there is a pass to draw into — by the active
/// app's egui `render` inside the main panel.
///
/// The `render` call is the sole display dependency in the whole per-frame path
/// (`main_panel().show()` needs a real egui/display stack); `update` needs no
/// context at all. So `egui: None` skips only the render, and a server run
/// drives every app's background loop without a GPU surface. See
/// [`Notedeck::tick_headless`].
#[profiling::function]
fn render_notedeck(
    app: Rc<RefCell<dyn App + 'static>>,
    app_ctx: &mut AppContext,
    egui: Option<&egui::Context>,
) {
    app.borrow_mut().update(app_ctx);
    let Some(ctx) = egui else {
        return;
    };
    main_panel(&ctx.style()).show(ctx, |ui| {
        app.borrow_mut().render(app_ctx, ui);
    });
}

impl eframe::App for Notedeck {
    #[profiling::function]
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        profiling::finish_frame!();
        self.frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);
        self.tick(ctx);
    }

    /// Called by the framework to save state before shutdown.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        //eframe::set_value(storage, eframe::APP_KEY, self);
    }
}

#[cfg(feature = "puffin")]
fn setup_puffin() {
    info!("setting up puffin");
    puffin::set_scopes_on(true); // tell puffin to collect data
}

impl Notedeck {
    /// Core per-frame logic, independent of eframe::Frame.
    /// Called by `eframe::App::update` in production and directly in tests.
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.tick_core(Some(ctx));
    }

    /// The host's current pass number: how many [`tick_core`](Self::tick_core)
    /// passes have run, and the clock the texture caches age entries against.
    pub fn pass_nr(&self) -> u64 {
        self.pass_nr
    }

    /// Headless per-frame tick: the background half of [`tick`](Self::tick)
    /// without any egui render pass.
    ///
    /// Drives exactly the same background work as `tick` — media jobs, texture
    /// eviction, `remote.poll_bridge()`, `pump_host_private_sync()`,
    /// `nip05_cache.poll()`, `zap_verifier.poll()`, `accounts.update()`,
    /// `zaps.process()`, every app's `update()`, unknown-id resolution and the
    /// outbox flush — but skips `main_panel().show()` and the display-pref
    /// persistence tail (zoom/theme/locale/window-size), which only make sense
    /// with a real window. Both entry points share [`tick_core`](Self::tick_core)
    /// so the headless path can't silently drift from production's background
    /// behaviour.
    ///
    /// Intended for a `--headless` server run with no eframe window / wgpu
    /// surface — and with no [`egui::Context`] anywhere, which is why this takes
    /// none. Wakes reach the run loop through
    /// [`headless_waker`](Self::headless_waker) directly, apps see
    /// [`AppContext::egui`](crate::AppContext::egui) as `None`, and the texture
    /// caches run off the host's own [`pass_nr`](Self::pass_nr).
    ///
    /// ## Render-side side effects skipped headless (audit)
    ///
    /// Because the render pass never runs, the work the chrome does *only* in
    /// `Chrome::render`/`Chrome::show` never fires: draining nav requests
    /// (`Navigator::take` + `apply_nav_requests`), popping/`cleanup_nav`,
    /// keybindings, focus restoration, and draining the [`AppActionQueue`]
    /// (`app_actions.take`). None of that is required for background
    /// correctness:
    ///
    /// - **Nav requests** ([`Navigator`]) and **app actions**
    ///   ([`AppActionQueue`]) are *only ever produced during render* — apps
    ///   enqueue them from `render`/`render_nav` and inline `KindRenderer`
    ///   widgets, never from `update`. With no render pass, both queues stay
    ///   empty, so skipping their drain leaks nothing and drops no work an
    ///   `update` loop depends on.
    /// - **Keybindings / focus** are input-driven UI concerns with no
    ///   background counterpart.
    ///
    /// The one behaviour that legitimately does not happen headless is
    /// render-time subscription seeding done inside an app's `render`/
    /// `render_nav` (rather than its `update`). That is an app-level concern,
    /// not a seam concern: `--headless` runs with `AllAppsActive`, and apps are
    /// expected to drive their background sync from `update` (the smoke-test
    /// subcard verifies ingest end-to-end).
    pub fn tick_headless(&mut self) {
        self.tick_core(None);
    }

    /// Wake signal for the headless run loop, present only when booted without
    /// a display. Every [`AppContext::wake`](crate::AppContext::wake), nostrdb
    /// ingest callback, relay-bridge fact and worker-thread wake signals it
    /// directly — so the loop hears about relay traffic *and* about any app or
    /// worker that has produced something (dave streaming a token, a budgeted
    /// loop yielding mid-work), and otherwise sleeps until its idle cap.
    ///
    /// `None` in GUI mode, where wakes are repaints and eframe drives the
    /// cadence.
    pub fn headless_waker(&self) -> Option<Arc<tokio::sync::Notify>> {
        self.headless_wake.clone()
    }

    /// Shared body of [`tick`](Self::tick) and [`tick_headless`](Self::tick_headless).
    ///
    /// Everything up to and including the outbox flush is background work that
    /// runs in both modes. `egui` — `None` headless, this pass's context in the
    /// GUI — gates only the display-coupled steps: the active app's render pass
    /// (via [`render_notedeck`]) and the trailing display-preference
    /// persistence. When no app is installed the function returns early in both
    /// modes, so neither the render nor the persistence tail runs.
    fn tick_core(&mut self, egui: Option<&egui::Context>) {
        // The pass number is the clock the texture caches age entries against,
        // and publishing it is the only way one enters the cache layer — so the
        // reads the render path makes below, the writes the job seam makes, and
        // the sweep's notion of age are all on this one number.
        self.pass_nr += 1;
        self.img_cache.textures.begin_pass(self.pass_nr);

        {
            profiling::scope!("media jobs");
            self.media_jobs.run_received(&mut self.job_pool, |id| {
                crate::run_media_job_pre_action(id, &mut self.img_cache.textures);
            });
            self.media_jobs.deliver_all_completed(|completed| {
                crate::deliver_completed_media_job(completed, &mut self.img_cache.textures)
            });
        }

        // Bound GPU texture memory before drawing anything. Doing it here rather
        // than after the UI means no texture can be dropped while this pass
        // still holds a reference to it.
        self.img_cache.textures.evict_over_budget();

        self.remote.poll_bridge();
        self.pump_host_private_sync();
        self.nip05_cache.poll();
        self.zap_verifier.poll(&self.ndb, self.zaps.pay_cache());
        let Some(app) = self.app.clone() else {
            return;
        };

        // Let the shell refresh the agent-tool registry from its running apps
        // before we hand a borrow of it to this frame's AppContext.
        if let Some(tools) = app.borrow_mut().take_tool_update() {
            self.registries.tools.reset(tools);
        }

        let mut app_ref = self.notedeck_ref();

        {
            let app_ctx = &mut app_ref.app_ctx;
            // Host remote frame order: relay traffic has already been ingested into NDB;
            // `Accounts::update` must refresh local projections before author-outbox
            // demand reconciles against the planner's local relay-list snapshot.
            app_ctx.accounts.update(app_ctx.ndb, &mut app_ctx.remote);
            app_ctx
                .zaps
                .process(app_ctx.accounts, app_ctx.global_wallet, app_ctx.ndb);
        }

        render_notedeck(app, &mut app_ref.app_ctx, egui);

        {
            let app_ctx = &mut app_ref.app_ctx;
            let use_outbox_relays = app_ctx.settings.columns_use_outbox_relays();
            let mut oneshot = app_ctx.remote.oneshot();
            crate::unknown_id_send(app_ctx.unknown_ids, &mut oneshot, use_outbox_relays);
        }

        {
            profiling::scope!("outbox ingestion");
            app_ref.app_ctx.remote.flush();
            drop(app_ref);
        }

        // Display-preference persistence below reads the context's zoom/theme and
        // the window size, none of which a headless run has.
        let Some(ctx) = egui else {
            return;
        };

        self.settings.update_batch(|settings| {
            settings.zoom_factor = ctx.zoom_factor();
            settings.locale = self.i18n.get_current_locale().to_string();
            settings.theme = if ctx.style().visuals.dark_mode {
                ThemePreference::Dark
            } else {
                ThemePreference::Light
            };
        });
        self.app_size.try_save_app_size(ctx);

        #[cfg(feature = "puffin")]
        puffin_egui::profiler_window(ctx);
    }

    /// Drive the host's account-wide private-note (PNS kind-1080) sync for the
    /// selected account. Runs every frame regardless of the foregrounded app, so
    /// private notes sync whether or not the app that owns them is open. A no-op
    /// when the host is disabled (tests) or the selected account has no secret (a
    /// pubkey-only account cannot derive its PNS keypair).
    fn pump_host_private_sync(&mut self) {
        let Some(host) = self.host_private_sync.as_mut() else {
            return;
        };
        let Some(filled) = self.accounts.selected_filled() else {
            return;
        };
        let account = *filled.pubkey;
        let secret = filled.secret_key.secret_bytes();
        // The private set only ever holds websocket relays (built from the
        // kind-10013 url list); the match is just exhaustiveness.
        let urls: Vec<NormRelayUrl> = self
            .accounts
            .selected_account_private_relays()
            .into_iter()
            .filter_map(|relay| match relay {
                RelayId::Websocket(url) => Some(url),
                RelayId::Multicast => None,
            })
            .collect();
        // SNS channels apps asked the host to sync (e.g. the notebook's derived
        // vault) — unioned onto the account's key-share roster inside `update`.
        let app_roots = self.private_channels.roots();
        host.update(&mut self.ndb, &account, &secret, &urls, &app_roots);
    }

    /// Force-enable the host private-note sync even under the test harness, where
    /// it is off by default. PNS end-to-end tests call this to exercise the host as
    /// the account's inbound sync path (production enables it unconditionally when
    /// not in test mode). Idempotent; the `Session` is still spawned lazily on the
    /// first pumped frame, so this must run within a Tokio runtime to take effect.
    pub fn enable_host_private_sync_for_test(&mut self) {
        self.host_private_sync
            .get_or_insert_with(crate::HostPrivateSync::new);
    }

    /// Shuts down app-owned runtime state before dropping the host.
    pub fn shutdown_app(&mut self) {
        self.app.take();
    }

    #[cfg(target_os = "android")]
    pub fn set_android_context(&mut self, context: AndroidApp) {
        self.android_app = Some(context);
    }

    /// Boot with a window: `ctx` is the host's egui context, which becomes both
    /// the wake seam and what apps read the display through.
    pub fn init<P: AsRef<Path>>(ctx: &egui::Context, data_path: P, args: &[String]) -> Self {
        Self::init_with_remote_config(ctx, data_path, args, NotedeckRemoteConfig::default())
    }

    /// Boot with no window at all — the `--headless` run loop.
    ///
    /// Wakes go to the [`headless_waker`](Self::headless_waker) of the host this
    /// builds, and apps see [`AppContext::egui`](crate::AppContext::egui) as
    /// `None`. There is nothing an `egui::Context` would be for here: nothing
    /// renders, nothing reads input, and no texture is ever allocated off a
    /// background path.
    pub fn init_headless<P: AsRef<Path>>(data_path: P, args: &[String]) -> Self {
        Self::init_inner(None, data_path, args, NotedeckRemoteConfig::default())
    }

    pub fn init_with_remote_config<P: AsRef<Path>>(
        ctx: &egui::Context,
        data_path: P,
        args: &[String],
        remote_config: NotedeckRemoteConfig,
    ) -> Self {
        Self::init_inner(Some(ctx), data_path, args, remote_config)
    }

    fn init_inner<P: AsRef<Path>>(
        egui: Option<&egui::Context>,
        data_path: P,
        args: &[String],
        remote_config: NotedeckRemoteConfig,
    ) -> Self {
        #[cfg(feature = "puffin")]
        setup_puffin();

        install_crypto();

        // Skip the first argument, which is the program name.
        let (parsed_args, unrecognized_args) = Args::parse(&args[1..]);

        let data_path = parsed_args
            .datapath
            .clone()
            .unwrap_or(data_path.as_ref().to_str().expect("db path ok").to_string());
        let path = DataPath::new(&data_path);
        let dbpath_str = parsed_args.db_path(&path).to_str().unwrap().to_string();

        let _ = std::fs::create_dir_all(&dbpath_str);

        let img_cache_dir = path.path(DataPathType::Cache);
        let _ = std::fs::create_dir_all(img_cache_dir.clone());

        let map_size = if parsed_args.options.contains(NotedeckOptions::Tests) {
            32usize * 1024usize * 1024usize
        } else if cfg!(target_os = "windows") {
            // 16 Gib on windows because it actually creates the file
            1024usize * 1024usize * 1024usize * 16usize
        } else {
            // 1 TiB for everything else since its just virtually mapped
            1024usize * 1024usize * 1024usize * 1024usize
        };

        let mut settings = SettingsHandler::new(&path).load();

        // Is there a window? `--headless` says no even when a caller handed one
        // in, and so does handing in nothing. That one question settles both the
        // wake seam and what apps may read, so it is asked once here.
        let display = egui.filter(|_| !parsed_args.options.contains(NotedeckOptions::Headless));

        // Everything that finishes work off the render thread wakes the host
        // through this one handle: nostrdb's ingester callback and the relay
        // bridge below, and every app's background path via
        // `AppContext::waker`. With a window that is a repaint for eframe to
        // schedule; without one it is the run loop's `Notify`, signalled
        // directly — no egui pass in the path, which is why a headless run needs
        // no context at all.
        let (waker, headless_wake) = match display {
            Some(ctx) => (Waker::egui(ctx), None),
            None => {
                let wake = Arc::new(tokio::sync::Notify::new());
                let waker = {
                    let wake = wake.clone();
                    Waker::new(move || wake.notify_one())
                };
                (waker, Some(wake))
            }
        };

        let egui = display.cloned();

        let config = Config::new()
            .set_ingester_threads(2)
            .set_mapsize(map_size)
            .set_sub_callback({
                let waker = waker.clone();
                move |_| waker.wake()
            });

        let keystore = if parsed_args.options.contains(NotedeckOptions::Tests) {
            // tests never persist secrets
            None
        } else {
            let accounts = Directory::new(path.path(DataPathType::Keys));
            let selected = Directory::new(path.path(DataPathType::SelectedKey));
            Some(
                if parsed_args.options.contains(NotedeckOptions::UseKeystore) {
                    // opt-in: OS secure store (keychain)
                    AccountStorage::with_keystore(accounts, selected)
                } else {
                    // default: file-based storage (secret kept in the account file)
                    AccountStorage::new(accounts, selected)
                },
            )
        };

        let mut unknown_ids = UnknownIds::default();
        try_swap_pruned_db(&dbpath_str);
        let mut ndb = Ndb::new(&dbpath_str, &config).expect("ndb");
        let txn = Transaction::new(&ndb).expect("txn");
        let runtime_budget = if parsed_args.options.contains(NotedeckOptions::Tests) {
            RuntimeThreadBudget::for_test_runner()
        } else {
            RuntimeThreadBudget::from_available_parallelism()
        };
        let app_async_runtime = if parsed_args.options.contains(NotedeckOptions::Tests) {
            AppAsyncRuntime::new_owned(runtime_budget.main_async_threads())
        } else {
            AppAsyncRuntime::from_handle(tokio::runtime::Handle::current())
        };
        let job_pool = JobPool::with_app_async(
            runtime_budget.sync_job_threads(),
            app_async_runtime.spawner(),
        );
        let remote_waker = waker.clone();
        let mut bridge_config = crate::remote_data::RemoteBridgeConfig::default();
        if let Some(timeout) = remote_config.pong_timeout {
            bridge_config = bridge_config.with_pong_timeout(timeout);
        }
        let mut remote = RemoteState::new_with_config(
            &ndb,
            job_pool.spawner(),
            move || remote_waker.wake(),
            bridge_config,
        );
        remote
            .set_max_websocket_connections(settings.websocket_connection_limit().max_connections());

        // Tests must not reach the network: hand a fresh account an empty
        // bootstrap set so it connects to nothing (the outbox then has nothing
        // to flush on `AppContext` drop, so no Tokio runtime is required).
        let bootstrap_relays = if parsed_args.options.contains(NotedeckOptions::Tests) {
            Vec::new()
        } else {
            crate::account::relay::default_bootstrap_relays()
        };

        let mut accounts = Accounts::new(
            keystore,
            parsed_args.relays.clone(),
            bootstrap_relays,
            FALLBACK_PUBKEY(),
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );

        for key in &parsed_args.keys {
            info!("adding account: {}", &key.pubkey);
            if let Some(resp) = accounts.add_account(key.clone()) {
                resp.unk_id_action
                    .process_action(&mut unknown_ids, &ndb, &txn);
            }
        }

        /* add keys to nostrdb ingest threads for giftwrap processing */
        for account in accounts.cache.accounts() {
            if let Some(seckey) = &account.key.secret_key {
                ndb.add_key(&seckey.secret_bytes());
            }
        }

        if let Some(first) = parsed_args.keys.first() {
            accounts.select_account_for_startup(&first.pubkey, &mut ndb, &txn);
        }

        {
            // Seed bridge account context before app construction can queue account-bound remote work.
            let mut remote_api = remote.api();
            remote_api.on_selected_account_changed(&accounts);
            remote_api.flush();
        }

        let img_cache = Images::new(img_cache_dir);
        let note_cache = NoteCache::default();

        let app_size = AppSizeHandler::new(&path);

        // migrate
        if let Err(e) = img_cache.migrate_v0() {
            error!("error migrating image cache: {e}");
        }

        let global_wallet = GlobalWallet::new(&path);
        let zaps = Zaps::default();

        // Initialize localization
        let mut i18n = Localization::new();

        let setting_locale: Result<LanguageIdentifier, LanguageIdentifierError> =
            settings.locale().parse();

        if let Ok(setting_locale) = setting_locale {
            if let Err(err) = i18n.set_locale(setting_locale) {
                error!("{err}");
            }
        }

        if let Some(locale) = &parsed_args.locale {
            if let Err(err) = i18n.set_locale(locale.to_owned()) {
                error!("{err}");
            }
        }

        let (send_new_jobs, receive_new_jobs) = std::sync::mpsc::channel();
        let media_job_cache = JobCache::new(receive_new_jobs, send_new_jobs);

        // Opening the default output device is not free of side effects: on
        // Windows it initialises WASAPI/COM on a thread rodio owns. A test binary
        // builds many `Notedeck`s in parallel on a runner that has no audio
        // device at all, and that faults the process outright
        // (STATUS_ACCESS_VIOLATION) — measured: with `sound` linked, `cargo test
        // -p notedeck --lib` dies; with it off, the same 321 tests pass. Tests
        // never play anything, so give them a manager that never opens a device,
        // the same way the private relay pool is left inert just below.
        let sound = if parsed_args.options.contains(NotedeckOptions::Tests) {
            crate::SoundManager::silent()
        } else {
            let s = settings.get_settings_mut();
            crate::SoundManager::new(s.sounds_enabled, s.sound_volume)
        };

        // Tests run no Tokio runtime and must not open a private relay pool; leave
        // the host inert there (mirrors the `local_relay` gating below).
        let host_private_sync = if parsed_args.options.contains(NotedeckOptions::Tests) {
            None
        } else {
            Some(crate::HostPrivateSync::new())
        };

        // Embedded localhost relay for dogfooding tooling. On by default; tests
        // never start it (no Tokio runtime, and it must not open a port).
        let local_relay = if parsed_args.options.contains(NotedeckOptions::Tests) {
            None
        } else {
            parsed_args
                .local_relay
                .as_ref()
                .and_then(|addr| match addr.parse() {
                    Ok(socket_addr) => nostrdb_net::relay::server::spawn(ndb.clone(), socket_addr)
                        .map_err(|err| error!("failed to start local relay on {addr}: {err}"))
                        .ok(),
                    Err(err) => {
                        error!("invalid relay bind address '{addr}': {err}");
                        None
                    }
                })
        };

        Self {
            ndb,
            img_cache,
            unknown_ids,
            remote,
            _app_async_runtime: app_async_runtime,
            note_cache,
            accounts,
            global_wallet,
            path: path.clone(),
            args: parsed_args,
            settings,
            app: None,
            app_size,
            unrecognized_args,
            frame_history: FrameHistory::default(),
            clipboard: Clipboard::new(None),
            zaps,
            zap_verifier: ZapVerifier::new(),
            job_pool,
            media_jobs: media_job_cache,
            nip05_cache: Nip05Cache::new(),
            i18n,
            sound,
            host_private_sync,
            private_channels: crate::PrivateChannels::default(),
            registries: crate::AppRegistries::default(),
            app_actions: AppActionQueue::default(),
            navigator: crate::Navigator::default(),
            local_relay,
            egui,
            waker,
            pass_nr: 0,
            headless_wake,
            #[cfg(target_os = "android")]
            android_app: None,
        }
    }

    /// Setup egui context
    pub fn setup(&self, ctx: &egui::Context) {
        // Initialize global i18n context
        //crate::i18n::init_global_i18n(i18n.clone());
        crate::setup::setup_egui_context(ctx, self.args.options, self.theme(), self.zoom_factor());
    }

    #[inline]
    pub fn options(&self) -> NotedeckOptions {
        self.args.options
    }

    pub fn has_option(&self, option: NotedeckOptions) -> bool {
        self.options().contains(option)
    }

    pub fn app<A: App + 'static>(mut self, app: A) -> Self {
        self.set_app(app);
        self
    }

    pub fn app_context(&mut self) -> AppContext<'_> {
        self.notedeck_ref().app_ctx
    }

    pub fn notedeck_ref<'a>(&'a mut self) -> NotedeckRef<'a> {
        let remote = self.remote.api();
        // No host (tests) or nothing declared ⇒ nothing to reconcile ⇒ settled, so
        // an app that gates on this is never blocked by a sync that isn't running.
        let private_sync_settled = self
            .host_private_sync
            .as_ref()
            .is_none_or(crate::HostPrivateSync::settled);
        NotedeckRef {
            app_ctx: AppContext {
                ndb: &mut self.ndb,
                img_cache: &mut self.img_cache,
                unknown_ids: &mut self.unknown_ids,
                remote,
                note_cache: &mut self.note_cache,
                accounts: &mut self.accounts,
                private_sync_settled,
                global_wallet: &mut self.global_wallet,
                path: &self.path,
                args: &self.args,
                settings: &mut self.settings,
                clipboard: &mut self.clipboard,
                zaps: &mut self.zaps,
                zap_verifier: &mut self.zap_verifier,
                frame_history: &mut self.frame_history,
                job_pool: &mut self.job_pool,
                media_jobs: &mut self.media_jobs,
                nip05_cache: &mut self.nip05_cache,
                i18n: &mut self.i18n,
                sound: &self.sound,
                registries: &self.registries,
                app_actions: &mut self.app_actions,
                navigator: &mut self.navigator,
                private_channels: &mut self.private_channels,
                waker: &self.waker,
                egui: self.egui.as_ref(),
                #[cfg(target_os = "android")]
                android: self.android_app.as_ref().unwrap().clone(),
            },
            internals: NotedeckInternals {
                unrecognized_args: &self.unrecognized_args,
            },
        }
    }

    pub fn set_app<T: App + 'static>(&mut self, app: T) {
        self.app = Some(Rc::new(RefCell::new(app)));
    }

    /// Register a renderer for nostr events of one or more kinds, so surfaces
    /// like the notebook can draw referenced entities inline. Call at startup.
    pub fn register_kind_renderer(&mut self, renderer: Box<dyn crate::KindRenderer>) {
        self.registries.kind_renderers.register(renderer);
    }

    /// Register a parser for a reference scheme, so text surfaces can resolve
    /// `scheme:token` references the app owns. Call at startup (see
    /// [`App::reference_parsers`]).
    pub fn register_reference_parser(&mut self, parser: Box<dyn crate::ReferenceParser>) {
        self.registries.reference_parsers.register(parser);
    }

    /// Register an app-contributed agent tool, so AI backends can advertise and
    /// dispatch it. Call at startup (see [`App::tools`]).
    pub fn register_tool(&mut self, tool: crate::RegisteredTool) {
        self.registries.tools.register(tool);
    }

    pub fn args(&self) -> &Args {
        &self.args
    }

    pub fn theme(&self) -> ThemePreference {
        self.settings.theme()
    }

    pub fn zoom_factor(&self) -> f32 {
        self.settings.zoom_factor()
    }

    pub fn unrecognized_args(&self) -> &BTreeSet<String> {
        &self.unrecognized_args
    }
}

/// Installs the default TLS crypto provider for rustls.
///
/// This function selects the crypto provider based on the target platform:
/// - **Windows**: Uses `ring` because `aws-lc-rs` requires cmake and NASM,
///   which adds significant friction for Windows developers.
/// - **Other platforms**: Uses `aws-lc-rs` for optimal performance.
///
/// Must be called once at application startup before any TLS operations.
pub fn install_crypto() {
    // On Windows, use ring (fewer build requirements than aws-lc-rs which needs cmake/NASM)
    #[cfg(windows)]
    {
        let provider = rustls::crypto::ring::default_provider();
        let _ = provider.install_default();
    }

    // On non-Windows platforms, use aws-lc-rs for optimal performance
    #[cfg(not(windows))]
    {
        let provider = rustls::crypto::aws_lc_rs::default_provider();
        let _ = provider.install_default();
    }
}

pub struct NotedeckRef<'a> {
    pub app_ctx: AppContext<'a>,
    pub internals: NotedeckInternals<'a>,
}

pub struct NotedeckInternals<'a> {
    pub unrecognized_args: &'a BTreeSet<String>,
}

impl<'a> NotedeckInternals<'a> {
    /// ensure we recognized all the arguments
    pub fn check_args(&self, other_app_args: &BTreeSet<String>) -> Result<(), Error> {
        let completely_unrecognized: Vec<String> = self
            .unrecognized_args
            .intersection(other_app_args)
            .cloned()
            .collect();
        if !completely_unrecognized.is_empty() {
            let err = format!("Unrecognized arguments: {completely_unrecognized:?}");
            tracing::error!("{}", &err);
            return Err(Error::Generic(err));
        }

        Ok(())
    }
}

/// If a pruned database exists at `{dbpath}/compact/`, swap it into place
/// before opening ndb. This replaces the main data.mdb with the pruned one.
///
/// The staging directory is still literally `compact/` — see
/// [`Args::db_prune_path`], which is where the settings job writes it.
fn try_swap_pruned_db(dbpath: &str) {
    let dbpath = Path::new(dbpath);
    let staged_path = dbpath.join("compact");
    let staged_data = staged_path.join("data.mdb");

    info!(
        "prune swap: checking for pruned db at '{}'",
        staged_data.display()
    );

    if !staged_data.exists() {
        info!("prune swap: no pruned db found, skipping");
        return;
    }

    let staged_size = std::fs::metadata(&staged_data)
        .map(|m| m.len())
        .unwrap_or(0);
    info!("prune swap: found pruned db ({staged_size} bytes)");

    let db_data = dbpath.join("data.mdb");
    let db_old = dbpath.join("data.mdb.old");

    let old_size = std::fs::metadata(&db_data).map(|m| m.len()).unwrap_or(0);
    info!(
        "prune swap: current db at '{}' ({old_size} bytes)",
        db_data.display()
    );

    if let Err(e) = std::fs::rename(&db_data, &db_old) {
        error!("prune swap: failed to rename old db: {e}");
        return;
    }

    if let Err(e) = std::fs::rename(&staged_data, &db_data) {
        error!("prune swap: failed to move pruned db: {e}");
        // Try to restore the original
        let _ = std::fs::rename(&db_old, &db_data);
        return;
    }

    let _ = std::fs::remove_file(&db_old);
    let _ = std::fs::remove_dir_all(&staged_path);
    info!("prune swap: success! {old_size} -> {staged_size} bytes");
}

#[cfg(test)]
mod render_nav_tests {
    use super::*;
    use std::cell::RefCell;

    /// A single-view test app that does **not** override
    /// [`App::render_nav`], so calling `render_nav` exercises the trait default.
    /// `render` records that it ran by flipping `rendered`, letting the test
    /// assert the default `render_nav` delegated to it.
    #[derive(Default)]
    struct FlagApp {
        rendered: bool,
    }

    impl App for FlagApp {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            self.rendered = true;
            AppResponse::none()
        }
    }

    /// The default `render_nav` ignores its token and falls back to
    /// [`App::render`], so a single-view app needs no token handling.
    #[tokio::test]
    async fn default_render_nav_falls_back_to_render() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let ui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(
            &ui_ctx,
            tmp.path(),
            &["notedeck".to_owned(), "--testrunner".to_owned()],
        );

        // `__run_test_ui` wants an `Fn` closure, so reach the `&mut self`
        // render path through `RefCell`s rather than mutable captures.
        let app = RefCell::new(FlagApp::default());
        let app_ctx = RefCell::new(notedeck.app_context());

        // An arbitrary token the app never inspects; the default `render_nav`
        // discards it and renders the whole app.
        let token: Rc<dyn Any> = Rc::new(());

        egui::__run_test_ui(|ui| {
            let mut ctx = app_ctx.borrow_mut();
            let _ = app.borrow_mut().render_nav(&mut ctx, ui, &token);
        });

        assert!(
            app.borrow().rendered,
            "default render_nav must delegate to render()"
        );
    }

    /// The default `cleanup_nav` ignores its token and does nothing, so an app
    /// whose routes own no external resources (or that never pushes a route)
    /// needs no cleanup — a popped `()` app-switch entry, or any token it
    /// doesn't recognize, is a safe no-op that never touches `render`.
    #[tokio::test]
    async fn default_cleanup_nav_is_a_noop() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let ui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(
            &ui_ctx,
            tmp.path(),
            &["notedeck".to_owned(), "--testrunner".to_owned()],
        );

        let mut app = FlagApp::default();
        let mut ctx = notedeck.app_context();
        let token: Rc<dyn Any> = Rc::new(());

        app.cleanup_nav(&mut ctx, &token);

        assert!(
            !app.rendered,
            "default cleanup_nav must not render or otherwise touch the app"
        );
    }
}

#[cfg(test)]
mod prune_swap_tests {
    use super::*;
    use crate::test_util::test_config;
    use nostrdb::Filter;

    /// Our own pubkey — the account whose notes the keep-policy preserves.
    const OWN_PUBKEY: &str = "32bf915904bfde2d136ba45dde32c88f4aca863783999faea2e847a8fafd2f15";
    /// A stranger's pubkey; nothing they authored survives the prune.
    const OTHER_PUBKEY: &str = "e586b8d54cfecacf251c71d0b2d9b01673c8870fb3fe82a20ce5afc44ce7fccc";
    /// The author of the kind-0 profile below, kept because *all* profiles are.
    const PROFILE_PUBKEY: &str = "3f770d65d3a764a9c5cb503ae123e62ec7598ad035d836e2a810f3877a745b24";

    /// Our own kind-1 note.
    const OWN_NOTE_ID: &str = "702555e52e82cc24ad517ba78c21879f6e47a7c0692b9b20df147916ae8731a3";
    const OWN_NOTE: &str = r#"["EVENT","b",{"id": "702555e52e82cc24ad517ba78c21879f6e47a7c0692b9b20df147916ae8731a3","pubkey": "32bf915904bfde2d136ba45dde32c88f4aca863783999faea2e847a8fafd2f15","created_at": 1702675561,"kind": 1,"tags": [],"content": "hello, world","sig": "2275c5f5417abfd644b7bc74f0388d70feb5d08b6f90fa18655dda5c95d013bfbc5258ea77c05b7e40e0ee51d8a2efa931dc7a0ec1db4c0a94519762c6625675"}]"#;
    /// The stranger's kind-1 note.
    const OTHER_NOTE: &str = r#"["EVENT","b",{"id":"2e577580420c4ef02e8067aa842dd068be7c957f81a32b325fa1849b1650d98b","pubkey":"e586b8d54cfecacf251c71d0b2d9b01673c8870fb3fe82a20ce5afc44ce7fccc","created_at":1768414963,"kind":1,"tags":[],"content":"hi","sig":"662d45856ffc66c32df33ce5e8b7b9de14981774679b36bdb787bb8feda22b47eee7257756b915f7d54a53317151b0907a40847c635c9626debfb2a7b038c76f"}]"#;
    /// A kind-0 profile authored by neither account.
    const PROFILE_NOTE: &str = r#"["EVENT","b",{  "id": "0b9f0e14727733e430dcb00c69b12a76a1e100f419ce369df837f7eb33e4523c",  "pubkey": "3f770d65d3a764a9c5cb503ae123e62ec7598ad035d836e2a810f3877a745b24",  "created_at": 1736785355,  "kind": 0,  "tags": [    [      "alt",      "User profile for Derek Ross"    ],    [      "i",      "twitter:derekmross",      "1634343988407726081"    ],    [      "i",      "github:derekross",      "3edaf845975fa4500496a15039323fa3I"    ]  ],  "content": "{\"about\":\"Building NostrPlebs.com and NostrNests.com. The purple pill helps the orange pill go down. Nostr is the social glue that binds all of your apps together.\",\"banner\":\"https://i.nostr.build/O2JE.jpg\",\"display_name\":\"Derek Ross\",\"lud16\":\"derekross@strike.me\",\"name\":\"Derek Ross\",\"nip05\":\"derekross@nostrplebs.com\",\"picture\":\"https://i.nostr.build/MVIJ6OOFSUzzjVEc.jpg\",\"website\":\"https://nostrplebs.com\",\"created_at\":1707238393}",  "sig": "51e1225ccaf9b6739861dc218ac29045b09d5cf3a51b0ac6ea64bd36827d2d4394244e5f58a4e4a324c84eeda060e1a27e267e0d536e5a0e45b0b6bdc2c43bbc"}]"#;

    fn pubkey_bytes(hex_str: &str) -> [u8; 32] {
        hex::decode(hex_str)
            .expect("valid hex")
            .try_into()
            .expect("32 bytes")
    }

    /// The database directory notedeck would use, plus the prune output path
    /// the settings UI derives from it.
    struct TestPaths {
        db: std::path::PathBuf,
        staged: std::path::PathBuf,
    }

    /// Derive both paths the way the app does, so the test breaks if the
    /// settings UI's [`Args::db_prune_path`] and the `compact/` directory
    /// [`try_swap_pruned_db`] looks in ever drift apart.
    fn test_paths(base: &Path) -> TestPaths {
        let (args, _unrecognized) = Args::parse(&[]);
        let data_path = DataPath::new(base);

        let db = args.db_path(&data_path);
        let staged = args.db_prune_path(&data_path);
        std::fs::create_dir_all(&db).expect("create db dir");

        TestPaths { db, staged }
    }

    /// Walk the whole user-visible prune flow: the settings button prunes into
    /// `{db}/compact/`, and the next launch swaps that in via
    /// [`try_swap_pruned_db`]. Ingest our own note, a stranger's note and a
    /// third party's profile; prune under the default keep-policy; swap; then
    /// reopen the swapped-in database and confirm it is a valid nostrdb that
    /// still holds what the policy keeps and nothing it doesn't.
    ///
    /// Note that pruning rewrites notes through the writer, so `NoteKey`s in
    /// the swapped-in database are freshly assigned and relay provenance is not
    /// carried over — the assertions below deliberately go by note id.
    #[tokio::test]
    async fn prune_then_swap_keeps_own_notes_and_profiles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(tmp.path());
        let db_str = paths.db.to_str().expect("utf8 db path").to_string();
        let staged_str = paths.staged.to_str().expect("utf8 staged path").to_string();

        let own_pubkey = pubkey_bytes(OWN_PUBKEY);
        let other_pubkey = pubkey_bytes(OTHER_PUBKEY);
        let profile_pubkey = pubkey_bytes(PROFILE_PUBKEY);

        // Populate a database and prune it, exactly as the settings job does.
        {
            let ndb = Ndb::new(&db_str, &test_config()).expect("open db");

            let filters = vec![Filter::new().kinds(vec![0, 1]).build()];
            let sub = ndb.subscribe(&filters).expect("subscribe");
            let waiter = ndb.wait_for_all_notes(sub, 3);

            ndb.process_event(OWN_NOTE).expect("ingest own note");
            ndb.process_event(OTHER_NOTE).expect("ingest other note");
            ndb.process_event(PROFILE_NOTE).expect("ingest profile");
            waiter.await.expect("all three ingested");

            {
                let txn = Transaction::new(&ndb).expect("txn");
                let all = ndb.query(&txn, &filters, 10).expect("query all");
                assert_eq!(all.len(), 3, "source db should hold all three notes");
            }

            let keep = Ndb::prune_default_filters(&[own_pubkey]).expect("default filters");
            ndb.prune(&staged_str, &keep).expect("prune");
        }

        assert!(
            paths.staged.join("data.mdb").exists(),
            "prune should have written a database into the staging dir"
        );

        // Next launch: swap the pruned database into place.
        try_swap_pruned_db(&db_str);

        assert!(
            !paths.staged.exists(),
            "a successful swap consumes the staging dir"
        );
        assert!(
            !paths.db.join("data.mdb.old").exists(),
            "a successful swap removes the backup of the old db"
        );

        // The swapped-in database must reopen and still hold the kept notes.
        let ndb = Ndb::new(&db_str, &test_config()).expect("reopen swapped db");

        // Scoped, so the read transaction and everything borrowing from it are
        // gone before the write below. nostrdb transactions are meant to be
        // short-lived, and holding a reader open across a write that grows the
        // map lets the writer remap underneath it — the results here point into
        // the old mapping, so reading them afterwards is a use-after-free.
        {
            let txn = Transaction::new(&ndb).expect("txn");

            let own = ndb
                .query(
                    &txn,
                    &[Filter::new()
                        .authors(vec![&own_pubkey])
                        .kinds(vec![1])
                        .build()],
                    10,
                )
                .expect("query own notes");
            assert_eq!(own.len(), 1, "our own note should survive the prune");
            assert_eq!(hex::encode(own[0].note.id()), OWN_NOTE_ID);

            let other = ndb
                .query(
                    &txn,
                    &[Filter::new()
                        .authors(vec![&other_pubkey])
                        .kinds(vec![1])
                        .build()],
                    10,
                )
                .expect("query other notes");
            assert!(
                other.is_empty(),
                "a stranger's note is outside the keep-policy"
            );

            let profiles = ndb
                .query(
                    &txn,
                    &[Filter::new()
                        .authors(vec![&profile_pubkey])
                        .kinds(vec![0])
                        .build()],
                    10,
                )
                .expect("query profiles");
            assert_eq!(profiles.len(), 1, "every profile is kept");
        }

        // The user keeps using this database after the swap, so it has to
        // accept writes too — not just answer queries.
        let sub = ndb
            .subscribe(&[Filter::new()
                .authors(vec![&other_pubkey])
                .kinds(vec![1])
                .build()])
            .expect("subscribe");
        let waiter = ndb.wait_for_notes(sub, 1);
        ndb.process_event(OTHER_NOTE)
            .expect("ingest into swapped db");
        // A rejected write would leave the waiter pending forever, so time out
        // rather than hang the test suite.
        tokio::time::timeout(Duration::from_secs(10), waiter)
            .await
            .expect("swapped db ingested a new note within 10s")
            .expect("ingest notified the subscription");
    }

    /// With no pruned database staged, startup must leave the live one alone.
    #[test]
    fn swap_without_a_pruned_db_leaves_the_live_db_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(tmp.path());
        let db_data = paths.db.join("data.mdb");
        std::fs::write(&db_data, b"live db").expect("write live db");

        try_swap_pruned_db(paths.db.to_str().expect("utf8 db path"));

        assert_eq!(
            std::fs::read(&db_data).expect("live db still readable"),
            b"live db",
            "swap must not touch the live db when nothing is staged"
        );
    }
}

#[cfg(test)]
mod tick_headless_tests {
    use super::*;
    use std::cell::Cell;

    /// An app that counts how often its background `update` and its egui
    /// `render` ran. The counters are shared `Rc<Cell<..>>`s so the test keeps
    /// handles to them after `set_app`/`Rc` ownership moves the app away.
    struct CountingApp {
        updates: Rc<Cell<u32>>,
        renders: Rc<Cell<u32>>,
    }

    impl App for CountingApp {
        fn update(&mut self, _ctx: &mut AppContext<'_>) {
            self.updates.set(self.updates.get() + 1);
        }

        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            self.renders.set(self.renders.get() + 1);
            AppResponse::none()
        }
    }

    /// An app that records whether the host handed it a display this tick.
    struct DisplayProbe {
        saw_egui: Rc<Cell<Option<bool>>>,
    }

    impl App for DisplayProbe {
        fn update(&mut self, ctx: &mut AppContext<'_>) {
            self.saw_egui.set(Some(ctx.egui.is_some()));
        }

        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }
    }

    fn test_notedeck(tmp: &tempfile::TempDir) -> Notedeck {
        let ui_ctx = egui::Context::default();
        Notedeck::init(
            &ui_ctx,
            tmp.path(),
            &["notedeck".to_owned(), "--testrunner".to_owned()],
        )
    }

    /// Booted the way `--headless` boots: no `egui::Context` anywhere.
    fn headless_notedeck(tmp: &tempfile::TempDir) -> Notedeck {
        Notedeck::init_headless(
            tmp.path(),
            &[
                "notedeck".to_owned(),
                "--testrunner".to_owned(),
                "--headless".to_owned(),
            ],
        )
    }

    /// Has a wake already landed on `notify`? `notify_one` stores its permit
    /// synchronously, so one poll settles it: a wake that happened is ready on
    /// the first poll, and a wake that never happened stays pending. No timer,
    /// and no way for the test to hang waiting on a wake that isn't coming.
    fn woke(notify: &tokio::sync::Notify) -> bool {
        use std::future::Future as _;
        let mut notified = Box::pin(notify.notified());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        notified.as_mut().poll(&mut cx).is_ready()
    }

    /// The headless per-frame tick must drive the installed app's background
    /// `update` but never its egui `render` — that's the whole point of the
    /// mode. Driven through the real `tick_headless` entry point, which is safe
    /// to call without a live egui pass precisely because it skips the render.
    #[tokio::test]
    async fn headless_tick_runs_update_not_render() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = test_notedeck(&tmp);

        let updates = Rc::new(Cell::new(0));
        let renders = Rc::new(Cell::new(0));
        notedeck.set_app(CountingApp {
            updates: updates.clone(),
            renders: renders.clone(),
        });

        notedeck.tick_headless();

        assert_eq!(updates.get(), 1, "headless tick must run the app's update");
        assert_eq!(renders.get(), 0, "headless tick must not render the app");
    }

    /// `render_notedeck` always runs the app's background `update`, and gates
    /// only the display pass on `headless`: skipped when headless, run
    /// (incrementing the render count) when not. The non-headless call needs a
    /// live pass because `main_panel().show()` does, so it runs inside
    /// `__run_test_ui`.
    #[tokio::test]
    async fn render_notedeck_gates_only_the_display_pass() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = test_notedeck(&tmp);

        let updates = Rc::new(Cell::new(0));
        let renders = Rc::new(Cell::new(0));
        let app: Rc<RefCell<dyn App + 'static>> = Rc::new(RefCell::new(CountingApp {
            updates: updates.clone(),
            renders: renders.clone(),
        }));

        // Headless: update runs, render is skipped — no egui pass required.
        {
            let mut app_ctx = notedeck.app_context();
            render_notedeck(app.clone(), &mut app_ctx, None);
        }
        assert_eq!(updates.get(), 1, "headless render_notedeck must update");
        assert_eq!(renders.get(), 0, "headless render_notedeck must not render");

        // Non-headless: both update and render run. `__run_test_ui` gives a live
        // pass for `main_panel().show()`; the `Fn` closure reaches the borrows
        // through `RefCell`s.
        let app_ctx = RefCell::new(notedeck.app_context());
        let app_cell = RefCell::new(app.clone());
        egui::__run_test_ui(|ui| {
            let mut ctx = app_ctx.borrow_mut();
            render_notedeck(app_cell.borrow().clone(), &mut ctx, Some(ui.ctx()));
        });
        assert_eq!(updates.get(), 2, "gui render_notedeck must also update");
        assert_eq!(renders.get(), 1, "gui render_notedeck must render");
    }

    /// Everything that produces work headless has to keep reaching the run
    /// loop, tick after tick — this is the whole wake seam, and the repeat is
    /// the point. It used to run through `request_repaint` and a callback egui
    /// re-armed only on `begin_pass`, which is why `tick_headless` opened an
    /// empty pass; the waker signals the loop's `Notify` itself, so there is no
    /// latch to re-arm and no pass to open.
    #[tokio::test]
    async fn headless_wakes_keep_reaching_the_loop() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = headless_notedeck(&tmp);
        let waker = notedeck.app_context().waker.clone();
        let wake = notedeck
            .headless_waker()
            .expect("a headless boot installs a waker");

        for round in 1..=3 {
            notedeck.tick_headless();
            // Stand in for a dave backend thread streaming a token, or a
            // budgeted loop yielding with work still to do.
            waker.wake();
            assert!(
                woke(&wake),
                "round {round}: a wake must reach the headless loop"
            );
        }
    }

    /// A GUI run must not get the headless waker: there, another pass means a
    /// repaint, and a run loop nobody is running would swallow it.
    #[tokio::test]
    async fn gui_boot_installs_no_headless_waker() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let notedeck = test_notedeck(&tmp);
        assert!(
            notedeck.headless_waker().is_none(),
            "a GUI boot must wake through egui, not a run-loop signal"
        );
    }

    /// `--headless` in the args means headless even when a caller hands a
    /// context in, so a GUI entry point can't accidentally boot a half-headless
    /// host: one that signals a run loop nobody runs, or draws into a window
    /// that isn't there.
    #[tokio::test]
    async fn headless_args_win_over_a_context_handed_in() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let ui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(
            &ui_ctx,
            tmp.path(),
            &[
                "notedeck".to_owned(),
                "--testrunner".to_owned(),
                "--headless".to_owned(),
            ],
        );

        assert!(
            notedeck.headless_waker().is_some(),
            "the args asked for headless, so wakes must go to the run loop"
        );
        assert!(
            notedeck.app_context().egui.is_none(),
            "the args asked for headless, so apps must see no display"
        );
    }

    /// The host's pass counter is the clock `tick_core` ages texture-cache
    /// entries against, so a headless tick has to advance it rather than hand
    /// every cache read the same frozen zero. (Nothing is evicted headless
    /// today — the cache stays empty because texture allocation only ever
    /// happens through a context captured at render — so this pins the clock,
    /// not a reclaim.)
    ///
    #[tokio::test]
    async fn headless_tick_advances_the_pass_counter() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = headless_notedeck(&tmp);

        let before = notedeck.pass_nr();
        notedeck.tick_headless();
        assert!(
            notedeck.pass_nr() > before,
            "a headless tick must advance the host's pass counter"
        );
    }

    /// An app's `ctx.wake()` must reach the headless run loop, and reach it
    /// *directly*: the waker signals the loop's `Notify` itself rather than
    /// going through `request_repaint` and the callback latch that made
    /// headway:notedeck/ginger-twice-gate's stall possible. So it works with no
    /// egui pass anywhere in sight, and it works twice.
    #[tokio::test]
    async fn an_apps_wake_reaches_the_headless_run_loop() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = headless_notedeck(&tmp);
        let wake = notedeck.headless_waker().expect("booted --headless");

        assert!(!woke(&wake), "nothing has asked for a pass yet");

        for round in 1..=2 {
            notedeck.app_context().wake();
            assert!(
                woke(&wake),
                "round {round}: an app's wake must reach the run loop"
            );
        }
    }

    /// A GUI boot's waker belongs to egui, so waking must request a repaint —
    /// the thing eframe schedules the next frame off. Observed by installing our
    /// own repaint callback, which only a GUI boot leaves free (headless takes
    /// it; see `gui_boot_installs_no_headless_waker`).
    #[tokio::test]
    async fn a_gui_wake_requests_a_repaint() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let ui_ctx = egui::Context::default();
        let mut notedeck = Notedeck::init(
            &ui_ctx,
            tmp.path(),
            &["notedeck".to_owned(), "--testrunner".to_owned()],
        );

        let repaints = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = repaints.clone();
        ui_ctx.set_request_repaint_callback(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });

        notedeck.app_context().wake();
        assert_eq!(
            repaints.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "a GUI wake must ask egui for another frame"
        );
    }

    /// `AppContext::egui` is the whole of what an app can still learn about the
    /// display, so what a `--headless` tick puts there is the contract: `None`,
    /// not a windowless context standing in for one. An app that guards its
    /// display-coupled work on this (columns' input handler, dave's focus
    /// steal, headway/notebook's animation repaints) is only correct if
    /// headless actually says so.
    #[tokio::test]
    async fn a_headless_tick_hands_the_app_no_display() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = headless_notedeck(&tmp);

        let saw_egui = Rc::new(Cell::new(None));
        notedeck.set_app(DisplayProbe {
            saw_egui: saw_egui.clone(),
        });

        notedeck.tick_headless();
        assert_eq!(
            saw_egui.get(),
            Some(false),
            "a headless app must be told there is no display, not handed a \
             windowless context"
        );
    }

    /// And the GUI tick does hand one over, or every app that guards on it would
    /// quietly stop reading input.
    #[tokio::test]
    async fn a_gui_tick_hands_the_app_its_display() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = test_notedeck(&tmp);

        let saw_egui = Rc::new(Cell::new(None));
        notedeck.set_app(DisplayProbe {
            saw_egui: saw_egui.clone(),
        });

        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| notedeck.tick(ctx));
        assert_eq!(
            saw_egui.get(),
            Some(true),
            "a GUI app must get its host's egui context"
        );
    }

    /// Advancing the counter is only half of it: the render path records cache
    /// reads against whatever pass the caches were last told, so a tick that
    /// bumps `pass_nr` without publishing it would leave reads stamped with a
    /// stale pass and a sweep measuring their age from a newer one — cold-
    /// looking textures that are in fact on screen.
    #[tokio::test]
    async fn a_tick_publishes_its_pass_to_the_texture_caches() {
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let mut notedeck = headless_notedeck(&tmp);

        for _ in 0..3 {
            notedeck.tick_headless();
            assert_eq!(
                notedeck.img_cache.textures.current_pass(),
                notedeck.pass_nr(),
                "the caches must age entries against the host's current pass"
            );
        }
    }
}
