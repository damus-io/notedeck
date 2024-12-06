use crate::{
    accounts::{Accounts, AccountsRoute},
    app_creation::setup_cc,
    app_size_handler::AppSizeHandler,
    args::Args,
    column::Columns,
    decks::{Decks, DecksCache},
    draft::Drafts,
    filter::FilterState,
    frame_history::FrameHistory,
    imgcache::ImageCache,
    nav,
    notecache::NoteCache,
    notes_holder::NotesHolderStorage,
    profile::Profile,
    storage::{self, DataPath, DataPathType, Directory, FileKeyStorage, KeyStorageType},
    subscriptions::{SubKind, Subscriptions},
    support::Support,
    thread::Thread,
    timeline::{self, Timeline},
    ui::{self, DesktopSidePanel},
    unknowns::UnknownIds,
    view_state::ViewState,
    Result,
};

use enostr::{ClientMessage, Keypair, Pubkey, RelayEvent, RelayMessage, RelayPool};
use uuid::Uuid;

use egui::{Context, Frame, Style};
use egui_extras::{Size, StripBuilder};

use nostrdb::{Config, Ndb, Transaction};

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tracing::{error, info, trace, warn};

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum DamusState {
    Initializing,
    Initialized,
}

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
pub struct Damus {
    state: DamusState,
    pub note_cache: NoteCache,
    pub pool: RelayPool,

    pub decks_cache: DecksCache,
    pub ndb: Ndb,
    pub view_state: ViewState,
    pub unknown_ids: UnknownIds,
    pub drafts: Drafts,
    pub threads: NotesHolderStorage<Thread>,
    pub profiles: NotesHolderStorage<Profile>,
    pub img_cache: ImageCache,
    pub accounts: Accounts,
    pub subscriptions: Subscriptions,
    pub app_rect_handler: AppSizeHandler,
    pub support: Support,

    frame_history: crate::frame_history::FrameHistory,

    pub path: DataPath,
    // TODO: make these bitflags
    pub debug: bool,
    pub since_optimize: bool,
    pub textmode: bool,
}

fn handle_key_events(input: &egui::InputState, _pixels_per_point: f32, columns: &mut Columns) {
    for event in &input.raw.events {
        if let egui::Event::Key {
            key, pressed: true, ..
        } = event
        {
            match key {
                egui::Key::J => {
                    columns.select_down();
                }
                egui::Key::K => {
                    columns.select_up();
                }
                egui::Key::H => {
                    columns.select_left();
                }
                egui::Key::L => {
                    columns.select_left();
                }
                _ => {}
            }
        }
    }
}

fn try_process_event(damus: &mut Damus, ctx: &egui::Context) -> Result<()> {
    let ppp = ctx.pixels_per_point();
    let current_columns = get_active_columns_mut(&damus.accounts, &mut damus.decks_cache);
    ctx.input(|i| handle_key_events(i, ppp, current_columns));

    let ctx2 = ctx.clone();
    let wakeup = move || {
        ctx2.request_repaint();
    };
    damus.pool.keepalive_ping(wakeup);

    // NOTE: we don't use the while let loop due to borrow issues
    #[allow(clippy::while_let_loop)]
    loop {
        let ev = if let Some(ev) = damus.pool.try_recv() {
            ev.into_owned()
        } else {
            break;
        };

        match (&ev.event).into() {
            RelayEvent::Opened => {
                damus
                    .accounts
                    .send_initial_filters(&mut damus.pool, &ev.relay);

                timeline::send_initial_timeline_filters(
                    &damus.ndb,
                    damus.since_optimize,
                    get_active_columns_mut(&damus.accounts, &mut damus.decks_cache),
                    &mut damus.subscriptions,
                    &mut damus.pool,
                    &ev.relay,
                );
            }
            // TODO: handle reconnects
            RelayEvent::Closed => warn!("{} connection closed", &ev.relay),
            RelayEvent::Error(e) => error!("{}: {}", &ev.relay, e),
            RelayEvent::Other(msg) => trace!("other event {:?}", &msg),
            RelayEvent::Message(msg) => process_message(damus, &ev.relay, &msg),
        }
    }

    let current_columns = get_active_columns_mut(&damus.accounts, &mut damus.decks_cache);
    let n_timelines = current_columns.timelines().len();
    for timeline_ind in 0..n_timelines {
        let is_ready = {
            let timeline = &mut current_columns.timelines[timeline_ind];
            timeline::is_timeline_ready(
                &damus.ndb,
                &mut damus.pool,
                &mut damus.note_cache,
                timeline,
                &damus.accounts.mutefun(),
            )
        };

        if is_ready {
            let txn = Transaction::new(&damus.ndb).expect("txn");

            if let Err(err) = Timeline::poll_notes_into_view(
                timeline_ind,
                current_columns.timelines_mut(),
                &damus.ndb,
                &txn,
                &mut damus.unknown_ids,
                &mut damus.note_cache,
                &damus.accounts.mutefun(),
            ) {
                error!("poll_notes_into_view: {err}");
            }
        } else {
            // TODO: show loading?
        }
    }

    if damus.unknown_ids.ready_to_send() {
        unknown_id_send(damus);
    }

    Ok(())
}

