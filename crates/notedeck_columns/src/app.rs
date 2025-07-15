use crate::{
    args::{ColumnsArgs, ColumnsFlag},
    column::Columns,
    decks::{Decks, DecksCache},
    draft::Drafts,
    nav::{self, ProcessNavResult},
    options::AppOptions,
    route::Route,
    storage,
    subscriptions::{SubKind, Subscriptions},
    support::Support,
    timeline::{self, kind::ListKind, thread::Threads, TimelineCache, TimelineKind},
    ui::{self, DesktopSidePanel, SidePanelAction},
    view_state::ViewState,
    Result,
};

use egui_extras::{Size, StripBuilder};
use enostr::{ClientMessage, PoolRelay, Pubkey, RelayEvent, RelayMessage, RelayPool};
use nostrdb::Transaction;
use notedeck::{
    ui::is_narrow, Accounts, AppAction, AppContext, DataPath, DataPathType, FilterState, UnknownIds,
};
use notedeck_ui::{jobs::JobsCache, NoteOptions};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

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
    pub timeline_cache: TimelineCache,
    pub subscriptions: Subscriptions,
    pub support: Support,
    pub jobs: JobsCache,
    pub threads: Threads,

    //frame_history: crate::frame_history::FrameHistory,

    // TODO: make these bitflags
    /// Were columns loaded from the commandline? If so disable persistence.
    pub options: AppOptions,
    pub note_options: NoteOptions,

    pub unrecognized_args: BTreeSet<String>,
}

