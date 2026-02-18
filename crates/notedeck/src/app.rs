use crate::account::FALLBACK_PUBKEY;
use crate::i18n::Localization;
use crate::nip05::Nip05Cache;
use crate::persist::{AppSizeHandler, SettingsHandler};
use crate::unknowns::unknown_id_send;
use crate::wallet::GlobalWallet;
use crate::zaps::Zaps;
use crate::NotedeckOptions;
use crate::{
    frame_history::FrameHistory, AccountStorage, Accounts, AppContext, Args, DataPath,
    DataPathType, Directory, Images, NoteAction, NoteCache, RelayDebugView, UnknownIds,
};
use crate::{Error, JobCache};
use crate::{JobPool, MediaJobs};
use egui::Margin;
use egui::ThemePreference;
use egui_winit::clipboard::Clipboard;
use enostr::{PoolEventBuf, PoolRelay, RelayEvent, RelayMessage, RelayPool};
use nostrdb::{Config, Ndb, Transaction};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;
use tracing::{error, info};
use unic_langid::{LanguageIdentifier, LanguageIdentifierError};

#[cfg(target_os = "android")]
use android_activity::AndroidApp;

pub enum AppAction {
    Note(NoteAction),
    ToggleChrome,
}

pub trait App {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse;
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
    pool: RelayPool,
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
    frame_history: FrameHistory,
    job_pool: JobPool,
    media_jobs: MediaJobs,
    nip05_cache: Nip05Cache,
    i18n: Localization,

    /// Desktop notification manager (macOS/Linux only).
    /// Owns the notification service lifecycle.
    #[cfg(not(target_os = "android"))]
    notification_manager: Option<crate::notifications::NotificationManager>,

    #[cfg(target_os = "android")]
    android_app: Option<AndroidApp>,
}

/// Our chrome, which is basically nothing
fn main_panel(style: &egui::Style) -> egui::CentralPanel {
    egui::CentralPanel::default().frame(egui::Frame {
        inner_margin: Margin::ZERO,
        fill: style.visuals.panel_fill,
        ..Default::default()
    })
}

fn render_notedeck(notedeck: &mut Notedeck, ctx: &egui::Context) {
    main_panel(&ctx.style()).show(ctx, |ui| {
        // render app
        let Some(app) = &notedeck.app else {
            return;
        };

        let app = app.clone();
        app.borrow_mut().update(&mut notedeck.app_context(), ui);

        // Move the screen up when we have a virtual keyboard
        // NOTE: actually, we only want to do this if the keyboard is covering the focused element?
        /*
        let keyboard_height = crate::platform::virtual_keyboard_height() as f32;
        if keyboard_height > 0.0 {
            ui.ctx().transform_layer_shapes(
                ui.layer_id(),
                egui::emath::TSTransform::from_translation(egui::Vec2::new(0.0, -(keyboard_height/2.0))),
            );
        }
        */
    });
}

