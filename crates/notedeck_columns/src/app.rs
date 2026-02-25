use crate::{
    args::{ColumnsArgs, ColumnsFlag},
    column::Columns,
    decks::{Decks, DecksCache},
    draft::Drafts,
    nav::{self, ProcessNavResult},
    onboarding::Onboarding,
    options::AppOptions,
    route::Route,
    storage,
    subscriptions::{SubKind, Subscriptions},
    support::Support,
    timeline::{self, kind::ListKind, thread::Threads, TimelineCache, TimelineKind},
    timeline_loader::{TimelineLoader, TimelineLoaderMsg},
    toolbar::unseen_notification,
    ui::{self, toolbar::toolbar, DesktopSidePanel, SidePanelAction},
    view_state::ViewState,
    Result,
};
use egui_extras::{Size, StripBuilder};
use enostr::{ClientMessage, Pubkey, RelayEvent, RelayMessage};
use nostrdb::Transaction;
use notedeck::{
    tr, try_process_events_core, ui::is_compiled_as_mobile, ui::is_narrow, Accounts, AppAction,
    AppContext, AppResponse, DataPath, DataPathType, FilterState, Images, Localization,
    MediaJobSender, NotedeckOptions, SettingsHandler,
};
use notedeck_ui::{
    media::{MediaViewer, MediaViewerFlags, MediaViewerState},
    NoteOptions,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Max timeline loader messages to process per frame to avoid UI stalls.
const MAX_TIMELINE_LOADER_MSGS_PER_FRAME: usize = 8;

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
    pub threads: Threads,
    /// Background loader for initial timeline scans.
    timeline_loader: TimelineLoader,
    /// Timelines currently loading initial notes.
    inflight_timeline_loads: HashSet<TimelineKind>,
    /// Timelines that have completed their initial load.
    loaded_timeline_loads: HashSet<TimelineKind>,

    //frame_history: crate::frame_history::FrameHistory,

    // TODO: make these bitflags
    /// Were columns loaded from the commandline? If so disable persistence.
    pub options: AppOptions,
    pub note_options: NoteOptions,

    pub unrecognized_args: BTreeSet<String>,

    /// keep track of follow packs
    pub onboarding: Onboarding,

    /// Track which column is hovered for mouse back/forward navigation
    hovered_column: Option<usize>,
}

#[profiling::function]
fn handle_egui_events(
    input: &egui::InputState,
    columns: &mut Columns,
    hovered_column: Option<usize>,
    wants_keyboard_input: bool,
) {
    for event in &input.raw.events {
        match event {
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } if *pressed => {
                // Browser-like navigation: Cmd+Arrow (macOS) / Ctrl+Arrow (others)
                if !wants_keyboard_input
                    && (modifiers.ctrl || modifiers.command)
                    && !modifiers.shift
                    && !modifiers.alt
                {
                    match key {
                        egui::Key::ArrowLeft | egui::Key::H => {
                            columns.get_selected_router().go_back();
                            continue;
                        }
                        egui::Key::ArrowRight | egui::Key::L => {
                            columns.get_selected_router().go_forward();
                            continue;
                        }
                        _ => {}
                    }
                }

                match key {
                    egui::Key::J => {
                        //columns.select_down();
                        {}
                    }
                    /*
                    egui::Key::K => {
                        columns.select_up();
                    }
                    egui::Key::H => {
                        columns.select_left();
                    }
                    egui::Key::L => {
                        columns.select_left();
                    }
                    */
                    egui::Key::BrowserBack | egui::Key::Escape => {
                        columns.get_selected_router().go_back();
                    }
                    _ => {}
                }
            }

            egui::Event::PointerButton {
                button: egui::PointerButton::Extra1,
                pressed: true,
                ..
            } => {
                if let Some(col_idx) = hovered_column {
                    columns.column_mut(col_idx).router_mut().go_back();
                } else {
                    columns.get_selected_router().go_back();
                }
            }

            egui::Event::PointerButton {
                button: egui::PointerButton::Extra2,
                pressed: true,
                ..
            } => {
                if let Some(col_idx) = hovered_column {
                    columns.column_mut(col_idx).router_mut().go_forward();
                } else {
                    columns.get_selected_router().go_forward();
                }
            }

            egui::Event::InsetsChanged => {
                tracing::debug!("insets have changed!");
            }

            _ => {}
        }
    }
}

