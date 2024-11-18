use crate::{
    account_manager::AccountManager,
    app_creation::setup_cc,
    app_size_handler::AppSizeHandler,
    app_style::user_requested_visuals_change,
    args::Args,
    column::Columns,
    decks::{AccountId, Decks, DecksCache},
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
    timeline::{self, is_timeline_ready, Timeline},
    ui::{self, DesktopSidePanel},
    unknowns::UnknownIds,
    view_state::ViewState,
    Result,
};

use enostr::{ClientMessage, Pubkey, RelayEvent, RelayMessage, RelayPool};
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
    pub accounts: AccountManager,
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

fn relay_setup(pool: &mut RelayPool, ctx: &egui::Context) {
    let ctx = ctx.clone();
    let wakeup = move || {
        ctx.request_repaint();
    };
    if let Err(e) = pool.add_url("ws://localhost:8080".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://relay.damus.io".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    //if let Err(e) = pool.add_url("wss://pyramid.fiatjaf.com".to_string(), wakeup.clone()) {
    //error!("{:?}", e)
    //}
    if let Err(e) = pool.add_url("wss://nos.lol".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://nostr.wine".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://purplepag.es".to_string(), wakeup) {
        error!("{:?}", e)
    }
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
            is_timeline_ready(&damus.ndb, &mut damus.pool, &mut damus.note_cache, timeline)
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
                get_active_columns_mut(&damus.accounts, &mut damus.decks_cache),
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

        let mut config = Config::new();
        config.set_ingester_threads(4);

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

        let mut accounts = AccountManager::new(keystore);

        let num_keys = parsed_args.keys.len();

        for key in parsed_args.keys {
            info!("adding account: {}", key.pubkey);
            accounts.add_account(key);
        }

        if num_keys != 0 {
            accounts.select_account(0);
        }

        // setup relays if we have them
        let pool = if parsed_args.relays.is_empty() {
            let mut pool = RelayPool::new();
            relay_setup(&mut pool, ctx);
            pool
        } else {
            let wakeup = {
                let ctx = ctx.clone();
                move || {
                    ctx.request_repaint();
                }
            };

            let mut pool = RelayPool::new();
            for relay in parsed_args.relays {
                if let Err(e) = pool.add_url(relay.clone(), wakeup.clone()) {
                    error!("error adding relay {}: {}", relay, e);
                }
            }
            pool
        };

        let account = accounts
            .get_selected_account()
            .as_ref()
            .map(|a| a.pubkey.bytes());
        let ndb = Ndb::new(&dbpath_str, &config).expect("ndb");

        let mut columns = if parsed_args.columns.is_empty() {
            if let Some(serializable_columns) = storage::load_columns(&path) {
                info!("Using columns from disk");
                serializable_columns.into_columns(&ndb, account)
            } else {
                info!("Could not load columns from disk");
                Columns::new()
            }
        } else {
            info!(
                "Using columns from command line arguments: {:?}",
                parsed_args.columns
            );
            let mut columns: Columns = Columns::new();
            for col in parsed_args.columns {
                if let Some(timeline) = col.into_timeline(&ndb, account) {
                    columns.add_new_timeline_column(timeline);
                }
            }

            columns
        };

        let debug = parsed_args.debug;

        if columns.columns().is_empty() {
            columns.new_column_picker();
        }

        let app_rect_handler = AppSizeHandler::new(&path);
        let support = Support::new(&path);

        let mut decks_cache = DecksCache::default();

        if let Some(account) = account {
            let mut decks = Decks::default();
            *decks.active_mut().columns_mut() = columns;

            decks_cache.add_decks(AccountId::User(Pubkey::new(*account)), decks);
        }
        for cur_account in accounts.get_accounts() {
            if let Some(acc) = account {
                if *cur_account.pubkey.bytes() == *acc {
                    continue;
                }
            }

            decks_cache.add_deck_default(AccountId::User(cur_account.pubkey));
        }

        Self {
            pool,
            debug,
            unknown_ids: UnknownIds::default(),
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

    pub fn accounts(&self) -> &AccountManager {
        &self.accounts
    }

    pub fn accounts_mut(&mut self) -> &mut AccountManager {
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

        let mut config = Config::new();
        config.set_ingester_threads(2);

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
            accounts: AccountManager::new(KeyStorageType::None),
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

fn top_panel(ctx: &egui::Context) -> egui::TopBottomPanel {
    let top_margin = egui::Margin {
        top: 4.0,
        left: 8.0,
        right: 8.0,
        ..Default::default()
    };

    let frame = Frame {
        inner_margin: top_margin,
        fill: ctx.style().visuals.panel_fill,
        ..Default::default()
    };

    egui::TopBottomPanel::top("top_panel")
        .frame(frame)
        .show_separator_line(false)
}

fn render_panel(ctx: &egui::Context, app: &mut Damus) {
    top_panel(ctx).show(ctx, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            ui.visuals_mut().button_frame = false;

            if let Some(new_visuals) =
                user_requested_visuals_change(ui::is_oled(), ctx.style().visuals.dark_mode, ui)
            {
                ctx.set_visuals(new_visuals)
            }

            if ui
                .add(egui::Button::new("A").frame(false))
                .on_hover_text("Text mode")
                .clicked()
            {
                app.textmode = !app.textmode;
            }

            /*
            if ui
                .add(egui::Button::new("+").frame(false))
                .on_hover_text("Add Timeline")
                .clicked()
            {
                app.n_panels += 1;
            }

            if app.n_panels != 1
                && ui
                    .add(egui::Button::new("-").frame(false))
                    .on_hover_text("Remove Timeline")
                    .clicked()
            {
                app.n_panels -= 1;
            }
            */

            //#[cfg(feature = "profiling")]
            {
                ui.weak(format!(
                    "FPS: {:.2}, {:10.1}ms",
                    app.frame_history.fps(),
                    app.frame_history.mean_frame_time() * 1e3
                ));

                /*
                if !app.timelines().count().is_empty() {
                    ui.weak(format!(
                        "{} notes",
                        &app.timelines()
                            .notes(ViewFilter::NotesAndReplies)
                            .len()
                    ));
                }
                */
            }
        });
    });
}

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    //render_panel(ctx, app, 0);

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //let routes = app.timelines[0].routes.clone();

    main_panel(&ctx.style(), ui::is_narrow(ctx)).show(ctx, |ui| {
        if !get_active_columns(&app.accounts, &app.decks_cache)
            .columns()
            .is_empty()
        {
            if let Some(r) = nav::render_nav(0, app, ui) {
                r.process_nav_response(&app.path, &mut app.accounts, &mut app.decks_cache)
            }
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
    render_panel(ctx, app);
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
            let mut selection_resp = None;
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
                    if let Some(resp) = DesktopSidePanel::perform_action(
                        &mut app.decks_cache,
                        &app.accounts,
                        &mut app.support,
                        side_panel.action,
                    ) {
                        info!("Got selection response from side panel: {:?}", resp);
                        selection_resp = Some(resp);
                    }
                }

                // vertical sidebar line
                ui.painter().vline(
                    rect.right(),
                    rect.y_range(),
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
            });

            let mut nav_resp: Option<nav::RenderNavResponse> = None;
            for col_index in 0..get_active_columns(&app.accounts, &app.decks_cache).num_columns() {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    if let Some(r) = nav::render_nav(col_index, app, ui) {
                        nav_resp = Some(r);
                    }

                    // vertical line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                //strip.cell(|ui| timeline::timeline_view(ui, app, timeline_ind));
            }

            if let Some(r) = nav_resp {
                r.process_nav_response(&app.path, &mut app.accounts, &mut app.decks_cache);
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

pub fn get_active_columns<'a>(
    accounts: &AccountManager,
    decks_cache: &'a DecksCache,
) -> &'a Columns {
    get_decks(accounts, decks_cache).active().columns()
}

pub fn get_decks<'a>(accounts: &AccountManager, decks_cache: &'a DecksCache) -> &'a Decks {
    if let Some(acc) = accounts.get_selected_account() {
        decks_cache.decks(&AccountId::User(acc.pubkey))
    } else {
        decks_cache.decks(&AccountId::Unnamed(0))
    }
}

pub fn get_active_columns_mut<'a>(
    accounts: &AccountManager,
    decks_cache: &'a mut DecksCache,
) -> &'a mut Columns {
    get_decks_mut(accounts, decks_cache)
        .active_mut()
        .columns_mut()
}

pub fn get_decks_mut<'a>(
    accounts: &AccountManager,
    decks_cache: &'a mut DecksCache,
) -> &'a mut Decks {
    if let Some(acc) = accounts.get_selected_account() {
        decks_cache.decks_mut(&AccountId::User(acc.pubkey))
    } else {
        decks_cache.decks_mut(&AccountId::Unnamed(0))
    }
}