fn unknown_id_send(damus: &mut Damus) {
    let filter = damus.unknown_ids.filter().expect("filter");
    info!(
        "Getting {} unknown ids from relays",
        damus.unknown_ids.ids().len()
    );
    let msg = ClientMessage::req("unknownids".to_string(), filter);
    damus.unknown_ids.clear();
    damus.pool.send(&msg);
}

#[cfg(feature = "profiling")]
fn setup_profiling() {
    puffin::set_scopes_on(true); // tell puffin to collect data
}

fn update_damus(damus: &mut Damus, ctx: &egui::Context) {
    damus.accounts.update(&damus.ndb, &mut damus.pool, ctx); // update user relay and mute lists

    match damus.state {
        DamusState::Initializing => {
            #[cfg(feature = "profiling")]
            setup_profiling();

            damus.state = DamusState::Initialized;
            // this lets our eose handler know to close unknownids right away
            damus
                .subscriptions()
                .insert("unknownids".to_string(), SubKind::OneShot);
            if let Err(err) = timeline::setup_initial_nostrdb_subs(
                &damus.ndb,
                &mut damus.note_cache,
                &mut damus.decks_cache,
                &damus.accounts.mutefun(),
            ) {
                warn!("update_damus init: {err}");
            }
        }

        DamusState::Initialized => (),
    };

    if let Err(err) = try_process_event(damus, ctx) {
        error!("error processing event: {}", err);
    }

    damus.app_rect_handler.try_save_app_size(ctx);
}

fn process_event(damus: &mut Damus, _subid: &str, event: &str) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //info!("processing event {}", event);
    if let Err(_err) = damus.ndb.process_event(event) {
        error!("error processing event {}", event);
    }
}