impl eframe::App for Notedeck {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        profiling::finish_frame!();
        self.frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);

        self.media_jobs.run_received(&mut self.job_pool, |id| {
            crate::run_media_job_pre_action(id, &mut self.img_cache.textures);
        });
        self.media_jobs.deliver_all_completed(|completed| {
            crate::deliver_completed_media_job(completed, &mut self.img_cache.textures)
        });

        self.nip05_cache.poll();

        // handle account updates
        self.accounts.update(&mut self.ndb, &mut self.pool, ctx);

        self.zaps
            .process(&mut self.accounts, &mut self.global_wallet, &self.ndb);

        render_notedeck(self, ctx);

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

        if self.args.options.contains(NotedeckOptions::RelayDebug) {
            if self.pool.debug.is_none() {
                self.pool.use_debug();
            }

            if let Some(debug) = &mut self.pool.debug {
                RelayDebugView::window(ctx, debug);
            }
        }

        #[cfg(feature = "puffin")]
        puffin_egui::profiler_window(ctx);
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
    #[cfg(target_os = "android")]
    pub fn set_android_context(&mut self, context: AndroidApp) {
        self.android_app = Some(context);
    }

    pub fn new<P: AsRef<Path>>(ctx: &egui::Context, data_path: P, args: &[String]) -> Self {
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
        let dbpath_str = parsed_args
            .dbpath
            .clone()
            .unwrap_or_else(|| path.path(DataPathType::Db).to_str().unwrap().to_string());

        let _ = std::fs::create_dir_all(&dbpath_str);

        let img_cache_dir = path.path(DataPathType::Cache);
        let _ = std::fs::create_dir_all(img_cache_dir.clone());

        let map_size = if cfg!(target_os = "windows") {
            // 16 Gib on windows because it actually creates the file
            1024usize * 1024usize * 1024usize * 16usize
        } else {
            // 1 TiB for everything else since its just virtually mapped
            1024usize * 1024usize * 1024usize * 1024usize
        };

        let settings = SettingsHandler::new(&path).load();

        let config = Config::new().set_ingester_threads(2).set_mapsize(map_size);

        let keystore = if parsed_args.options.contains(NotedeckOptions::UseKeystore) {
            let keys_path = path.path(DataPathType::Keys);
            let selected_key_path = path.path(DataPathType::SelectedKey);
            Some(AccountStorage::new(
                Directory::new(keys_path),
                Directory::new(selected_key_path),
            ))
        } else {
            None
        };

        // AccountManager will setup the pool on first update
        let mut pool = RelayPool::new();
        {
            let ctx = ctx.clone();
            if let Err(err) = pool.add_multicast_relay(move || ctx.request_repaint()) {
                error!("error setting up multicast relay: {err}");
            }
        }

        let mut unknown_ids = UnknownIds::default();
        let mut ndb = Ndb::new(&dbpath_str, &config).expect("ndb");
        let txn = Transaction::new(&ndb).expect("txn");

        let mut accounts = Accounts::new(
            keystore,
            parsed_args.relays.clone(),
            FALLBACK_PUBKEY(),
            &mut ndb,
            &txn,
            &mut pool,
            ctx,
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
            accounts.select_account(&first.pubkey, &mut ndb, &txn, &mut pool, ctx);
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
        let job_pool = JobPool::default();

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

        Self {
            ndb,
            img_cache,
            unknown_ids,
            pool,
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
            job_pool,
            media_jobs: media_job_cache,
            nip05_cache: Nip05Cache::new(),
            i18n,
            #[cfg(not(target_os = "android"))]
            notification_manager: None,
            #[cfg(target_os = "android")]
            android_app: None,
        }
    }

    /// Setup egui context
    pub fn setup(&self, ctx: &egui::Context) {
        // Initialize global i18n context
        //crate::i18n::init_global_i18n(i18n.clone());
        crate::setup::setup_egui_context(
            ctx,
            self.args.options,
            self.theme(),
            self.note_body_font_size(),
            self.zoom_factor(),
        );
    }

    /// ensure we recognized all the arguments
    pub fn check_args(&self, other_app_args: &BTreeSet<String>) -> Result<(), Error> {
        let completely_unrecognized: Vec<String> = self
            .unrecognized_args()
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
        AppContext {
            ndb: &mut self.ndb,
            img_cache: &mut self.img_cache,
            unknown_ids: &mut self.unknown_ids,
            pool: &mut self.pool,
            note_cache: &mut self.note_cache,
            accounts: &mut self.accounts,
            global_wallet: &mut self.global_wallet,
            path: &self.path,
            args: &self.args,
            settings: &mut self.settings,
            clipboard: &mut self.clipboard,
            zaps: &mut self.zaps,
            frame_history: &mut self.frame_history,
            job_pool: &mut self.job_pool,
            media_jobs: &mut self.media_jobs,
            nip05_cache: &mut self.nip05_cache,
            i18n: &mut self.i18n,
            #[cfg(not(target_os = "android"))]
            notification_manager: &mut self.notification_manager,
            #[cfg(target_os = "android")]
            android: self.android_app.as_ref().unwrap().clone(),
        }
    }

    pub fn set_app<T: App + 'static>(&mut self, app: T) {
        self.app = Some(Rc::new(RefCell::new(app)));
    }

    pub fn args(&self) -> &Args {
        &self.args
    }

    pub fn theme(&self) -> ThemePreference {
        self.settings.theme()
    }

    pub fn note_body_font_size(&self) -> f32 {
        self.settings.note_body_font_size()
    }

    pub fn zoom_factor(&self) -> f32 {
        self.settings.zoom_factor()
    }

    pub fn unrecognized_args(&self) -> &BTreeSet<String> {
        &self.unrecognized_args
    }
}

