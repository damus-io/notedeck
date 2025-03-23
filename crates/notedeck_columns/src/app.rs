use crate::{
    args::ColumnsArgs,
    column::Columns,
    decks::{Decks, DecksCache, FALLBACK_PUBKEY},
    draft::Drafts,
    nav, storage,
    subscriptions::{SubKind, Subscriptions},
    support::Support,
    timeline::{self, TimelineCache},
    ui::{self, note::NoteOptions, DesktopSidePanel},
    view_state::ViewState,
    Result,
};

use notedeck::{
    subman::LegacyRelayHandler, Accounts, AppContext, DataPath, DataPathType, FilterState,
    NoteCache, RelaySpec, SubError, SubMan, UnknownIds,
};

use enostr::{ClientMessage, Keypair, Pubkey, RelayPool};
use uuid::Uuid;

use egui_extras::{Size, StripBuilder};

use nostrdb::{Ndb, Transaction};

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error, info, warn};

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

    //frame_history: crate::frame_history::FrameHistory,

    // TODO: make these bitflags
    /// Were columns loaded from the commandline? If so disable persistence.
    pub tmp_columns: bool,
    pub debug: bool,
    pub since_optimize: bool,
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

struct RelayHandler<'a> {
    // From AppContext
    unknown_ids: &'a mut UnknownIds,
    note_cache: &'a mut NoteCache,

    // From Damus
    subscriptions: &'a mut Subscriptions,
    timeline_cache: &'a mut TimelineCache,

    since_optimize: bool,
}

impl<'a> RelayHandler<'a> {
    fn new(
        unknown_ids: &'a mut UnknownIds,
        note_cache: &'a mut NoteCache,
        subscriptions: &'a mut Subscriptions,
        timeline_cache: &'a mut TimelineCache,
        since_optimize: bool,
    ) -> Self {
        RelayHandler {
            unknown_ids,
            note_cache,
            subscriptions,
            timeline_cache,
            since_optimize,
        }
    }
}

impl LegacyRelayHandler for RelayHandler<'_> {
    /// Handle relay opened
    fn handle_opened(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, relay: &str) {
        timeline::send_initial_timeline_filters(
            ndb,
            self.since_optimize,
            self.timeline_cache,
            self.subscriptions,
            pool,
            relay,
        );
    }

    /// Handle end-of-stored-events
    fn handle_eose(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, sid: &str, relay: &str) {
        do_handle_eose(
            ndb,
            pool,
            &*self.subscriptions,
            self.timeline_cache,
            sid,
            relay,
            self.unknown_ids,
            self.note_cache,
        )
        .ok(); // we've already logged the error and intend to keep going
    }
}