fn handle_eose(damus: &mut Damus, subid: &str, relay_url: &str) -> Result<()> {
    let sub_kind = if let Some(sub_kind) = damus.subscriptions().get(subid) {
        sub_kind
    } else {
        let n_subids = damus.subscriptions().len();
        warn!(
            "got unknown eose subid {}, {} tracked subscriptions",
            subid, n_subids
        );
        return Ok(());
    };

    match *sub_kind {
        SubKind::Timeline(_) => {
            // eose on timeline? whatevs
        }
        SubKind::Initial => {
            let txn = Transaction::new(&damus.ndb)?;
            UnknownIds::update(
                &txn,
                &mut damus.unknown_ids,
                get_active_columns(&damus.accounts, &damus.decks_cache),
                &damus.ndb,
                &mut damus.note_cache,
            );
            // this is possible if this is the first time
            if damus.unknown_ids.ready_to_send() {
                unknown_id_send(damus);
            }
        }

        // oneshot subs just close when they're done
        SubKind::OneShot => {
            let msg = ClientMessage::close(subid.to_string());
            damus.pool.send_to(&msg, relay_url);
        }

        SubKind::FetchingContactList(timeline_uid) => {
            let timeline = if let Some(tl) =
                get_active_columns_mut(&damus.accounts, &mut damus.decks_cache)
                    .find_timeline_mut(timeline_uid)
            {
                tl
            } else {
                error!(
                    "timeline uid:{} not found for FetchingContactList",
                    timeline_uid
                );
                return Ok(());
            };

            let filter_state = timeline.filter.get(relay_url);

            // If this request was fetching a contact list, our filter
            // state should be "FetchingRemote". We look at the local
            // subscription for that filter state and get the subscription id
            let local_sub = if let FilterState::FetchingRemote(unisub) = filter_state {
                unisub.local
            } else {
                // TODO: we could have multiple contact list results, we need
                // to check to see if this one is newer and use that instead
                warn!(
                    "Expected timeline to have FetchingRemote state but was {:?}",
                    timeline.filter
                );
                return Ok(());
            };

            info!(
                "got contact list from {}, updating filter_state to got_remote",
                relay_url
            );

            // We take the subscription id and pass it to the new state of
            // "GotRemote". This will let future frames know that it can try
            // to look for the contact list in nostrdb.
            timeline
                .filter
                .set_relay_state(relay_url.to_string(), FilterState::got_remote(local_sub));
        }
    }

    Ok(())
}

fn process_message(damus: &mut Damus, relay: &str, msg: &RelayMessage) {
    match msg {
        RelayMessage::Event(subid, ev) => process_event(damus, subid, ev),
        RelayMessage::Notice(msg) => warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(sid) => {
            if let Err(err) = handle_eose(damus, sid, relay) {
                error!("error handling eose: {}", err);
            }
        }
    }
}

fn render_damus(damus: &mut Damus, ctx: &Context) {
    if ui::is_narrow(ctx) {
        render_damus_mobile(ctx, damus);
    } else {
        render_damus_desktop(ctx, damus);
    }

    ctx.request_repaint_after(Duration::from_secs(1));

    #[cfg(feature = "profiling")]
    puffin_egui::profiler_window(ctx);
}

/*
fn determine_key_storage_type() -> KeyStorageType {
    #[cfg(target_os = "macos")]
    {
        KeyStorageType::MacOS
    }

    #[cfg(target_os = "linux")]
    {
        KeyStorageType::Linux
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        KeyStorageType::None
    }
}
*/