#[profiling::function]
fn try_process_event(
    damus: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ctx: &egui::Context,
) -> Result<()> {
    let current_columns =
        get_active_columns_mut(app_ctx.i18n, app_ctx.accounts, &mut damus.decks_cache);
    let wants_keyboard_input = ctx.wants_keyboard_input();
    ctx.input(|i| {
        handle_egui_events(
            i,
            current_columns,
            damus.hovered_column,
            wants_keyboard_input,
        )
    });

    try_process_events_core(app_ctx, ctx, |app_ctx, ev| match (&ev.event).into() {
        RelayEvent::Opened => {
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            timeline::send_initial_timeline_filters(
                damus.options.contains(AppOptions::SinceOptimize),
                &mut damus.timeline_cache,
                &mut damus.subscriptions,
                app_ctx.legacy_pool,
                &ev.relay,
                app_ctx.accounts,
                &mut scoped_subs,
            );
        }
        RelayEvent::Message(msg) => {
            process_message(damus, app_ctx, &ev.relay, &msg);
        }
        _ => {}
    });

    for (kind, timeline) in &mut damus.timeline_cache {
        let selected_account_pk = *app_ctx.accounts.selected_account_pubkey();
        let is_ready = {
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            timeline::is_timeline_ready(app_ctx.ndb, &mut scoped_subs, timeline, app_ctx.accounts)
        };

        if is_ready {
            schedule_timeline_load(
                &damus.timeline_loader,
                &mut damus.inflight_timeline_loads,
                &damus.loaded_timeline_loads,
                app_ctx.ndb,
                kind,
                timeline,
                app_ctx.accounts.selected_account_pubkey(),
            );
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            // only thread timelines are reversed
            let reversed = false;

            if let Err(err) = timeline.poll_notes_into_view(
                &selected_account_pk,
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
            match kind {
                TimelineKind::List(ListKind::Contact(_))
                | TimelineKind::Algo(timeline::kind::AlgoTimeline::LastPerPubkey(
                    ListKind::Contact(_),
                )) => {
                    timeline::fetch_contact_list(
                        &mut damus.subscriptions,
                        timeline,
                        app_ctx.accounts,
                    );
                }
                TimelineKind::List(ListKind::PeopleList(plr))
                | TimelineKind::Algo(timeline::kind::AlgoTimeline::LastPerPubkey(
                    ListKind::PeopleList(plr),
                )) => {
                    let plr = plr.clone();
                    for relay in &mut app_ctx.legacy_pool.relays {
                        timeline::fetch_people_list(
                            &mut damus.subscriptions,
                            relay,
                            timeline,
                            &plr,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(follow_packs) = damus.onboarding.get_follow_packs_mut() {
        follow_packs.poll_for_notes(app_ctx.ndb, app_ctx.unknown_ids);
    }

    Ok(())
}

/// Schedule an initial timeline load if it is not already in-flight or complete.
fn schedule_timeline_load(
    loader: &TimelineLoader,
    inflight: &mut HashSet<TimelineKind>,
    loaded: &HashSet<TimelineKind>,
    ndb: &nostrdb::Ndb,
    kind: &TimelineKind,
    timeline: &mut timeline::Timeline,
    account_pk: &Pubkey,
) {
    if loaded.contains(kind) || inflight.contains(kind) {
        return;
    }

    let Some(filter) = timeline.filter.get_any_ready().cloned() else {
        return;
    };

    if timeline.kind.should_subscribe_locally() {
        timeline
            .subscription
            .try_add_local(*account_pk, ndb, &filter);
    }

    loader.load_timeline(kind.clone());
    inflight.insert(kind.clone());
}

/// Drain timeline loader messages and apply them to the timeline cache.
#[profiling::function]
fn handle_timeline_loader_messages(damus: &mut Damus, app_ctx: &mut AppContext<'_>) {
    let mut handled = 0;
    while handled < MAX_TIMELINE_LOADER_MSGS_PER_FRAME {
        let Some(msg) = damus.timeline_loader.try_recv() else {
            break;
        };
        handled += 1;

        match msg {
            TimelineLoaderMsg::TimelineBatch { kind, notes } => {
                let Some(timeline) = damus.timeline_cache.get_mut(&kind) else {
                    warn!("timeline loader batch for missing timeline {:?}", kind);
                    continue;
                };
                let txn = Transaction::new(app_ctx.ndb).expect("txn");
                if let Some(pks) =
                    timeline.insert_new(&txn, app_ctx.ndb, app_ctx.note_cache, &notes)
                {
                    pks.process(app_ctx.ndb, &txn, app_ctx.unknown_ids);
                }
            }
            TimelineLoaderMsg::TimelineFinished { kind } => {
                damus.inflight_timeline_loads.remove(&kind);
                damus.loaded_timeline_loads.insert(kind);
            }
            TimelineLoaderMsg::Failed { kind, error } => {
                warn!("timeline loader failed for {:?}: {}", kind, error);
                damus.inflight_timeline_loads.remove(&kind);
            }
        }
    }
}

#[profiling::function]
fn update_damus(damus: &mut Damus, app_ctx: &mut AppContext<'_>, ctx: &egui::Context) {
    app_ctx.img_cache.urls.cache.handle_io();

    damus
        .timeline_loader
        .start(ctx.clone(), app_ctx.ndb.clone());

    if damus.columns(app_ctx.accounts).columns().is_empty() {
        damus
            .columns_mut(app_ctx.i18n, app_ctx.accounts)
            .new_column_picker();
    }

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
                app_ctx.unknown_ids,
                *app_ctx.accounts.selected_account_pubkey(),
            ) {
                warn!("update_damus init: {err}");
            }

            if !app_ctx.settings.welcome_completed() {
                let split =
                    egui_nav::Split::PercentFromTop(egui_nav::Percent::new(40).expect("40 <= 100"));
                if let Some(col) = damus
                    .decks_cache
                    .selected_column_mut(app_ctx.i18n, app_ctx.accounts)
                {
                    col.sheet_router.route_to(Route::Welcome, split);
                }
            } else if is_compiled_as_mobile() && !app_ctx.settings.tos_accepted() {
                damus
                    .columns_mut(app_ctx.i18n, app_ctx.accounts)
                    .get_selected_router()
                    .route_to(Route::TosAcceptance);
            }
        }

        DamusState::Initialized => (),
    };

    handle_timeline_loader_messages(damus, app_ctx);

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
            ctx.legacy_pool.send_to(&msg, relay_url);
        }

        SubKind::FetchingContactList(timeline_uid) => {
            let timeline = if let Some(tl) = timeline_cache.get_mut(timeline_uid) {
                tl
            } else {
                error!(
                    "timeline uid:{:?} not found for FetchingContactList",
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
                notedeck::filter::FetchingRemoteType::PeopleList => {
                    FilterState::GotRemote(notedeck::filter::GotRemoteType::PeopleList)
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
    let RelayMessage::Eose(sid) = msg else {
        return;
    };

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

fn render_damus(damus: &mut Damus, app_ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
    damus
        .note_options
        .set(NoteOptions::Wide, is_narrow(ui.ctx()));

    let app_resp = if notedeck::ui::is_narrow(ui.ctx()) {
        render_damus_mobile(damus, app_ctx, ui)
    } else {
        render_damus_desktop(damus, app_ctx, ui)
    };

    fullscreen_media_viewer_ui(
        ui,
        &mut damus.view_state.media_viewer,
        app_ctx.img_cache,
        app_ctx.media_jobs.sender(),
    );

    // We use this for keeping timestamps and things up to date
    //ui.ctx().request_repaint_after(Duration::from_secs(5));

    app_resp
}

/// Present a fullscreen media viewer if the FullscreenMedia AppOptions flag is set. This is
/// typically set by image carousels using a MediaAction's on_view_media callback when
/// an image is clicked
fn fullscreen_media_viewer_ui(
    ui: &mut egui::Ui,
    state: &mut MediaViewerState,
    img_cache: &mut Images,
    jobs: &MediaJobSender,
) {
    if !state.should_show(ui) {
        if state.scene_rect.is_some() {
            // if we shouldn't show yet we will have a scene
            // rect, then we should clear it for next time
            tracing::debug!("fullscreen_media_viewer_ui: resetting scene rect");
            state.scene_rect = None;
        }
        return;
    }

    let resp = MediaViewer::new(state)
        .fullscreen(true)
        .ui(img_cache, jobs, ui);

    if resp.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        fullscreen_media_close(state);
    }
}

/// Close the fullscreen media player. This also resets the scene_rect state
fn fullscreen_media_close(state: &mut MediaViewerState) {
    state.flags.set(MediaViewerFlags::Open, false);
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
    pub fn new(app_context: &mut AppContext<'_>, args: &[String]) -> Self {
        // arg parsing

        let (parsed_args, unrecognized_args) =
            ColumnsArgs::parse(args, Some(app_context.accounts.selected_account_pubkey()));

        let account = app_context.accounts.selected_account_pubkey_bytes();

        let mut timeline_cache = TimelineCache::default();
        let mut options = AppOptions::default();
        let tmp_columns = !parsed_args.columns.is_empty();
        options.set(AppOptions::TmpColumns, tmp_columns);
        options.set(
            AppOptions::Debug,
            app_context.args.options.contains(NotedeckOptions::Debug),
        );
        options.set(
            AppOptions::SinceOptimize,
            parsed_args.is_flag_set(ColumnsFlag::SinceOptimize),
        );

        let decks_cache = if tmp_columns {
            info!("DecksCache: loading from command line arguments");
            let mut columns: Columns = Columns::new();
            let txn = Transaction::new(app_context.ndb).unwrap();
            for col in &parsed_args.columns {
                let timeline_kind = col.clone().into_timeline_kind();
                let mut scoped_subs = app_context.remote.scoped_subs(app_context.accounts);
                if let Some(add_result) = columns.add_new_timeline_column(
                    &mut timeline_cache,
                    &txn,
                    app_context.ndb,
                    app_context.note_cache,
                    &mut scoped_subs,
                    &timeline_kind,
                    *app_context.accounts.selected_account_pubkey(),
                ) {
                    add_result.process(
                        app_context.ndb,
                        app_context.note_cache,
                        &txn,
                        &mut timeline_cache,
                        app_context.unknown_ids,
                    );
                }
            }

            columns_to_decks_cache(app_context.i18n, columns, account)
        } else if let Some(decks_cache) = crate::storage::load_decks_cache(
            app_context.path,
            app_context.ndb,
            &mut timeline_cache,
            app_context.i18n,
        ) {
            info!(
                "DecksCache: loading from disk {}",
                crate::storage::DECKS_CACHE_FILE
            );
            decks_cache
        } else {
            info!("DecksCache: creating new with demo configuration");
            DecksCache::new_with_demo_config(&mut timeline_cache, app_context)
            //for (pk, _) in &app_context.accounts.cache {
            //    cache.add_deck_default(*pk);
            //}
        };

        let support = Support::new(app_context.path);
        let note_options = get_note_options(parsed_args, app_context.settings);
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
            threads,
            onboarding: Onboarding::default(),
            hovered_column: None,
            timeline_loader: TimelineLoader::default(),
            inflight_timeline_loads: HashSet::new(),
            loaded_timeline_loads: HashSet::new(),
        }
    }

    /// Scroll to the top of the currently selected column. This is called
    /// by the chrome when you click the toolbar
    pub fn scroll_to_top(&mut self) {
        self.options.insert(AppOptions::ScrollToTop)
    }

    pub fn columns_mut(&mut self, i18n: &mut Localization, accounts: &Accounts) -> &mut Columns {
        get_active_columns_mut(i18n, accounts, &mut self.decks_cache)
    }

    pub fn columns(&self, accounts: &Accounts) -> &Columns {
        get_active_columns(accounts, &self.decks_cache)
    }

    pub fn gen_subid(&self, kind: &SubKind) -> String {
        if self.options.contains(AppOptions::Debug) {
            format!("{kind:?}")
        } else {
            Uuid::new_v4().to_string()
        }
    }

    pub fn mock<P: AsRef<Path>>(data_path: P) -> Self {
        let mut i18n = Localization::default();
        let decks_cache = DecksCache::default_decks_cache(&mut i18n);

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
            threads: Threads::default(),
            onboarding: Onboarding::default(),
            hovered_column: None,
            timeline_loader: TimelineLoader::default(),
            inflight_timeline_loads: HashSet::new(),
            loaded_timeline_loads: HashSet::new(),
        }
    }

    pub fn subscriptions(&mut self) -> &mut HashMap<String, SubKind> {
        &mut self.subscriptions.subs
    }

    pub fn unrecognized_args(&self) -> &BTreeSet<String> {
        &self.unrecognized_args
    }

    pub fn toolbar_height() -> f32 {
        48.0
    }

    pub fn initially_selected_toolbar_index() -> i32 {
        0
    }
}

fn get_note_options(args: ColumnsArgs, settings_handler: &mut SettingsHandler) -> NoteOptions {
    let mut note_options = NoteOptions::default();

    note_options.set(
        NoteOptions::Textmode,
        args.is_flag_set(ColumnsFlag::Textmode),
    );
    note_options.set(
        NoteOptions::ScrambleText,
        args.is_flag_set(ColumnsFlag::Scramble),
    );
    note_options.set(
        NoteOptions::HideMedia,
        args.is_flag_set(ColumnsFlag::NoMedia),
    );
    note_options.set(
        NoteOptions::RepliesNewestFirst,
        settings_handler.show_replies_newest_first(),
    );
    note_options
}

/*
fn circle_icon(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let stroke = ui.style().interact(&response).fg_stroke;
    let radius = egui::lerp(2.0..=3.0, openness);
    ui.painter()
        .circle_filled(response.rect.center(), radius, stroke.color);
}
*/

/// Logic that handles toolbar visibility
fn toolbar_visibility_height(skb_rect: Option<egui::Rect>, ui: &mut egui::Ui) -> f32 {
    // Auto-hide toolbar when scrolling down
    let toolbar_visible_id = egui::Id::new("toolbar_visible");

    // Detect scroll direction using egui input state
    let scroll_delta = ui.ctx().input(|i| i.smooth_scroll_delta.y);
    let velocity_threshold = 1.0;

    // Update toolbar visibility based on scroll direction
    if scroll_delta > velocity_threshold {
        // Scrolling up (content moving down) - show toolbar
        ui.ctx()
            .data_mut(|d| d.insert_temp(toolbar_visible_id, true));
    } else if scroll_delta < -velocity_threshold {
        // Scrolling down (content moving up) - hide toolbar
        ui.ctx()
            .data_mut(|d| d.insert_temp(toolbar_visible_id, false));
    }

    let toolbar_visible = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(toolbar_visible_id))
        .unwrap_or(true); // Default to visible

    let toolbar_anim = ui
        .ctx()
        .animate_bool_responsive(toolbar_visible_id.with("anim"), toolbar_visible);

    if skb_rect.is_none() {
        Damus::toolbar_height() * toolbar_anim
    } else {
        0.0
    }
}

#[profiling::function]
fn render_damus_mobile(
    app: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> AppResponse {
    //let routes = app.timelines[0].routes.clone();

    let mut can_take_drag_from = Vec::new();
    let active_col = app.columns_mut(app_ctx.i18n, app_ctx.accounts).selected as usize;
    let mut app_action: Option<AppAction> = None;
    // don't show toolbar if soft keyboard is open
    let skb_rect = app_ctx.soft_keyboard_rect(
        ui.ctx().screen_rect(),
        notedeck::SoftKeyboardContext::platform(ui.ctx()),
    );

    let toolbar_height = toolbar_visibility_height(skb_rect, ui);
    StripBuilder::new(ui)
        .size(Size::remainder()) // top cell
        .size(Size::exact(toolbar_height)) // bottom cell
        .vertical(|mut strip| {
            strip.cell(|ui| {
                let rect = ui.available_rect_before_wrap();
                if !app.columns(app_ctx.accounts).columns().is_empty() {
                    let resp = nav::render_nav(
                        active_col,
                        ui.available_rect_before_wrap(),
                        app,
                        app_ctx,
                        ui,
                    );

                    can_take_drag_from.extend(resp.can_take_drag_from());

                    let r = resp.process_render_nav_response(app, app_ctx, ui);
                    if let Some(r) = r {
                        match r {
                            ProcessNavResult::SwitchOccurred => {
                                if !app.options.contains(AppOptions::TmpColumns) {
                                    storage::save_decks_cache(app_ctx.path, &app.decks_cache);
                                }
                            }

                            ProcessNavResult::PfpClicked => {
                                app_action = Some(AppAction::ToggleChrome);
                            }

                            ProcessNavResult::SwitchAccount(pubkey) => {
                                // Add as pubkey-only account if not already present
                                let kp = enostr::Keypair::only_pubkey(pubkey);
                                let _ = app_ctx.accounts.add_account(kp);

                                let txn = nostrdb::Transaction::new(app_ctx.ndb).expect("txn");
                                app_ctx.accounts.select_account(
                                    &pubkey,
                                    app_ctx.ndb,
                                    &txn,
                                    app_ctx.legacy_pool,
                                    ui.ctx(),
                                );
                            }

                            ProcessNavResult::ExternalNoteAction(note_action) => {
                                app_action = Some(AppAction::Note(note_action));
                            }
                        }
                    }
                }

                hovering_post_button(ui, app, app_ctx, rect);
            });

            strip.cell(|ui| 'brk: {
                if toolbar_height <= 0.0 {
                    break 'brk;
                }

                let unseen_notif = unseen_notification(app, app_ctx.accounts, active_col);

                if skb_rect.is_none() {
                    let resp = toolbar(ui, unseen_notif);
                    if let Some(action) = resp {
                        action.process(app, app_ctx);
                    }
                }
            });
        });

    AppResponse::action(app_action).drag(can_take_drag_from)
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

    rect.min.x = rect.max.x - (if is_narrow(ui.ctx()) { 60.0 } else { 100.0 } * button_y);
    rect.min.y = rect.max.y - 100.0;
    rect.max.x += 48.0 * (1.0 - button_y);

    let darkmode = ui.ctx().style().visuals.dark_mode;

    // only show the compose button on profile pages and on home
    let compose_resp = ui
        .put(rect, ui::post::compose_note_button(darkmode))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if compose_resp.clicked() && !app.columns(app_ctx.accounts).columns().is_empty() {
        // just use the some side panel logic as the desktop
        DesktopSidePanel::perform_action(
            &mut app.decks_cache,
            app_ctx.accounts,
            SidePanelAction::ComposeNote,
            app_ctx.i18n,
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
                    ListKind::PeopleList(_) => true,
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
        Route::Settings => false,
        Route::ComposeNote => false,
        Route::AddColumn(_) => false,
        Route::EditProfile(_) => false,
        Route::Support => false,
        Route::NewDeck => false,
        Route::Search => false,
        Route::EditDeck(_) => false,
        Route::Wallet(_) => false,
        Route::CustomizeZapAmount(_) => false,
        Route::RepostDecision(_) => false,
        Route::Following(_) => false,
        Route::FollowedBy(_) => false,
        Route::TosAcceptance => false,
        Route::Welcome => false,
        Route::Report(_) => false,
    }
}

#[profiling::function]
fn render_damus_desktop(
    app: &mut Damus,
    app_ctx: &mut AppContext<'_>,
    ui: &mut egui::Ui,
) -> AppResponse {
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
) -> AppResponse {
    let num_cols = get_active_columns(ctx.accounts, &app.decks_cache).num_columns();
    let mut side_panel_action: Option<nav::SwitchingAction> = None;
    let mut responses = Vec::with_capacity(num_cols);

    let mut can_take_drag_from = Vec::new();

    StripBuilder::new(ui)
        .size(Size::exact(ui::side_panel::SIDE_PANEL_WIDTH))
        .sizes(sizes, num_cols)
        .clip(true)
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                let rect = ui.available_rect_before_wrap();
                // Clone the route to avoid holding a borrow on app.decks_cache
                let current_route = get_active_columns(ctx.accounts, &app.decks_cache)
                    .selected()
                    .map(|col| col.router().top().clone());
                let side_panel = DesktopSidePanel::new(
                    ctx.accounts.get_selected_account(),
                    &app.decks_cache,
                    ctx.i18n,
                    ctx.ndb,
                    ctx.img_cache,
                    ctx.media_jobs.sender(),
                    current_route.as_ref(),
                    ctx.remote.relay_inspect(),
                )
                .show(ui);

                if let Some(side_panel) = side_panel {
                    if side_panel.response.clicked() || side_panel.response.secondary_clicked() {
                        if let Some(action) = DesktopSidePanel::perform_action(
                            &mut app.decks_cache,
                            ctx.accounts,
                            side_panel.action,
                            ctx.i18n,
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

            app.hovered_column = None;

            for col_index in 0..num_cols {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    let v_line_stroke = ui.visuals().widgets.noninteractive.bg_stroke;
                    let inner_rect = {
                        let mut inner = rect;
                        inner.set_right(rect.right() - v_line_stroke.width);
                        inner
                    };
                    let resp = nav::render_nav(col_index, inner_rect, app, ctx, ui);
                    can_take_drag_from.extend(resp.can_take_drag_from());
                    responses.push(resp);

                    // Track hovered column for mouse back/forward navigation
                    if ui.rect_contains_pointer(rect) {
                        app.hovered_column = Some(col_index);
                    }

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
            || action.process(
                &mut app.timeline_cache,
                &mut app.decks_cache,
                ctx,
                &mut app.subscriptions,
                ui.ctx(),
            );
    }

    let mut app_action: Option<AppAction> = None;

    for response in responses {
        let nav_result = response.process_render_nav_response(app, ctx, ui);

        if let Some(nr) = nav_result {
            match nr {
                ProcessNavResult::SwitchOccurred => save_cols = true,

                ProcessNavResult::PfpClicked => {
                    app_action = Some(AppAction::ToggleChrome);
                }

                ProcessNavResult::SwitchAccount(pubkey) => {
                    // Add as pubkey-only account if not already present
                    let kp = enostr::Keypair::only_pubkey(pubkey);
                    let _ = ctx.accounts.add_account(kp);

                    let txn = nostrdb::Transaction::new(ctx.ndb).expect("txn");
                    ctx.accounts
                        .select_account(&pubkey, ctx.ndb, &txn, ctx.legacy_pool, ui.ctx());
                }

                ProcessNavResult::ExternalNoteAction(note_action) => {
                    app_action = Some(AppAction::Note(note_action));
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

    AppResponse::action(app_action).drag(can_take_drag_from)
}

impl notedeck::App for Damus {
    #[profiling::function]
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
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
    i18n: &mut Localization,
    accounts: &Accounts,
    decks_cache: &'a mut DecksCache,
) -> &'a mut Columns {
    get_decks_mut(i18n, accounts, decks_cache)
        .active_mut()
        .columns_mut()
}

pub fn get_decks_mut<'a>(
    i18n: &mut Localization,
    accounts: &Accounts,
    decks_cache: &'a mut DecksCache,
) -> &'a mut Decks {
    decks_cache.decks_mut(i18n, accounts.selected_account_pubkey())
}

fn columns_to_decks_cache(i18n: &mut Localization, cols: Columns, key: &[u8; 32]) -> DecksCache {
    let mut account_to_decks: HashMap<Pubkey, Decks> = Default::default();
    let decks = Decks::new(crate::decks::Deck::new_with_columns(
        crate::decks::Deck::default_icon(),
        tr!(i18n, "My Deck", "Title for the user's deck"),
        cols,
    ));

    let account = Pubkey::new(*key);
    account_to_decks.insert(account, decks);
    DecksCache::new(account_to_decks, i18n)
}