fn try_process_event<'a>(
    damus: &'a mut Damus,
    app_ctx: &'a mut AppContext<'_>,
    ctx: &egui::Context,
) {
    let current_columns = get_active_columns_mut(app_ctx.accounts, &mut damus.decks_cache);
    ctx.input(|i| handle_key_events(i, current_columns));

    {
        let default_relays = app_ctx.accounts.get_all_selected_account_relays();
        let mut relay_handler = RelayHandler::new(
            app_ctx.unknown_ids,
            app_ctx.note_cache,
            &mut damus.subscriptions,
            &mut damus.timeline_cache,
            damus.since_optimize,
        );
        app_ctx
            .subman
            .process_relays(&mut relay_handler, &default_relays)
            .ok();
    }

    for (_kind, timeline) in damus.timeline_cache.timelines.iter_mut() {
        let is_ready = timeline::is_timeline_ready(
            app_ctx.ndb,
            app_ctx.subman.pool(),
            app_ctx.note_cache,
            timeline,
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
        unknown_id_send(
            app_ctx.unknown_ids,
            app_ctx.subman,
            &app_ctx.accounts.get_all_selected_account_relays(),
        );
    }
}

fn unknown_id_send(
    unknown_ids: &mut UnknownIds,
    subman: &mut SubMan,
    default_relays: &[RelaySpec],
) {
    info!("Getting {} unknown ids from relays", &unknown_ids.numids());
    for subspec in unknown_ids.generate_resolution_requests() {
        debug!("unknown_ids subscribe: {:?}", subspec);
        match subman.subscribe(subspec, default_relays) {
            Err(err) => error!("unknown_id_send subscribe failed: {:?}", err),
            Ok(mut rcvr) => {
                tokio::spawn(async move {
                    loop {
                        match rcvr.next().await {
                            Err(SubError::StreamEnded) => {
                                debug!("unknown_id_send: {} complete", rcvr.idstr());
                                break;
                            }
                            Err(err) => {
                                error!("unknown_id_send: {}: error: {:?}", rcvr.idstr(), err);
                                break;
                            }
                            Ok(note_keys) => {
                                debug!("{}: received note keys: {:?}", rcvr.idstr(), note_keys);
                                // only need the prefetch into ndb, all done
                            }
                        }
                    }
                });
            }
        }
    }
    unknown_ids.clear();
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

    try_process_event(damus, app_ctx, ctx);
}

#[allow(clippy::too_many_arguments)]
fn do_handle_eose(
    ndb: &mut Ndb,
    pool: &mut RelayPool,
    subscriptions: &Subscriptions,
    timeline_cache: &mut TimelineCache,
    subid: &str,
    relay_url: &str,
    unknown_ids: &mut UnknownIds,
    note_cache: &mut NoteCache,
) -> Result<()> {
    debug!("app do_handle_eose {} {}", subid, relay_url);
    let sub_kind = if let Some(sub_kind) = subscriptions.subs.get(subid) {
        sub_kind
    } else {
        let n_subids = subscriptions.subs.len();
        warn!(
            "got unknown eose subid {} relay {}, {} tracked subscriptions",
            subid, relay_url, n_subids
        );
        return Ok(());
    };

    match sub_kind {
        SubKind::Timeline(_) => {
            // eose on timeline? whatevs
        }
        SubKind::Initial => {
            // let txn = Transaction::new(ndb)?;
            // unknowns::update_from_columns(&txn, unknown_ids, timeline_cache, ndb, note_cache);
        }

        // oneshot subs just close when they're done
        SubKind::OneShot => {
            let msg = ClientMessage::close(subid.to_string());
            debug!("app do_handle_eose close {} {}", subid, relay_url);
            pool.send_to(&msg, relay_url);
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

fn render_damus(damus: &mut Damus, app_ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
    if notedeck::ui::is_narrow(ui.ctx()) {
        render_damus_mobile(damus, app_ctx, ui);
    } else {
        render_damus_desktop(damus, app_ctx, ui);
    }

    // We use this for keeping timestamps and things up to date
    ui.ctx().request_repaint_after(Duration::from_secs(1));
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
            ColumnsArgs::parse(args, ctx.accounts.selected_account_pubkey());

        let account = ctx.accounts.selected_account_pubkey_bytes();

        let mut timeline_cache = TimelineCache::default();
        let tmp_columns = !parsed_args.columns.is_empty();
        let decks_cache = if tmp_columns {
            info!("DecksCache: loading from command line arguments");
            let mut columns: Columns = Columns::new();
            let txn = Transaction::new(ctx.ndb).unwrap();
            for col in parsed_args.columns {
                let timeline_kind = col.into_timeline_kind();
                if let Some(add_result) = columns.add_new_timeline_column(
                    &mut timeline_cache,
                    &txn,
                    ctx.ndb,
                    ctx.note_cache,
                    ctx.subman.pool(),
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
            let mut cache = DecksCache::new_with_demo_config(&mut timeline_cache, ctx);
            for account in ctx.accounts.get_accounts() {
                cache.add_deck_default(account.key.pubkey);
            }
            set_demo(&mut cache, ctx.ndb, ctx.accounts, ctx.unknown_ids);

            cache
        };

        let debug = ctx.args.debug;
        let support = Support::new(ctx.path);
        let mut note_options = NoteOptions::default();
        note_options.set_textmode(parsed_args.textmode);
        note_options.set_scramble_text(parsed_args.scramble);
        note_options.set_hide_media(parsed_args.no_media);

        Self {
            subscriptions: Subscriptions::default(),
            since_optimize: parsed_args.since_optimize,
            timeline_cache,
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            note_options,
            //frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            tmp_columns,
            support,
            decks_cache,
            debug,
            unrecognized_args,
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
        let imgcache_dir = path.path(DataPathType::Cache);
        let _ = std::fs::create_dir_all(imgcache_dir.clone());
        let debug = true;

        let support = Support::new(&path);

        Self {
            debug,
            subscriptions: Subscriptions::default(),
            since_optimize: true,
            timeline_cache: TimelineCache::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            note_options: NoteOptions::default(),
            tmp_columns: true,
            //frame_history: FrameHistory::default(),
            view_state: ViewState::default(),
            support,
            decks_cache,
            unrecognized_args: BTreeSet::default(),
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

fn render_damus_mobile(app: &mut Damus, app_ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //let routes = app.timelines[0].routes.clone();

    if !app.columns(app_ctx.accounts).columns().is_empty()
        && nav::render_nav(0, ui.available_rect_before_wrap(), app, app_ctx, ui)
            .process_render_nav_response(app, app_ctx)
        && !app.tmp_columns
    {
        storage::save_decks_cache(app_ctx.path, &app.decks_cache);
    }
}

fn render_damus_desktop(app: &mut Damus, app_ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

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
        egui::ScrollArea::horizontal().show(ui, |ui| {
            timelines_view(ui, panel_sizes, app, app_ctx);
        });
    } else {
        timelines_view(ui, panel_sizes, app, app_ctx);
    }
}

fn timelines_view(ui: &mut egui::Ui, sizes: Size, app: &mut Damus, ctx: &mut AppContext<'_>) {
    let num_cols = get_active_columns(ctx.accounts, &app.decks_cache).num_columns();
    StripBuilder::new(ui)
        .size(Size::exact(ui::side_panel::SIDE_PANEL_WIDTH))
        .sizes(sizes, num_cols)
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

            let mut responses = Vec::with_capacity(num_cols);
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
                });

                //strip.cell(|ui| timeline::timeline_view(ui, app, timeline_ind));
            }

            // process the side panel action after so we don't change the number of columns during
            // StripBuilder rendering
            let mut save_cols = false;
            if let Some(action) = side_panel_action {
                save_cols =
                    save_cols || action.process(&mut app.timeline_cache, &mut app.decks_cache, ctx);
            }

            for response in responses {
                let save = response.process_render_nav_response(app, ctx);
                save_cols = save_cols || save;
            }

            if app.tmp_columns {
                save_cols = false;
            }

            if save_cols {
                storage::save_decks_cache(ctx.path, &app.decks_cache);
            }
        });
}

impl notedeck::App for Damus {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        /*
        self.app
            .frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);
        */

        update_damus(self, ctx, ui.ctx());
        render_damus(self, ctx, ui);
    }
}

pub fn get_active_columns<'a>(accounts: &Accounts, decks_cache: &'a DecksCache) -> &'a Columns {
    get_decks(accounts, decks_cache).active().columns()
}

pub fn get_decks<'a>(accounts: &Accounts, decks_cache: &'a DecksCache) -> &'a Decks {
    let key = accounts
        .selected_account_pubkey()
        .unwrap_or_else(|| decks_cache.get_fallback_pubkey());
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
    match accounts.selected_account_pubkey() {
        Some(acc) => decks_cache.decks_mut(acc),
        None => decks_cache.fallback_mut(),
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