impl Damus {
    /// Called once before the first frame.
    pub fn new<P: AsRef<Path>>(ctx: &egui::Context, data_path: P, args: Vec<String>) -> Self {
        // arg parsing
        let parsed_args = Args::parse(&args);
        let is_mobile = parsed_args.is_mobile.unwrap_or(ui::is_compiled_as_mobile());

        setup_cc(ctx, is_mobile, parsed_args.light);

        let data_path = parsed_args
            .datapath
            .unwrap_or(data_path.as_ref().to_str().expect("db path ok").to_string());
        let path = DataPath::new(&data_path);
        let dbpath_str = parsed_args
            .dbpath
            .unwrap_or_else(|| path.path(DataPathType::Db).to_str().unwrap().to_string());

        let _ = std::fs::create_dir_all(&dbpath_str);

        let imgcache_dir = path.path(DataPathType::Cache).join(ImageCache::rel_dir());
        let _ = std::fs::create_dir_all(imgcache_dir.clone());

        let mapsize = if cfg!(target_os = "windows") {
            // 16 Gib on windows because it actually creates the file
            1024usize * 1024usize * 1024usize * 16usize
        } else {
            // 1 TiB for everything else since its just virtually mapped
            1024usize * 1024usize * 1024usize * 1024usize
        };

        let config = Config::new().set_ingester_threads(4).set_mapsize(mapsize);

        let keystore = if parsed_args.use_keystore {
            let keys_path = path.path(DataPathType::Keys);
            let selected_key_path = path.path(DataPathType::SelectedKey);
            KeyStorageType::FileSystem(FileKeyStorage::new(
                Directory::new(keys_path),
                Directory::new(selected_key_path),
            ))
        } else {
            KeyStorageType::None
        };

        let mut accounts = Accounts::new(keystore, parsed_args.relays);

        let num_keys = parsed_args.keys.len();

        let mut unknown_ids = UnknownIds::default();
        let ndb = Ndb::new(&dbpath_str, &config).expect("ndb");

        {
            let txn = Transaction::new(&ndb).expect("txn");
            for key in parsed_args.keys {
                info!("adding account: {}", key.pubkey);
                accounts
                    .add_account(key)
                    .process_action(&mut unknown_ids, &ndb, &txn);
            }
        }

        if num_keys != 0 {
            accounts.select_account(0);
        }

        // AccountManager will setup the pool on first update
        let pool = RelayPool::new();

        let account = accounts
            .get_selected_account()
            .as_ref()
            .map(|a| a.pubkey.bytes());

        let columns = if parsed_args.columns.is_empty() {
            if let Some(serializable_columns) = storage::load_columns(&path) {
                info!("Using columns from disk");
                serializable_columns.into_columns(&ndb, account)
            } else {
                info!("Could not load columns from disk");
                Columns::new()
            }
        } else {
            let mut columns: Columns = Columns::new();
            for col in parsed_args.columns {
                if let Some(timeline) = col.into_timeline(&ndb, account) {
                    columns.add_new_timeline_column(timeline);
                }
            }

            columns
        };

        let mut decks_cache = {
            let mut decks_cache = DecksCache::default();

            let mut decks = Decks::default();
            *decks.active_mut().columns_mut() = columns;

            if let Some(acc) = account {
                decks_cache.add_decks(Pubkey::new(*acc), decks);
            }

            decks_cache
        };

        let debug = parsed_args.debug;

        if get_active_columns(&accounts, &decks_cache).columns().is_empty() {
            if accounts.get_accounts().is_empty() {
                set_demo(&path, &ndb, &mut accounts, &mut decks_cache, &mut unknown_ids);
            } else {
                get_active_columns_mut(&accounts, &mut decks_cache).new_column_picker();
            }
        }

        let app_rect_handler = AppSizeHandler::new(&path);
        let support = Support::new(&path);

        Self {
            pool,
            debug,
            unknown_ids,
            subscriptions: Subscriptions::default(),
            since_optimize: parsed_args.since_optimize,
            threads: NotesHolderStorage::default(),
            profiles: NotesHolderStorage::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            img_cache: ImageCache::new(imgcache_dir),
            note_cache: NoteCache::default(),
            textmode: parsed_args.textmode,
            ndb,
            accounts,
            frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            path,
            app_rect_handler,
            support,
            decks_cache,
        }
    }

    pub fn pool_mut(&mut self) -> &mut RelayPool {
        &mut self.pool
    }

    pub fn ndb(&self) -> &Ndb {
        &self.ndb
    }

    pub fn drafts_mut(&mut self) -> &mut Drafts {
        &mut self.drafts
    }

    pub fn img_cache_mut(&mut self) -> &mut ImageCache {
        &mut self.img_cache
    }

    pub fn accounts(&self) -> &Accounts {
        &self.accounts
    }

    pub fn accounts_mut(&mut self) -> &mut Accounts {
        &mut self.accounts
    }

    pub fn view_state_mut(&mut self) -> &mut ViewState {
        &mut self.view_state
    }

    pub fn columns_mut(&mut self) -> &mut Columns {
        get_active_columns_mut(&self.accounts, &mut self.decks_cache)
    }

    pub fn columns(&self) -> &Columns {
        get_active_columns(&self.accounts, &self.decks_cache)
    }

    pub fn gen_subid(&self, kind: &SubKind) -> String {
        if self.debug {
            format!("{:?}", kind)
        } else {
            Uuid::new_v4().to_string()
        }
    }