/// Install the rustls crypto provider for TLS support.
///
/// Uses the ring crypto backend. Logs an error if installation fails,
/// which can happen if a provider was already installed.
pub fn install_crypto() {
    let provider = rustls::crypto::ring::default_provider();
    if let Err(e) = provider.install_default() {
        tracing::error!("Failed to install rustls crypto provider: {:?}", e);
    }
}

#[profiling::function]
pub fn try_process_events_core(
    app_ctx: &mut AppContext<'_>,
    ctx: &egui::Context,
    mut receive: impl FnMut(&mut AppContext, PoolEventBuf),
) {
    let ctx2 = ctx.clone();
    let wakeup = move || {
        ctx2.request_repaint();
    };

    app_ctx.pool.keepalive_ping(wakeup);

    // NOTE: we don't use the while let loop due to borrow issues
    #[allow(clippy::while_let_loop)]
    loop {
        let ev = if let Some(ev) = app_ctx.pool.try_recv() {
            ev.into_owned()
        } else {
            break;
        };

        match (&ev.event).into() {
            RelayEvent::Opened => {
                tracing::trace!("Opened relay {}", ev.relay);
                app_ctx
                    .accounts
                    .send_initial_filters(app_ctx.pool, &ev.relay);
            }
            RelayEvent::Closed => tracing::warn!("{} connection closed", &ev.relay),
            RelayEvent::Other(msg) => {
                tracing::trace!("relay {} sent other event {:?}", ev.relay, &msg)
            }
            RelayEvent::Error(error) => error!("relay {} had error: {error:?}", &ev.relay),
            RelayEvent::Message(ref msg) => {
                process_message_core(app_ctx, &ev.relay, msg);

                // Forward notification-relevant relay events to the manager
                #[cfg(not(target_os = "android"))]
                if let RelayMessage::Event(_subid, relay_msg) = msg {
                    if let Some(mgr) = app_ctx.notification_manager.as_ref() {
                        mgr.process_relay_message(
                            relay_msg,
                            app_ctx.ndb,
                            &app_ctx.accounts,
                            app_ctx.i18n,
                        );
                    }
                }
            }
        }

        receive(app_ctx, ev);
    }

    if app_ctx.unknown_ids.ready_to_send() {
        unknown_id_send(app_ctx.unknown_ids, app_ctx.pool);
    }
}

#[profiling::function]
fn process_message_core(ctx: &mut AppContext<'_>, relay: &str, msg: &RelayMessage) {
    match msg {
        RelayMessage::Event(_subid, ev) => {
            let relay = if let Some(relay) = ctx.pool.relays.iter().find(|r| r.url() == relay) {
                relay
            } else {
                error!("couldn't find relay {} for note processing!?", relay);
                return;
            };

            match relay {
                PoolRelay::Websocket(_) => {
                    //info!("processing event {}", event);
                    tracing::trace!("processing event {ev}");
                    if let Err(err) = ctx.ndb.process_event_with(
                        ev,
                        nostrdb::IngestMetadata::new()
                            .client(false)
                            .relay(relay.url()),
                    ) {
                        error!("error processing event {ev}: {err}");
                    }
                }
                PoolRelay::Multicast(_) => {
                    // multicast events are client events
                    if let Err(err) = ctx.ndb.process_event_with(
                        ev,
                        nostrdb::IngestMetadata::new()
                            .client(true)
                            .relay(relay.url()),
                    ) {
                        error!("error processing multicast event {ev}: {err}");
                    }
                }
            }
        }
        RelayMessage::Notice(msg) => tracing::warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(id) => {
            tracing::trace!("Relay {} received eose: {id}", relay)
        }
    }
}