fn handle_key_events(input: &egui::InputState, columns: &mut Columns) {
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
    let current_columns = get_active_columns_mut(app_ctx.accounts, &mut damus.decks_cache);
    ctx.input(|i| handle_key_events(i, current_columns));

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
                    damus.options.contains(AppOptions::SinceOptimize),
                    &mut damus.timeline_cache,
                    &mut damus.subscriptions,
                    app_ctx.pool,
                    &ev.relay,
                    app_ctx.accounts,
                );
            }
            // TODO: handle reconnects
            RelayEvent::Closed => warn!("{} connection closed", &ev.relay),
            RelayEvent::Error(e) => error!("{}: {}", &ev.relay, e),
            RelayEvent::Other(msg) => trace!("other event {:?}", &msg),
            RelayEvent::Message(msg) => {
                process_message(damus, app_ctx, &ev.relay, &msg);
            }
        }
    }

    for (_kind, timeline) in damus.timeline_cache.timelines.iter_mut() {
        let is_ready = timeline::is_timeline_ready(
            app_ctx.ndb,
            app_ctx.pool,
            app_ctx.note_cache,
            timeline,
            app_ctx.accounts,
        );

        if is_ready {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            // only thread timelines are reversed
            let reversed = false;

            if let Err(err) = timeline.poll_notes_into_view(
                app_ctx.ndb,
                &txn,
                app_ctx.unknown_ids,
                app_ctx.note_cache,
                reversed,
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
    debug!("unknown_id_send called on: {:?}", &unknown_ids);
    let filter = unknown_ids.filter().expect("filter");
    debug!(
        "Getting {} unknown ids from relays",
        unknown_ids.ids_iter().len()
    );
    let msg = ClientMessage::req("unknownids".to_string(), filter);
    unknown_ids.clear();
    pool.send(&msg);
}

fn update_damus(damus: &mut Damus, app_ctx: &mut AppContext<'_>, ctx: &egui::Context) {
    app_ctx.img_cache.urls.cache.handle_io();

    match damus.state {
        DamusState::Initializing => {
            damus.state = DamusState::Initialized;
            // this lets our eose handler know to close unknownids right away
            damus
                .subscriptions()
                .insert("unknownids".to_string(), SubKind::OneShot);
            if let Err(err) = timeline::setup_initial_nostrdb_subs(
                app_ctx.ndb,
                app_ctx.note_cache,
                &mut damus.timeline_cache,
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

fn handle_eose(
    subscriptions: &Subscriptions,
    timeline_cache: &mut TimelineCache,
    ctx: &mut AppContext<'_>,
    subid: &str,
    relay_url: &str,
) -> Result<()> {
    let sub_kind = if let Some(sub_kind) = subscriptions.subs.get(subid) {
        sub_kind
    } else {
        let n_subids = subscriptions.subs.len();
        warn!(
            "got unknown eose subid {}, {} tracked subscriptions",
            subid, n_subids
        );
        return Ok(());
    };

    match sub_kind {
        SubKind::Timeline(_) => {
            // eose on timeline? whatevs
        }
        SubKind::Initial => {
            //let txn = Transaction::new(ctx.ndb)?;
            //unknowns::update_from_columns(
            //    &txn,
            //    ctx.unknown_ids,
            //    timeline_cache,
            //    ctx.ndb,
            //    ctx.note_cache,
            //);
            //// this is possible if this is the first time
            //if ctx.unknown_ids.ready_to_send() {
            //    unknown_id_send(ctx.unknown_ids, ctx.pool);
            //}
        }

        // oneshot subs just close when they're done
        SubKind::OneShot => {
            let msg = ClientMessage::close(subid.to_string());
            ctx.pool.send_to(&msg, relay_url);
        }

        SubKind::FetchingContactList(timeline_uid) => {
            let timeline = if let Some(tl) = timeline_cache.timelines.get_mut(timeline_uid) {
                tl
            } else {
                error!(
                    "timeline uid:{} not found for FetchingContactList",
                    timeline_uid
                );
                return Ok(());
            };

            let filter_state = timeline.filter.get_mut(relay_url);

            let FilterState::FetchingRemote(fetching_remote_type) = filter_state else {
                // TODO: we could have multiple contact list results, we need
                // to check to see if this one is newer and use that instead
                warn!(
                    "Expected timeline to have FetchingRemote state but was {:?}",
                    timeline.filter
                );
                return Ok(());
            };

            let new_filter_state = match fetching_remote_type {
                notedeck::filter::FetchingRemoteType::Normal(unified_subscription) => {
                    FilterState::got_remote(unified_subscription.local)
                }
                notedeck::filter::FetchingRemoteType::Contact => {
                    FilterState::GotRemote(notedeck::filter::GotRemoteType::Contact)
                }
            };

            // We take the subscription id and pass it to the new state of
            // "GotRemote". This will let future frames know that it can try
            // to look for the contact list in nostrdb.
            timeline
                .filter
                .set_relay_state(relay_url.to_string(), new_filter_state);
        }
    }

    Ok(())
}

fn process_message(damus: &mut Damus, ctx: &mut AppContext<'_>, relay: &str, msg: &RelayMessage) {
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
        RelayMessage::Notice(msg) => warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(sid) => {
            if let Err(err) = handle_eose(
                &damus.subscriptions,
                &mut damus.timeline_cache,
                ctx,
                sid,
                relay,
            ) {
                error!("error handling eose: {}", err);
            }
        }
    }
}

fn render_damus(
    damus: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> Option<AppAction> {
    let app_action = if notedeck::ui::is_narrow(ui.ctx()) {
        render_damus_mobile(damus, app_ctx, ui)
    } else {
        render_damus_desktop(damus, app_ctx, ui)
    };

    // We use this for keeping timestamps and things up to date
    ui.ctx().request_repaint_after(Duration::from_secs(5));

    app_action
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

        let (parsed_args, unrecognized_args) =
            ColumnsArgs::parse(args, Some(ctx.accounts.selected_account_pubkey()));

        let account = ctx.accounts.selected_account_pubkey_bytes();

        let mut timeline_cache = TimelineCache::default();
        let mut options = AppOptions::default();
        let tmp_columns = !parsed_args.columns.is_empty();
        options.set(AppOptions::TmpColumns, tmp_columns);

        let decks_cache = if tmp_columns {
            info!("DecksCache: loading from command line arguments");
            let mut columns: Columns = Columns::new();
            let txn = Transaction::new(ctx.ndb).unwrap();
            for col in &parsed_args.columns {
                let timeline_kind = col.clone().into_timeline_kind();
                if let Some(add_result) = columns.add_new_timeline_column(
                    &mut timeline_cache,
                    &txn,
                    ctx.ndb,
                    ctx.note_cache,
                    ctx.pool,
                    &timeline_kind,
                ) {
                    add_result.process(
                        ctx.ndb,
                        ctx.note_cache,
                        &txn,
                        &mut timeline_cache,
                        ctx.unknown_ids,
                    );
                }
            }

            columns_to_decks_cache(columns, account)
        } else if let Some(decks_cache) =
            crate::storage::load_decks_cache(ctx.path, ctx.ndb, &mut timeline_cache)
        {
            info!(
                "DecksCache: loading from disk {}",
                crate::storage::DECKS_CACHE_FILE
            );
            decks_cache
        } else {
            info!("DecksCache: creating new with demo configuration");
            DecksCache::new_with_demo_config(&mut timeline_cache, ctx)
            //for (pk, _) in &ctx.accounts.cache {
            //    cache.add_deck_default(*pk);
            //}
        };

        let support = Support::new(ctx.path);
        let mut note_options = NoteOptions::default();
        note_options.set(
            NoteOptions::Textmode,
            parsed_args.is_flag_set(ColumnsFlag::Textmode),
        );
        note_options.set(
            NoteOptions::ScrambleText,
            parsed_args.is_flag_set(ColumnsFlag::Scramble),
        );
        note_options.set(
            NoteOptions::HideMedia,
            parsed_args.is_flag_set(ColumnsFlag::NoMedia),
        );
        options.set(AppOptions::Debug, ctx.args.debug);
        options.set(
            AppOptions::SinceOptimize,
            parsed_args.is_flag_set(ColumnsFlag::SinceOptimize),
        );

        let jobs = JobsCache::default();

        let threads = Threads::default();

        Self {
            subscriptions: Subscriptions::default(),
            timeline_cache,
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            note_options,
            options,
            //frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            support,
            decks_cache,
            unrecognized_args,
            jobs,
            threads,
        }
    }

    /// Scroll to the top of the currently selected column. This is called
    /// by the chrome when you click the toolbar
    pub fn scroll_to_top(&mut self) {
        self.options.insert(AppOptions::ScrollToTop)
    }

    pub fn columns_mut(&mut self, accounts: &Accounts) -> &mut Columns {
        get_active_columns_mut(accounts, &mut self.decks_cache)
    }

    pub fn columns(&self, accounts: &Accounts) -> &Columns {
        get_active_columns(accounts, &self.decks_cache)
    }

    pub fn gen_subid(&self, kind: &SubKind) -> String {
        if self.options.contains(AppOptions::Debug) {
            format!("{:?}", kind)
        } else {
            Uuid::new_v4().to_string()
        }
    }

    pub fn mock<P: AsRef<Path>>(data_path: P) -> Self {
        let decks_cache = DecksCache::default();

        let path = DataPath::new(&data_path);
        let imgcache_dir = path.path(DataPathType::Cache);
        let _ = std::fs::create_dir_all(imgcache_dir.clone());
        let options = AppOptions::default() | AppOptions::Debug | AppOptions::TmpColumns;

        let support = Support::new(&path);

        Self {
            subscriptions: Subscriptions::default(),
            timeline_cache: TimelineCache::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            note_options: NoteOptions::default(),
            //frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            support,
            options,
            decks_cache,
            unrecognized_args: BTreeSet::default(),
            jobs: JobsCache::default(),
            threads: Threads::default(),
        }
    }

    pub fn subscriptions(&mut self) -> &mut HashMap<String, SubKind> {
        &mut self.subscriptions.subs
    }

    pub fn unrecognized_args(&self) -> &BTreeSet<String> {
        &self.unrecognized_args
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

#[profiling::function]
fn render_damus_mobile(
    app: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> Option<AppAction> {
    //let routes = app.timelines[0].routes.clone();

    let rect = ui.available_rect_before_wrap();
    let mut app_action: Option<AppAction> = None;

    let active_col = app.columns_mut(app_ctx.accounts).selected as usize;
    if !app.columns(app_ctx.accounts).columns().is_empty() {
        let r = nav::render_nav(
            active_col,
            ui.available_rect_before_wrap(),
            app,
            app_ctx,
            ui,
        )
        .process_render_nav_response(app, app_ctx, ui);
        if let Some(r) = &r {
            match r {
                ProcessNavResult::SwitchOccurred => {
                    if !app.options.contains(AppOptions::TmpColumns) {
                        storage::save_decks_cache(app_ctx.path, &app.decks_cache);
                    }
                }

                ProcessNavResult::PfpClicked => {
                    app_action = Some(AppAction::ToggleChrome);
                }
            }
        }
    }

    hovering_post_button(ui, app, app_ctx, rect);

    app_action
}

fn hovering_post_button(
    ui: &mut egui::Ui,
    app: &mut Damus,
    app_ctx: &mut AppContext,
    mut rect: egui::Rect,
) {
    let should_show_compose = should_show_compose_button(&app.decks_cache, app_ctx.accounts);
    let btn_id = ui.id().with("hover_post_btn");
    let button_y = ui
        .ctx()
        .animate_bool_responsive(btn_id, should_show_compose);
    rect.min.x = rect.max.x - if is_narrow(ui.ctx()) { 60.0 } else { 100.0 };
    rect.min.y = rect.max.y - 100.0 * button_y;

    let darkmode = ui.ctx().style().visuals.dark_mode;

    // only show the compose button on profile pages and on home
    let compose_resp = ui.put(rect, ui::post::compose_note_button(darkmode));
    if compose_resp.hovered() {
        notedeck_ui::show_pointer(ui);
    }
    if compose_resp.clicked() && !app.columns(app_ctx.accounts).columns().is_empty() {
        // just use the some side panel logic as the desktop
        DesktopSidePanel::perform_action(
            &mut app.decks_cache,
            app_ctx.accounts,
            SidePanelAction::ComposeNote,
        );
    }
}

/// Should we show the compose button? When in threads we should hide it, etc
fn should_show_compose_button(decks: &DecksCache, accounts: &Accounts) -> bool {
    let Some(col) = decks.selected_column(accounts) else {
        return false;
    };

    match col.router().top() {
        Route::Timeline(timeline_kind) => {
            match timeline_kind {
                TimelineKind::List(list_kind) => match list_kind {
                    ListKind::Contact(_pk) => true,
                },

                TimelineKind::Algo(_pk) => true,
                TimelineKind::Profile(_pk) => true,
                TimelineKind::Universe => true,
                TimelineKind::Generic(_) => true,
                TimelineKind::Hashtag(_) => true,

                // no!
                TimelineKind::Search(_) => false,
                TimelineKind::Notifications(_) => false,
            }
        }

        Route::Thread(_) => false,
        Route::Accounts(_) => false,
        Route::Reply(_) => false,
        Route::Quote(_) => false,
        Route::Relays => false,
        Route::ComposeNote => false,
        Route::AddColumn(_) => false,
        Route::EditProfile(_) => false,
        Route::Support => false,
        Route::NewDeck => false,
        Route::Search => false,
        Route::EditDeck(_) => false,
        Route::Wallet(_) => false,
        Route::CustomizeZapAmount(_) => false,
    }
}

#[profiling::function]
fn render_damus_desktop(
    app: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> Option<AppAction> {
    let screen_size = ui.ctx().screen_rect().width();
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

    ui.spacing_mut().item_spacing.x = 0.0;

    if need_scroll {
        egui::ScrollArea::horizontal()
            .show(ui, |ui| timelines_view(ui, panel_sizes, app, app_ctx))
            .inner
    } else {
        timelines_view(ui, panel_sizes, app, app_ctx)
    }
}

fn timelines_view(
    ui: &mut egui::Ui,
    sizes: Size,
    app: &mut Damus,
    ctx: &mut AppContext<'_>,
) -> Option<AppAction> {
    let num_cols = get_active_columns(ctx.accounts, &app.decks_cache).num_columns();
    let mut side_panel_action: Option<nav::SwitchingAction> = None;
    let mut responses = Vec::with_capacity(num_cols);

    StripBuilder::new(ui)
        .size(Size::exact(ui::side_panel::SIDE_PANEL_WIDTH))
        .sizes(sizes, num_cols)
        .clip(true)
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                let rect = ui.available_rect_before_wrap();
                let side_panel =
                    DesktopSidePanel::new(ctx.accounts.get_selected_account(), &app.decks_cache)
                        .show(ui);

                if let Some(side_panel) = side_panel {
                    if side_panel.response.clicked() || side_panel.response.secondary_clicked() {
                        if let Some(action) = DesktopSidePanel::perform_action(
                            &mut app.decks_cache,
                            ctx.accounts,
                            side_panel.action,
                        ) {
                            side_panel_action = Some(action);
                        }
                    }
                }

                // debug
                /*
                ui.painter().rect(
                    rect,
                    0,
                    egui::Color32::RED,
                    egui::Stroke::new(1.0, egui::Color32::BLUE),
                    egui::StrokeKind::Inside,
                );
                */

                // vertical sidebar line
                ui.painter().vline(
                    rect.right(),
                    rect.y_range(),
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
            });

            for col_index in 0..num_cols {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    let v_line_stroke = ui.visuals().widgets.noninteractive.bg_stroke;
                    let inner_rect = {
                        let mut inner = rect;
                        inner.set_right(rect.right() - v_line_stroke.width);
                        inner
                    };
                    responses.push(nav::render_nav(col_index, inner_rect, app, ctx, ui));

                    // vertical line
                    ui.painter()
                        .vline(rect.right(), rect.y_range(), v_line_stroke);

                    // we need borrow ui context for processing, so proces
                    // responses in the last cell

                    if col_index == num_cols - 1 {}
                });

                //strip.cell(|ui| timeline::timeline_view(ui, app, timeline_ind));
            }
        });

    // process the side panel action after so we don't change the number of columns during
    // StripBuilder rendering
    let mut save_cols = false;
    if let Some(action) = side_panel_action {
        save_cols = save_cols
            || action.process(&mut app.timeline_cache, &mut app.decks_cache, ctx, ui.ctx());
    }

    let mut app_action: Option<AppAction> = None;

    for response in responses {
        let nav_result = response.process_render_nav_response(app, ctx, ui);

        if let Some(nr) = &nav_result {
            match nr {
                ProcessNavResult::SwitchOccurred => save_cols = true,

                ProcessNavResult::PfpClicked => {
                    app_action = Some(AppAction::ToggleChrome);
                }
            }
        }
    }

    if app.options.contains(AppOptions::TmpColumns) {
        save_cols = false;
    }

    if save_cols {
        storage::save_decks_cache(ctx.path, &app.decks_cache);
    }

    app_action
}

impl notedeck::App for Damus {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
        /*
        self.app
            .frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);
        */

        update_damus(self, ctx, ui.ctx());
        render_damus(self, ctx, ui)
    }
}

pub fn get_active_columns<'a>(accounts: &Accounts, decks_cache: &'a DecksCache) -> &'a Columns {
    get_decks(accounts, decks_cache).active().columns()
}

pub fn get_decks<'a>(accounts: &Accounts, decks_cache: &'a DecksCache) -> &'a Decks {
    let key = accounts.selected_account_pubkey();
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
    decks_cache.decks_mut(accounts.selected_account_pubkey())
}

fn columns_to_decks_cache(cols: Columns, key: &[u8; 32]) -> DecksCache {
    let mut account_to_decks: HashMap<Pubkey, Decks> = Default::default();
    let decks = Decks::new(crate::decks::Deck::new_with_columns(
        crate::decks::Deck::default().icon,
        "My Deck".to_owned(),
        cols,
    ));

    let account = Pubkey::new(*key);
    account_to_decks.insert(account, decks);
    DecksCache::new(account_to_decks)
}