    pub fn mock<P: AsRef<Path>>(data_path: P) -> Self {
        let decks_cache = DecksCache::default();

        let path = DataPath::new(&data_path);
        let imgcache_dir = path.path(DataPathType::Cache).join(ImageCache::rel_dir());
        let _ = std::fs::create_dir_all(imgcache_dir.clone());
        let debug = true;

        let app_rect_handler = AppSizeHandler::new(&path);
        let support = Support::new(&path);

        let config = Config::new().set_ingester_threads(2);

        Self {
            debug,
            unknown_ids: UnknownIds::default(),
            subscriptions: Subscriptions::default(),
            since_optimize: true,
            threads: NotesHolderStorage::default(),
            profiles: NotesHolderStorage::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            pool: RelayPool::new(),
            img_cache: ImageCache::new(imgcache_dir),
            note_cache: NoteCache::default(),
            textmode: false,
            ndb: Ndb::new(
                path.path(DataPathType::Db)
                    .to_str()
                    .expect("db path should be ok"),
                &config,
            )
            .expect("ndb"),
            accounts: Accounts::new(KeyStorageType::None, vec![]),
            frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            path,
            app_rect_handler,
            support,
            decks_cache,
        }
    }

    pub fn subscriptions(&mut self) -> &mut HashMap<String, SubKind> {
        &mut self.subscriptions.subs
    }

    pub fn note_cache_mut(&mut self) -> &mut NoteCache {
        &mut self.note_cache
    }

    pub fn unknown_ids_mut(&mut self) -> &mut UnknownIds {
        &mut self.unknown_ids
    }

    pub fn threads(&self) -> &NotesHolderStorage<Thread> {
        &self.threads
    }

    pub fn threads_mut(&mut self) -> &mut NotesHolderStorage<Thread> {
        &mut self.threads
    }

    pub fn note_cache(&self) -> &NoteCache {
        &self.note_cache
    }
}

/*
fn circle_icon(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let stroke = ui.style().interact(&response).fg_stroke;
    let radius = egui::lerp(2.0..=3.0, openness);
    ui.painter()
        .circle_filled(response.rect.center(), radius, stroke.color);
}
*/

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //let routes = app.timelines[0].routes.clone();

    main_panel(&ctx.style(), ui::is_narrow(ctx)).show(ctx, |ui| {
        if !app.columns().columns().is_empty()
            && nav::render_nav(0, app, ui).process_render_nav_response(app)
        {
            storage::save_columns(&app.path, app.columns().as_serializable_columns());
        }
    });
}

fn main_panel(style: &Style, narrow: bool) -> egui::CentralPanel {
    let inner_margin = egui::Margin {
        top: if narrow { 50.0 } else { 0.0 },
        left: 0.0,
        right: 0.0,
        bottom: 0.0,
    };
    egui::CentralPanel::default().frame(Frame {
        inner_margin,
        fill: style.visuals.panel_fill,
        ..Default::default()
    })
}

fn render_damus_desktop(ctx: &egui::Context, app: &mut Damus) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let screen_size = ctx.screen_rect().width();
    let calc_panel_width = (screen_size
        / get_active_columns(&app.accounts, &app.decks_cache).num_columns() as f32)
        - 30.0;
    let min_width = 320.0;
    let need_scroll = calc_panel_width < min_width;
    let panel_sizes = if need_scroll {
        Size::exact(min_width)
    } else {
        Size::remainder()
    };

    main_panel(&ctx.style(), ui::is_narrow(ctx)).show(ctx, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if need_scroll {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                timelines_view(ui, panel_sizes, app);
            });
        } else {
            timelines_view(ui, panel_sizes, app);
        }
    });
}

