use crate::account::FALLBACK_PUBKEY;
use crate::i18n::Localization;
use crate::nip05::Nip05Cache;
use crate::persist::{AppSizeHandler, SettingsHandler};
use crate::remote_data::RemoteState;
use crate::wallet::GlobalWallet;
use crate::zaps::{ZapVerifier, Zaps};
use crate::NotedeckOptions;
use crate::{
    frame_history::FrameHistory, AccountStorage, Accounts, AppContext, Args, DataPath,
    DataPathType, Directory, Images, NoteAction, NoteCache, UnknownIds,
};
use crate::{Error, JobCache};
use crate::{JobPool, MediaJobs};
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

pub trait App {
    /// Background processing — called every frame for ALL apps.
    fn update(&mut self, _ctx: &mut AppContext<'_>, _egui_ctx: &egui::Context) {}

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
    #[cfg(feature = "local-relay")]
    #[allow(dead_code)]
    local_relay: Option<nostrdb_net::relay::server::RelayHandle>,

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

#[profiling::function]
fn render_notedeck(
    app: Rc<RefCell<dyn App + 'static>>,
    app_ctx: &mut AppContext,
    ctx: &egui::Context,
) {
    app.borrow_mut().update(app_ctx, ctx);
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
        {
            profiling::scope!("media jobs");
            self.media_jobs.run_received(&mut self.job_pool, |id| {
                crate::run_media_job_pre_action(id, &mut self.img_cache.textures);
            });
            self.media_jobs.deliver_all_completed(|completed| {
                crate::deliver_completed_media_job(completed, &mut self.img_cache.textures)
            });
        }

        {
            self.remote.service_relays(&mut self.job_pool);
            self.remote.process_events(ctx, &self.ndb);
        }
        self.pump_host_private_sync();
        self.nip05_cache.poll();
        self.zap_verifier.poll(&self.ndb, self.zaps.pay_cache());
        let Some(app) = &self.app else {
            self.remote
                .request_repaint_for_next_full_history_deadline(ctx);
            return;
        };
        let app = app.clone();

        // Let the shell refresh the agent-tool registry from its running apps
        // before we hand a borrow of it to this frame's AppContext.
        if let Some(tools) = app.borrow_mut().take_tool_update() {
            self.registries.tools.reset(tools);
        }

        let mut app_ref = self.notedeck_ref(ctx);

        {
            let app_ctx = &mut app_ref.app_ctx;
            app_ctx.accounts.update(app_ctx.ndb, &mut app_ctx.remote);
            app_ctx
                .zaps
                .process(app_ctx.accounts, app_ctx.global_wallet, app_ctx.ndb);
            if app_ctx.unknown_ids.ready_to_send() {
                let mut oneshot = app_ctx.remote.oneshot(app_ctx.accounts);
                crate::unknown_id_send(app_ctx.unknown_ids, &mut oneshot);
            }
        }

        render_notedeck(app, &mut app_ref.app_ctx, ctx);

        {
            profiling::scope!("outbox ingestion");
            drop(app_ref);
        }
        self.remote
            .request_repaint_for_next_full_history_deadline(ctx);

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

    pub fn set_pong_timeout(&mut self, timeout: Duration) {
        self.remote.set_pong_timeout(timeout);
    }

    #[cfg(target_os = "android")]
    pub fn set_android_context(&mut self, context: AndroidApp) {
        self.android_app = Some(context);
    }

    pub fn init<P: AsRef<Path>>(ctx: &egui::Context, data_path: P, args: &[String]) -> Self {
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

        let config = Config::new()
            .set_ingester_threads(2)
            .set_mapsize(map_size)
            .set_sub_callback({
                let ctx = ctx.clone();
                move |_| ctx.request_repaint()
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
        try_swap_compacted_db(&dbpath_str);
        let mut ndb = Ndb::new(&dbpath_str, &config).expect("ndb");
        let txn = Transaction::new(&ndb).expect("txn");
        let job_pool = JobPool::default();
        let remote = RemoteState::new(&ndb, job_pool.spawner());

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

        let sound = {
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
        #[cfg(feature = "local-relay")]
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
            #[cfg(feature = "local-relay")]
            local_relay,
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

    pub fn app_context(&mut self, ui_ctx: &egui::Context) -> AppContext<'_> {
        self.notedeck_ref(ui_ctx).app_ctx
    }

    pub fn notedeck_ref<'a>(&'a mut self, ui_ctx: &egui::Context) -> NotedeckRef<'a> {
        let remote = self.remote.api(ui_ctx);
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

/// If a compacted database exists at `{dbpath}/compact/`, swap it into place
/// before opening ndb. This replaces the main data.mdb with the compacted one.
fn try_swap_compacted_db(dbpath: &str) {
    let dbpath = Path::new(dbpath);
    let compact_path = dbpath.join("compact");
    let compact_data = compact_path.join("data.mdb");

    info!(
        "compact swap: checking for compacted db at '{}'",
        compact_data.display()
    );

    if !compact_data.exists() {
        info!("compact swap: no compacted db found, skipping");
        return;
    }

    let compact_size = std::fs::metadata(&compact_data)
        .map(|m| m.len())
        .unwrap_or(0);
    info!("compact swap: found compacted db ({compact_size} bytes)");

    let db_data = dbpath.join("data.mdb");
    let db_old = dbpath.join("data.mdb.old");

    let old_size = std::fs::metadata(&db_data).map(|m| m.len()).unwrap_or(0);
    info!(
        "compact swap: current db at '{}' ({old_size} bytes)",
        db_data.display()
    );

    if let Err(e) = std::fs::rename(&db_data, &db_old) {
        error!("compact swap: failed to rename old db: {e}");
        return;
    }

    if let Err(e) = std::fs::rename(&compact_data, &db_data) {
        error!("compact swap: failed to move compacted db: {e}");
        // Try to restore the original
        let _ = std::fs::rename(&db_old, &db_data);
        return;
    }

    let _ = std::fs::remove_file(&db_old);
    let _ = std::fs::remove_dir_all(&compact_path);
    info!("compact swap: success! {old_size} -> {compact_size} bytes");
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
        let app_ctx = RefCell::new(notedeck.app_context(&ui_ctx));

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
        let mut ctx = notedeck.app_context(&ui_ctx);
        let token: Rc<dyn Any> = Rc::new(());

        app.cleanup_nav(&mut ctx, &token);

        assert!(
            !app.rendered,
            "default cleanup_nav must not render or otherwise touch the app"
        );
    }
}
