use crate::{
    args::ColumnsArgs,
    column::Columns,
    decks::{Decks, DecksCache, FALLBACK_PUBKEY},
    draft::Drafts,
    nav,
    notes_holder::NotesHolderStorage,
    profile::Profile,
    storage,
    subscriptions::{SubKind, Subscriptions},
    support::Support,
    thread::Thread,
    timeline::{self, Timeline},
    ui::{self, DesktopSidePanel},
    unknowns,
    view_state::ViewState,
    Result,
};

use notedeck::{Accounts, AppContext, DataPath, DataPathType, FilterState, ImageCache, UnknownIds};

use enostr::{ClientMessage, Keypair, Pubkey, RelayEvent, RelayMessage, RelayPool};
use uuid::Uuid;

use egui::{Frame, Style};
use egui_extras::{Size, StripBuilder};

use nostrdb::{Ndb, Transaction};

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
    pub decks_cache: DecksCache,
    pub view_state: ViewState,
    pub drafts: Drafts,
    pub threads: NotesHolderStorage<Thread>,
    pub profiles: NotesHolderStorage<Profile>,
    pub subscriptions: Subscriptions,
    pub support: Support,

    //frame_history: crate::frame_history::FrameHistory,

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

fn try_process_event(
    damus: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ctx: &egui::Context,
) -> Result<()> {
    let ppp = ctx.pixels_per_point();
    let current_columns = get_active_columns_mut(app_ctx.accounts, &mut damus.decks_cache);
    ctx.input(|i| handle_key_events(i, ppp, current_columns));

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
                app_ctx
                    .accounts
                    .send_initial_filters(app_ctx.pool, &ev.relay);

                timeline::send_initial_timeline_filters(
                    app_ctx.ndb,
                    damus.since_optimize,
                    get_active_columns_mut(app_ctx.accounts, &mut damus.decks_cache),
                    &mut damus.subscriptions,
                    app_ctx.pool,
                    &ev.relay,
                );
            }
            // TODO: handle reconnects
            RelayEvent::Closed => warn!("{} connection closed", &ev.relay),
            RelayEvent::Error(e) => error!("{}: {}", &ev.relay, e),
            RelayEvent::Other(msg) => trace!("other event {:?}", &msg),
            RelayEvent::Message(msg) => process_message(damus, app_ctx, &ev.relay, &msg),
        }
    }

    let current_columns = get_active_columns_mut(app_ctx.accounts, &mut damus.decks_cache);
    let n_timelines = current_columns.timelines().len();
    for timeline_ind in 0..n_timelines {
        let is_ready = {
            let timeline = &mut current_columns.timelines[timeline_ind];
            timeline::is_timeline_ready(
                app_ctx.ndb,
                app_ctx.pool,
                app_ctx.note_cache,
                timeline,
                &app_ctx.accounts.mutefun(),
            )
        };

        if is_ready {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");

            if let Err(err) = Timeline::poll_notes_into_view(
                timeline_ind,
                current_columns.timelines_mut(),
                app_ctx.ndb,
                &txn,
                app_ctx.unknown_ids,
                app_ctx.note_cache,
                &app_ctx.accounts.mutefun(),
            ) {
                error!("poll_notes_into_view: {err}");
            }
        } else {
            // TODO: show loading?
        }
    }

    if app_ctx.unknown_ids.ready_to_send() {
        unknown_id_send(app_ctx.unknown_ids, app_ctx.pool);
    }

    Ok(())
}

fn unknown_id_send(unknown_ids: &mut UnknownIds, pool: &mut RelayPool) {
    let filter = unknown_ids.filter().expect("filter");
    info!(
        "Getting {} unknown ids from relays",
        unknown_ids.ids().len()
    );
    let msg = ClientMessage::req("unknownids".to_string(), filter);
    unknown_ids.clear();
    pool.send(&msg);
}

#[cfg(feature = "profiling")]
fn setup_profiling() {
    puffin::set_scopes_on(true); // tell puffin to collect data
}

fn update_damus(damus: &mut Damus, app_ctx: &mut AppContext<'_>) {
    let _ctx = app_ctx.egui.clone();
    let ctx = &_ctx;

    app_ctx.accounts.update(app_ctx.ndb, app_ctx.pool, ctx); // update user relay and mute lists

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
                app_ctx.ndb,
                app_ctx.note_cache,
                &mut damus.decks_cache,
                &app_ctx.accounts.mutefun(),
            ) {
                warn!("update_damus init: {err}");
            }
        }

        DamusState::Initialized => (),
    };

    if let Err(err) = try_process_event(damus, app_ctx, ctx) {
        error!("error processing event: {}", err);
    }
}

fn process_event(ndb: &Ndb, _subid: &str, event: &str) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //info!("processing event {}", event);
    if let Err(_err) = ndb.process_event(event) {
        error!("error processing event {}", event);
    }
}