fn timelines_view(ui: &mut egui::Ui, sizes: Size, app: &mut Damus) {
    StripBuilder::new(ui)
        .size(Size::exact(ui::side_panel::SIDE_PANEL_WIDTH))
        .sizes(
            sizes,
            get_active_columns(&app.accounts, &app.decks_cache).num_columns(),
        )
        .clip(true)
        .horizontal(|mut strip| {
            let mut side_panel_action: Option<nav::SwitchingAction> = None;
            strip.cell(|ui| {
                let rect = ui.available_rect_before_wrap();
                let side_panel = DesktopSidePanel::new(
                    &app.ndb,
                    &mut app.img_cache,
                    app.accounts.get_selected_account(),
                    &app.decks_cache,
                )
                .show(ui);

                if side_panel.response.clicked() || side_panel.response.secondary_clicked() {
                    if let Some(action) = DesktopSidePanel::perform_action(
                        &mut app.decks_cache,
                        &app.accounts,
                        &mut app.support,
                        side_panel.action,
                    ) {
                        side_panel_action = Some(action);
                    }
                }

                // vertical sidebar line
                ui.painter().vline(
                    rect.right(),
                    rect.y_range(),
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
            });

            let num_cols = app.columns().num_columns();
            let mut responses = Vec::with_capacity(num_cols);
            for col_index in 0..num_cols {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    responses.push(nav::render_nav(col_index, app, ui));

                    // vertical line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                //strip.cell(|ui| timeline::timeline_view(ui, app, timeline_ind));
            }

            let mut save_cols = false;
            if let Some(action) = side_panel_action {
                save_cols = save_cols || action.process(app);
            }

            for response in responses {
                let save = response.process_render_nav_response(app);
                save_cols = save_cols || save;
            }

            if save_cols {
                storage::save_columns(&app.path, app.columns().as_serializable_columns());
            }
        });
}

impl eframe::App for Damus {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        //eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    /// Put your widgets into a `SidePanel`, `TopPanel`, `CentralPanel`, `Window` or `Area`.
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);

        #[cfg(feature = "profiling")]
        puffin::GlobalProfiler::lock().new_frame();
        update_damus(self, ctx);
        render_damus(self, ctx);
    }
}

pub fn get_active_columns<'a>(accounts: &Accounts, decks_cache: &'a DecksCache) -> &'a Columns {
    get_decks(accounts, decks_cache).active().columns()
}

pub fn get_decks<'a>(accounts: &Accounts, decks_cache: &'a DecksCache) -> &'a Decks {
    let key = if let Some(acc) = accounts.get_selected_account() {
        &acc.pubkey
    } else {
        &decks_cache.fallback_pubkey
    };
    decks_cache.decks(key)
}

pub fn get_active_columns_mut<'a>(
    accounts: &Accounts,
    decks_cache: &'a mut DecksCache,
) -> &'a mut Columns {
    get_decks_mut(accounts, decks_cache)
        .active_mut()
        .columns_mut()
}

pub fn get_decks_mut<'a>(accounts: &Accounts, decks_cache: &'a mut DecksCache) -> &'a mut Decks {
    if let Some(acc) = accounts.get_selected_account() {
        decks_cache.decks_mut(&acc.pubkey)
    } else {
        decks_cache.fallback_mut()
    }
}

pub fn set_demo(
    data_path: &DataPath,
    ndb: &Ndb,
    accounts: &mut Accounts,
    decks_cache: &mut DecksCache,
    unk_ids: &mut UnknownIds,
) {
    let columns = get_active_columns_mut(accounts, decks_cache);
    let demo_pubkey =
        Pubkey::from_hex("aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe")
            .unwrap();
    {
        let txn = Transaction::new(ndb).expect("txn");
        accounts
            .add_account(Keypair::only_pubkey(demo_pubkey))
            .process_action(unk_ids, ndb, &txn);
        accounts.select_account(0);
    }

    columns.add_column(crate::column::Column::new(vec![
        crate::route::Route::AddColumn(ui::add_column::AddColumnRoute::Base),
        crate::route::Route::Accounts(AccountsRoute::Accounts),
    ]));

    if let Some(timeline) =
        timeline::TimelineKind::contact_list(timeline::PubkeySource::Explicit(demo_pubkey))
            .into_timeline(ndb, Some(demo_pubkey.bytes()))
    {
        columns.add_new_timeline_column(timeline);
    }

    columns.add_new_timeline_column(Timeline::hashtag("introductions".to_string()));

    storage::save_columns(data_path, columns.as_serializable_columns());
}