fn handle_eose(
    damus: &mut Damus,
    ctx: &mut AppContext<'_>,
    subid: &str,
    relay_url: &str,
) -> Result<()> {
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
            let txn = Transaction::new(ctx.ndb)?;
            unknowns::update_from_columns(
                &txn,
                ctx.unknown_ids,
                get_active_columns(ctx.accounts, &damus.decks_cache),
                ctx.ndb,
                ctx.note_cache,
            );
            // this is possible if this is the first time
            if ctx.unknown_ids.ready_to_send() {
                unknown_id_send(ctx.unknown_ids, ctx.pool);
            }
        }

        // oneshot subs just close when they're done
        SubKind::OneShot => {
            let msg = ClientMessage::close(subid.to_string());
            ctx.pool.send_to(&msg, relay_url);
        }

        SubKind::FetchingContactList(timeline_uid) => {
            let timeline = if let Some(tl) =
                get_active_columns_mut(ctx.accounts, &mut damus.decks_cache)
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

fn process_message(damus: &mut Damus, ctx: &mut AppContext<'_>, relay: &str, msg: &RelayMessage) {
    match msg {
        RelayMessage::Event(subid, ev) => process_event(ctx.ndb, subid, ev),
        RelayMessage::Notice(msg) => warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(sid) => {
            if let Err(err) = handle_eose(damus, ctx, sid, relay) {
                error!("error handling eose: {}", err);
            }
        }
    }
}

fn render_damus(damus: &mut Damus, app_ctx: &mut AppContext<'_>) {
    if notedeck::ui::is_narrow(app_ctx.egui) {
        render_damus_mobile(damus, app_ctx);
    } else {
        render_damus_desktop(damus, app_ctx);
    }

    // We use this for keeping timestamps and things up to date
    app_ctx.egui.request_repaint_after(Duration::from_secs(1));

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
    pub fn new(ctx: &mut AppContext<'_>, args: &[String]) -> Self {
        // arg parsing

        let parsed_args = ColumnsArgs::parse(args);
        let account = ctx
            .accounts
            .get_selected_account()
            .as_ref()
            .map(|a| a.pubkey.bytes());

        let decks_cache = if !parsed_args.columns.is_empty() {
            info!("DecksCache: loading from command line arguments");
            let mut columns: Columns = Columns::new();
            for col in parsed_args.columns {
                if let Some(timeline) = col.into_timeline(ctx.ndb, account) {
                    columns.add_new_timeline_column(timeline);
                }
            }

            columns_to_decks_cache(columns, account)
        } else if let Some(decks_cache) = crate::storage::load_decks_cache(ctx.path, ctx.ndb) {
            info!(
                "DecksCache: loading from disk {}",
                crate::storage::DECKS_CACHE_FILE
            );
            decks_cache
        } else if let Some(cols) = storage::deserialize_columns(ctx.path, ctx.ndb, account) {
            info!(
                "DecksCache: loading from disk at depreciated location {}",
                crate::storage::COLUMNS_FILE
            );
            columns_to_decks_cache(cols, account)
        } else {
            info!("DecksCache: creating new with demo configuration");
            let mut cache = DecksCache::new_with_demo_config(ctx.ndb);
            for account in ctx.accounts.get_accounts() {
                cache.add_deck_default(account.pubkey);
            }
            set_demo(&mut cache, ctx.ndb, ctx.accounts, ctx.unknown_ids);

            cache
        };

        let debug = ctx.args.debug;
        let support = Support::new(ctx.path);

        Self {
            subscriptions: Subscriptions::default(),
            since_optimize: parsed_args.since_optimize,
            threads: NotesHolderStorage::default(),
            profiles: NotesHolderStorage::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            textmode: parsed_args.textmode,
            //frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            support,
            decks_cache,
            debug,
        }
    }

    pub fn columns_mut(&mut self, accounts: &Accounts) -> &mut Columns {
        get_active_columns_mut(accounts, &mut self.decks_cache)
    }

    pub fn columns(&self, accounts: &Accounts) -> &Columns {
        get_active_columns(accounts, &self.decks_cache)
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

        let support = Support::new(&path);

        Self {
            debug,
            subscriptions: Subscriptions::default(),
            since_optimize: true,
            threads: NotesHolderStorage::default(),
            profiles: NotesHolderStorage::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            textmode: false,
            //frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            support,
            decks_cache,
        }
    }

    pub fn subscriptions(&mut self) -> &mut HashMap<String, SubKind> {
        &mut self.subscriptions.subs
    }

    pub fn threads(&self) -> &NotesHolderStorage<Thread> {
        &self.threads
    }

    pub fn threads_mut(&mut self) -> &mut NotesHolderStorage<Thread> {
        &mut self.threads
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

fn render_damus_mobile(app: &mut Damus, app_ctx: &mut AppContext<'_>) {
    let _ctx = app_ctx.egui.clone();
    let ctx = &_ctx;

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //let routes = app.timelines[0].routes.clone();

    main_panel(&ctx.style(), notedeck::ui::is_narrow(ctx)).show(ctx, |ui| {
        if !app.columns(app_ctx.accounts).columns().is_empty()
            && nav::render_nav(0, app, app_ctx, ui).process_render_nav_response(app, app_ctx)
        {
            storage::save_decks_cache(app_ctx.path, &app.decks_cache);
        }
    });
}

fn margin_top(narrow: bool) -> f32 {
    #[cfg(target_os = "android")]
    {
        // FIXME - query the system bar height and adjust more precisely
        let _ = narrow; // suppress compiler warning on android
        40.0
    }
    #[cfg(not(target_os = "android"))]
    {
        if narrow {
            50.0
        } else {
            0.0
        }
    }
}

fn main_panel(style: &Style, narrow: bool) -> egui::CentralPanel {
    let inner_margin = egui::Margin {
        top: margin_top(narrow),
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

fn render_damus_desktop(app: &mut Damus, app_ctx: &mut AppContext<'_>) {
    let _ctx = app_ctx.egui.clone();
    let ctx = &_ctx;

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let screen_size = ctx.screen_rect().width();
    let calc_panel_width = (screen_size
        / get_active_columns(app_ctx.accounts, &app.decks_cache).num_columns() as f32)
        - 30.0;
    let min_width = 320.0;
    let need_scroll = calc_panel_width < min_width;
    let panel_sizes = if need_scroll {
        Size::exact(min_width)
    } else {
        Size::remainder()
    };

    main_panel(&ctx.style(), notedeck::ui::is_narrow(ctx)).show(ctx, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if need_scroll {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                timelines_view(ui, panel_sizes, app, app_ctx);
            });
        } else {
            timelines_view(ui, panel_sizes, app, app_ctx);
        }
    });
}

fn timelines_view(ui: &mut egui::Ui, sizes: Size, app: &mut Damus, ctx: &mut AppContext<'_>) {
    StripBuilder::new(ui)
        .size(Size::exact(ui::side_panel::SIDE_PANEL_WIDTH))
        .sizes(
            sizes,
            get_active_columns(ctx.accounts, &app.decks_cache).num_columns(),
        )
        .clip(true)
        .horizontal(|mut strip| {
            let mut side_panel_action: Option<nav::SwitchingAction> = None;
            strip.cell(|ui| {
                let rect = ui.available_rect_before_wrap();
                let side_panel = DesktopSidePanel::new(
                    ctx.ndb,
                    ctx.img_cache,
                    ctx.accounts.get_selected_account(),
                    &app.decks_cache,
                )
                .show(ui);

                if side_panel.response.clicked() || side_panel.response.secondary_clicked() {
                    if let Some(action) = DesktopSidePanel::perform_action(
                        &mut app.decks_cache,
                        ctx.accounts,
                        &mut app.support,
                        ctx.theme,
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

            let mut save_cols = false;
            if let Some(action) = side_panel_action {
                save_cols = save_cols || action.process(app, ctx);
            }

            let num_cols = app.columns(ctx.accounts).num_columns();
            let mut responses = Vec::with_capacity(num_cols);
            for col_index in 0..num_cols {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    responses.push(nav::render_nav(col_index, app, ctx, ui));

                    // vertical line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                //strip.cell(|ui| timeline::timeline_view(ui, app, timeline_ind));
            }

            for response in responses {
                let save = response.process_render_nav_response(app, ctx);
                save_cols = save_cols || save;
            }

            if save_cols {
                storage::save_decks_cache(ctx.path, &app.decks_cache);
            }
        });
}

impl notedeck::App for Damus {
    fn update(&mut self, ctx: &mut AppContext<'_>) {
        /*
        self.app
            .frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);
        */

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
        decks_cache.get_fallback_pubkey()
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
    decks_cache: &mut DecksCache,
    ndb: &Ndb,
    accounts: &mut Accounts,
    unk_ids: &mut UnknownIds,
) {
    let txn = Transaction::new(ndb).expect("txn");
    accounts
        .add_account(Keypair::only_pubkey(*decks_cache.get_fallback_pubkey()))
        .process_action(unk_ids, ndb, &txn);
    accounts.select_account(accounts.num_accounts() - 1);
}

fn columns_to_decks_cache(cols: Columns, key: Option<&[u8; 32]>) -> DecksCache {
    let mut account_to_decks: HashMap<Pubkey, Decks> = Default::default();
    let decks = Decks::new(crate::decks::Deck::new_with_columns(
        crate::decks::Deck::default().icon,
        "My Deck".to_owned(),
        cols,
    ));

    let account = if let Some(key) = key {
        Pubkey::new(*key)
    } else {
        FALLBACK_PUBKEY()
    };
    account_to_decks.insert(account, decks);
    DecksCache::new(account_to_decks)
}
