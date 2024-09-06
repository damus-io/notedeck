use crate::account_manager::AccountManager;
use crate::actionbar::BarResult;
use crate::app_creation::setup_cc;
use crate::app_style::user_requested_visuals_change;
use crate::args::Args;
use crate::column::{Column, ColumnKind, Columns};
use crate::draft::Drafts;
use crate::error::{Error, FilterError};
use crate::filter::FilterState;
use crate::frame_history::FrameHistory;
use crate::imgcache::ImageCache;
use crate::key_storage::KeyStorageType;
use crate::note::NoteRef;
use crate::notecache::{CachedNote, NoteCache};
use crate::relay_pool_manager::RelayPoolManager;
use crate::routable_widget_state::RoutableWidgetState;
use crate::route::{ManageAccountRoute, Route};
use crate::subscriptions::{SubKind, Subscriptions};
use crate::thread::{DecrementResult, Threads};
use crate::timeline::{Timeline, TimelineKind, TimelineSource, ViewFilter};
use crate::ui::note::PostAction;
use crate::ui::{self, AccountSelectionWidget};
use crate::ui::{DesktopSidePanel, RelayView, View};
use crate::unknowns::UnknownIds;
use crate::{filter, Result};
use egui_nav::{Nav, NavAction};
use enostr::{ClientMessage, RelayEvent, RelayMessage, RelayPool};
use uuid::Uuid;

use egui::{Context, Frame, Style};
use egui_extras::{Size, StripBuilder};

use nostrdb::{Config, Filter, Ndb, Note, Transaction};

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum DamusState {
    Initializing,
    Initialized,
}

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
pub struct Damus {
    state: DamusState,
    note_cache: NoteCache,
    pub pool: RelayPool,

    pub columns: Columns,
    pub account_management_view_state: RoutableWidgetState<ManageAccountRoute>,
    pub ndb: Ndb,
    pub unknown_ids: UnknownIds,
    pub drafts: Drafts,
    pub threads: Threads,
    pub img_cache: ImageCache,
    pub accounts: AccountManager,
    pub subscriptions: Subscriptions,

    frame_history: crate::frame_history::FrameHistory,

    // TODO: make these bitflags
    pub debug: bool,
    pub since_optimize: bool,
    pub textmode: bool,
    pub show_account_switcher: bool,
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

fn send_initial_timeline_filter(
    ndb: &Ndb,
    can_since_optimize: bool,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    timeline: &mut Timeline,
    to: &str,
) {
    let filter_state = timeline.filter.clone();

    match filter_state {
        FilterState::Broken(err) => {
            error!(
                "FetchingRemote state in broken state when sending initial timeline filter? {err}"
            );
        }

        FilterState::FetchingRemote(_unisub) => {
            error!("FetchingRemote state when sending initial timeline filter?");
        }

        FilterState::GotRemote(_sub) => {
            error!("GotRemote state when sending initial timeline filter?");
        }

        FilterState::Ready(filter) => {
            let filter = filter.to_owned();
            let new_filters = filter.into_iter().map(|f| {
                // limit the size of remote filters
                let default_limit = filter::default_remote_limit();
                let mut lim = f.limit().unwrap_or(default_limit);
                let mut filter = f;
                if lim > default_limit {
                    lim = default_limit;
                    filter = filter.limit_mut(lim);
                }

                let notes = timeline.notes(ViewFilter::NotesAndReplies);

                // Should we since optimize? Not always. For example
                // if we only have a few notes locally. One way to
                // determine this is by looking at the current filter
                // and seeing what its limit is. If we have less
                // notes than the limit, we might want to backfill
                // older notes
                if can_since_optimize && filter::should_since_optimize(lim, notes.len()) {
                    filter = filter::since_optimize_filter(filter, notes);
                } else {
                    warn!("Skipping since optimization for {:?}: number of local notes is less than limit, attempting to backfill.", filter);
                }

                filter
            }).collect();

            //let sub_id = damus.gen_subid(&SubKind::Initial);
            let sub_id = Uuid::new_v4().to_string();
            subs.subs.insert(sub_id.clone(), SubKind::Initial);

            let cmd = ClientMessage::req(sub_id, new_filters);
            pool.send_to(&cmd, to);
        }

        // we need some data first
        FilterState::NeedsRemote(filter) => {
            let sub_kind = SubKind::FetchingContactList(timeline.id);
            //let sub_id = damus.gen_subid(&sub_kind);
            let sub_id = Uuid::new_v4().to_string();
            let local_sub = ndb.subscribe(&filter).expect("sub");

            timeline.filter = FilterState::fetching_remote(sub_id.clone(), local_sub);

            subs.subs.insert(sub_id.clone(), sub_kind);

            pool.subscribe(sub_id, filter.to_owned());
        }
    }
}

fn send_initial_filters(damus: &mut Damus, relay_url: &str) {
    info!("Sending initial filters to {}", relay_url);
    for timeline in damus.columns.timelines_mut() {
        send_initial_timeline_filter(
            &damus.ndb,
            damus.since_optimize,
            &mut damus.subscriptions,
            &mut damus.pool,
            timeline,
            relay_url,
        );
    }
}

enum ContextAction {
    SetPixelsPerPoint(f32),
}

fn handle_key_events(
    input: &egui::InputState,
    pixels_per_point: f32,
    columns: &mut Columns,
) -> Option<ContextAction> {
    let amount = 0.2;

    // We can't do things like setting the pixels_per_point when we are holding
    // on to an locked InputState context, so we need to pass actions externally
    let mut context_action: Option<ContextAction> = None;

    for event in &input.raw.events {
        if let egui::Event::Key {
            key, pressed: true, ..
        } = event
        {
            match key {
                egui::Key::Equals => {
                    context_action =
                        Some(ContextAction::SetPixelsPerPoint(pixels_per_point + amount));
                }
                egui::Key::Minus => {
                    context_action =
                        Some(ContextAction::SetPixelsPerPoint(pixels_per_point - amount));
                }
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

    context_action
}

fn try_process_event(damus: &mut Damus, ctx: &egui::Context) -> Result<()> {
    let ppp = ctx.pixels_per_point();
    let res = ctx.input(|i| handle_key_events(i, ppp, &mut damus.columns));
    if let Some(action) = res {
        match action {
            ContextAction::SetPixelsPerPoint(amt) => {
                ctx.set_pixels_per_point(amt);
            }
        }
    }

    let ctx2 = ctx.clone();
    let wakeup = move || {
        ctx2.request_repaint();
    };
    damus.pool.keepalive_ping(wakeup);

    // pool stuff
    while let Some(ev) = damus.pool.try_recv() {
        let relay = ev.relay.to_owned();

        match (&ev.event).into() {
            RelayEvent::Opened => send_initial_filters(damus, &relay),
            // TODO: handle reconnects
            RelayEvent::Closed => warn!("{} connection closed", &relay),
            RelayEvent::Error(e) => error!("{}: {}", &relay, e),
            RelayEvent::Other(msg) => trace!("other event {:?}", &msg),
            RelayEvent::Message(msg) => process_message(damus, &relay, &msg),
        }
    }

    let n_cols = damus.columns.columns().len();
    for col_ind in 0..n_cols {
        let timeline =
            if let ColumnKind::Timeline(timeline) = damus.columns.column_mut(col_ind).kind_mut() {
                timeline
            } else {
                continue;
            };

        if let Ok(true) =
            is_timeline_ready(&damus.ndb, &mut damus.pool, &mut damus.note_cache, timeline)
        {
            let txn = Transaction::new(&damus.ndb).expect("txn");
            if let Err(err) = TimelineSource::column(timeline.id).poll_notes_into_view(
                &txn,
                &damus.ndb,
                &mut damus.columns,
                &mut damus.threads,
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

/// Check our timeline filter and see if we have any filter data ready.
/// Our timelines may require additional data before it is functional. For
/// example, when we have to fetch a contact list before we do the actual
/// following list query.
fn is_timeline_ready(
    ndb: &Ndb,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    timeline: &mut Timeline,
) -> Result<bool> {
    let sub = match &timeline.filter {
        FilterState::GotRemote(sub) => *sub,
        FilterState::Ready(_f) => return Ok(true),
        _ => return Ok(false),
    };

    // We got at least one eose for our filter request. Let's see
    // if nostrdb is done processing it yet.
    let res = ndb.poll_for_notes(sub, 1);
    if res.is_empty() {
        debug!(
            "check_timeline_filter_state: no notes found (yet?) for timeline {:?}",
            timeline
        );
        return Ok(false);
    }

    info!("notes found for contact timeline after GotRemote!");

    let note_key = res[0];

    let filter = {
        let txn = Transaction::new(ndb).expect("txn");
        let note = ndb.get_note_by_key(&txn, note_key).expect("note");
        filter::filter_from_tags(&note).map(|f| f.into_follow_filter())
    };

    // TODO: into_follow_filter is hardcoded to contact lists, let's generalize
    match filter {
        Err(Error::Filter(e)) => {
            error!("got broken when building filter {e}");
            timeline.filter = FilterState::broken(e);
        }
        Err(err) => {
            error!("got broken when building filter {err}");
            timeline.filter = FilterState::broken(FilterError::EmptyContactList);
            return Err(err);
        }
        Ok(filter) => {
            // we just switched to the ready state, we should send initial
            // queries and setup the local subscription
            info!("Found contact list! Setting up local and remote contact list query");
            setup_initial_timeline(ndb, timeline, note_cache, &filter).expect("setup init");
            timeline.filter = FilterState::ready(filter.clone());

            //let ck = &timeline.kind;
            //let subid = damus.gen_subid(&SubKind::Column(ck.clone()));
            let subid = Uuid::new_v4().to_string();
            pool.subscribe(subid, filter)
        }
    }

    Ok(true)
}

#[cfg(feature = "profiling")]
fn setup_profiling() {
    puffin::set_scopes_on(true); // tell puffin to collect data
}

fn setup_initial_timeline(
    ndb: &Ndb,
    timeline: &mut Timeline,
    note_cache: &mut NoteCache,
    filters: &[Filter],
) -> Result<()> {
    timeline.subscription = Some(ndb.subscribe(filters)?);
    let txn = Transaction::new(ndb)?;
    debug!(
        "querying nostrdb sub {:?} {:?}",
        timeline.subscription, timeline.filter
    );
    let lim = filters[0].limit().unwrap_or(crate::filter::default_limit()) as i32;
    let results = ndb.query(&txn, filters, lim)?;

    let filters = {
        let views = &timeline.views;
        let filters: Vec<fn(&CachedNote, &Note) -> bool> =
            views.iter().map(|v| v.filter.filter()).collect();
        filters
    };

    for result in results {
        for (view, filter) in filters.iter().enumerate() {
            if filter(
                note_cache.cached_note_or_insert_mut(result.note_key, &result.note),
                &result.note,
            ) {
                timeline.views[view].notes.push(NoteRef {
                    key: result.note_key,
                    created_at: result.note.created_at(),
                })
            }
        }
    }

    Ok(())
}

fn setup_initial_nostrdb_subs(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    columns: &mut Columns,
) -> Result<()> {
    for timeline in columns.timelines_mut() {
        match &timeline.filter {
            FilterState::Ready(filters) => {
                { setup_initial_timeline(ndb, timeline, note_cache, &filters.clone()) }?
            }

            FilterState::Broken(err) => {
                error!("FetchingRemote state broken in setup_initial_nostr_subs: {err}")
            }
            FilterState::FetchingRemote(_) => {
                error!("FetchingRemote state in setup_initial_nostr_subs")
            }
            FilterState::GotRemote(_) => {
                error!("GotRemote state in setup_initial_nostr_subs")
            }
            FilterState::NeedsRemote(_filters) => {
                // can't do anything yet, we defer to first connect to send
                // remote filters
            }
        }
    }

    Ok(())
}

fn update_damus(damus: &mut Damus, ctx: &egui::Context) {
    if damus.state == DamusState::Initializing {
        #[cfg(feature = "profiling")]
        setup_profiling();

        damus.state = DamusState::Initialized;
        // this lets our eose handler know to close unknownids right away
        damus
            .subscriptions()
            .insert("unknownids".to_string(), SubKind::OneShot);
        setup_initial_nostrdb_subs(&damus.ndb, &mut damus.note_cache, &mut damus.columns)
            .expect("home subscription failed");
    }

    if let Err(err) = try_process_event(damus, ctx) {
        error!("error processing event: {}", err);
    }
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
                &damus.columns,
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
            let timeline = if let Some(tl) = damus.columns.find_timeline_mut(timeline_uid) {
                tl
            } else {
                error!(
                    "timeline uid:{} not found for FetchingContactList",
                    timeline_uid
                );
                return Ok(());
            };

            // If this request was fetching a contact list, our filter
            // state should be "FetchingRemote". We look at the local
            // subscription for that filter state and get the subscription id
            let local_sub = if let FilterState::FetchingRemote(unisub) = &timeline.filter {
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

            // We take the subscription id and pass it to the new state of
            // "GotRemote". This will let future frames know that it can try
            // to look for the contact list in nostrdb.
            timeline.filter = FilterState::got_remote(local_sub);
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
    pub fn new<P: AsRef<Path>>(
        cc: &eframe::CreationContext<'_>,
        data_path: P,
        args: Vec<String>,
    ) -> Self {
        // arg parsing
        let parsed_args = Args::parse(&args);
        let is_mobile = parsed_args.is_mobile.unwrap_or(ui::is_compiled_as_mobile());

        setup_cc(cc, is_mobile, parsed_args.light);

        let data_path = parsed_args
            .datapath
            .unwrap_or(data_path.as_ref().to_str().expect("db path ok").to_string());
        let dbpath = parsed_args.dbpath.unwrap_or(data_path.clone());

        let _ = std::fs::create_dir_all(dbpath.clone());

        let imgcache_dir = format!("{}/{}", data_path, ImageCache::rel_datadir());
        let _ = std::fs::create_dir_all(imgcache_dir.clone());

        let mut config = Config::new();
        config.set_ingester_threads(4);

        let mut accounts = AccountManager::new(
            // TODO: should pull this from settings
            None,
            // TODO: use correct KeyStorage mechanism for current OS arch
            KeyStorageType::None,
        );

        for key in parsed_args.keys {
            info!("adding account: {}", key.pubkey);
            accounts.add_account(key);
        }

        // TODO: pull currently selected account from settings
        if accounts.num_accounts() > 0 {
            accounts.select_account(0);
        }

        // setup relays if we have them
        let pool = if parsed_args.relays.is_empty() {
            let mut pool = RelayPool::new();
            relay_setup(&mut pool, &cc.egui_ctx);
            pool
        } else {
            let ctx = cc.egui_ctx.clone();
            let wakeup = move || {
                ctx.request_repaint();
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
        let ndb = Ndb::new(&dbpath, &config).expect("ndb");

        let mut columns: Vec<Column> = Vec::with_capacity(parsed_args.columns.len());
        for col in parsed_args.columns {
            if let Some(timeline) = col.into_timeline(&ndb, account) {
                columns.push(Column::timeline(timeline));
            }
        }

        let debug = parsed_args.debug;

        if columns.is_empty() {
            let filter = Filter::from_json(include_str!("../queries/timeline.json")).unwrap();
            columns.push(Column::timeline(Timeline::new(
                TimelineKind::Generic,
                FilterState::ready(vec![filter]),
            )));
        }

        Self {
            pool,
            debug,
            unknown_ids: UnknownIds::default(),
            subscriptions: Subscriptions::default(),
            since_optimize: parsed_args.since_optimize,
            threads: Threads::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            img_cache: ImageCache::new(imgcache_dir.into()),
            note_cache: NoteCache::default(),
            columns: Columns::new(columns),
            textmode: parsed_args.textmode,
            ndb,
            accounts,
            frame_history: FrameHistory::default(),
            show_account_switcher: false,
            account_management_view_state: RoutableWidgetState::default(),
        }
    }

    pub fn gen_subid(&self, kind: &SubKind) -> String {
        if self.debug {
            format!("{:?}", kind)
        } else {
            Uuid::new_v4().to_string()
        }
    }

    pub fn mock<P: AsRef<Path>>(data_path: P) -> Self {
        let mut columns: Vec<Column> = vec![];
        let filter = Filter::from_json(include_str!("../queries/global.json")).unwrap();
        columns.push(Column::timeline(Timeline::new(
            TimelineKind::Universe,
            FilterState::ready(vec![filter]),
        )));

        let imgcache_dir = data_path.as_ref().join(ImageCache::rel_datadir());
        let _ = std::fs::create_dir_all(imgcache_dir.clone());
        let debug = true;

        let mut config = Config::new();
        config.set_ingester_threads(2);
        Self {
            debug,
            unknown_ids: UnknownIds::default(),
            subscriptions: Subscriptions::default(),
            since_optimize: true,
            threads: Threads::default(),
            drafts: Drafts::default(),
            state: DamusState::Initializing,
            pool: RelayPool::new(),
            img_cache: ImageCache::new(imgcache_dir),
            note_cache: NoteCache::default(),
            columns: Columns::new(columns),
            textmode: false,
            ndb: Ndb::new(data_path.as_ref().to_str().expect("db path ok"), &config).expect("ndb"),
            accounts: AccountManager::new(None, KeyStorageType::None),
            frame_history: FrameHistory::default(),
            show_account_switcher: false,
            account_management_view_state: RoutableWidgetState::default(),
        }
    }

    pub fn subscriptions(&mut self) -> &mut HashMap<String, SubKind> {
        &mut self.subscriptions.subs
    }

    pub fn note_cache_mut(&mut self) -> &mut NoteCache {
        &mut self.note_cache
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

/// Local thread unsubscribe
fn thread_unsubscribe(
    ndb: &Ndb,
    threads: &mut Threads,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    id: &[u8; 32],
) {
    let (unsubscribe, remote_subid) = {
        let txn = Transaction::new(ndb).expect("txn");
        let root_id = crate::note::root_note_id_from_selected_id(ndb, note_cache, &txn, id);

        let thread = threads.thread_mut(ndb, &txn, root_id).get_ptr();
        let unsub = thread.decrement_sub();

        let mut remote_subid: Option<String> = None;
        if let Ok(DecrementResult::LastSubscriber(_subid)) = unsub {
            *thread.subscription_mut() = None;
            remote_subid = thread.remote_subscription().to_owned();
            *thread.remote_subscription_mut() = None;
        }

        (unsub, remote_subid)
    };

    match unsubscribe {
        Ok(DecrementResult::LastSubscriber(sub)) => {
            if let Err(e) = ndb.unsubscribe(sub) {
                error!(
                    "failed to unsubscribe from thread: {e}, subid:{}, {} active subscriptions",
                    sub.id(),
                    ndb.subscription_count()
                );
            } else {
                info!(
                    "Unsubscribed from thread subid:{}. {} active subscriptions",
                    sub.id(),
                    ndb.subscription_count()
                );
            }

            // unsub from remote
            if let Some(subid) = remote_subid {
                pool.unsubscribe(subid);
            }
        }

        Ok(DecrementResult::ActiveSubscribers) => {
            info!(
                "Keeping thread subscription. {} active subscriptions.",
                ndb.subscription_count()
            );
            // do nothing
        }

        Err(e) => {
            // something is wrong!
            error!(
                "Thread unsubscribe error: {e}. {} active subsciptions.",
                ndb.subscription_count()
            );
        }
    }
}

fn render_nav(show_postbox: bool, col: usize, app: &mut Damus, ui: &mut egui::Ui) {
    let navigating = app.columns.column(col).navigating;
    let returning = app.columns.column(col).returning;

    let nav_response = Nav::new(app.columns.column(col).routes().to_vec())
        .navigating(navigating)
        .returning(returning)
        .title(false)
        .show_mut(ui, |ui, nav| match nav.top() {
            Route::Timeline(_n) => {
                let column = app.columns.column_mut(col);
                if column.kind().timeline().is_some() {
                    if show_postbox {
                        if let Some(kp) = app.accounts.selected_or_first_nsec() {
                            ui::timeline::postbox_view(
                                &app.ndb,
                                kp,
                                &mut app.pool,
                                &mut app.drafts,
                                &mut app.img_cache,
                                ui,
                            );
                        }
                    }
                    ui::TimelineView::new(
                        &app.ndb,
                        column,
                        &mut app.note_cache,
                        &mut app.img_cache,
                        &mut app.threads,
                        &mut app.pool,
                        app.textmode,
                    )
                    .ui(ui);
                } else {
                    ui.label("no timeline for this column?");
                }
                None
            }

            Route::Relays => {
                let manager = RelayPoolManager::new(&mut app.pool);
                RelayView::new(manager).ui(ui);
                None
            }

            Route::Thread(id) => {
                let result = ui::ThreadView::new(
                    col,
                    &mut app.columns,
                    &mut app.threads,
                    &app.ndb,
                    &mut app.note_cache,
                    &mut app.img_cache,
                    &mut app.unknown_ids,
                    &mut app.pool,
                    app.textmode,
                    id.bytes(),
                )
                .ui(ui);

                if let Some(bar_result) = result {
                    match bar_result {
                        BarResult::NewThreadNotes(new_notes) => {
                            let thread = app.threads.thread_expected_mut(new_notes.root_id.bytes());
                            new_notes.process(thread);
                        }
                    }
                }

                None
            }

            Route::Reply(id) => {
                let txn = if let Ok(txn) = Transaction::new(&app.ndb) {
                    txn
                } else {
                    ui.label("Reply to unknown note");
                    return None;
                };

                let note = if let Ok(note) = app.ndb.get_note_by_id(&txn, id.bytes()) {
                    note
                } else {
                    ui.label("Reply to unknown note");
                    return None;
                };

                let id = egui::Id::new((
                    "post",
                    app.columns.column(col).view_id(),
                    note.key().unwrap(),
                ));

                if let Some(poster) = app.accounts.selected_or_first_nsec() {
                    let response = egui::ScrollArea::vertical().show(ui, |ui| {
                        ui::PostReplyView::new(
                            &app.ndb,
                            poster,
                            &mut app.pool,
                            &mut app.drafts,
                            &mut app.note_cache,
                            &mut app.img_cache,
                            &note,
                        )
                        .id_source(id)
                        .show(ui)
                    });

                    Some(response)
                } else {
                    None
                }
            }
        });

    let column = app.columns.column_mut(col);
    if let Some(reply_response) = nav_response.inner {
        if let Some(PostAction::Post(_np)) = reply_response.inner.action {
            column.returning = true;
        }
    }

    if let Some(NavAction::Returned) = nav_response.action {
        let popped = column.routes_mut().pop();
        if let Some(Route::Thread(id)) = popped {
            thread_unsubscribe(
                &app.ndb,
                &mut app.threads,
                &mut app.pool,
                &mut app.note_cache,
                id.bytes(),
            );
        }
        column.returning = false;
    } else if let Some(NavAction::Navigated) = nav_response.action {
        column.navigating = false;
    }
}

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    //render_panel(ctx, app, 0);

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //let routes = app.timelines[0].routes.clone();

    main_panel(&ctx.style(), ui::is_narrow(ctx)).show(ctx, |ui| {
        if !app.columns.columns().is_empty() {
            render_nav(false, 0, app, ui);
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
    let calc_panel_width = (screen_size / app.columns.columns().len() as f32) - 30.0;
    let min_width = 320.0;
    let need_scroll = calc_panel_width < min_width;
    let panel_sizes = if need_scroll {
        Size::exact(min_width)
    } else {
        Size::remainder()
    };

    main_panel(&ctx.style(), ui::is_narrow(ctx)).show(ctx, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        AccountSelectionWidget::ui(app, ui);
        if need_scroll {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                timelines_view(ui, panel_sizes, app, app.columns.columns().len());
            });
        } else {
            timelines_view(ui, panel_sizes, app, app.columns.columns().len());
        }
    });
}

fn timelines_view(ui: &mut egui::Ui, sizes: Size, app: &mut Damus, columns: usize) {
    StripBuilder::new(ui)
        .size(Size::exact(40.0))
        .sizes(sizes, columns)
        .clip(true)
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                let rect = ui.available_rect_before_wrap();
                let side_panel = DesktopSidePanel::new(app).show(ui);

                if side_panel.response.clicked() {
                    info!("clicked {:?}", side_panel.action);
                }

                DesktopSidePanel::perform_action(app, side_panel.action);

                // vertical sidebar line
                ui.painter().vline(
                    rect.right(),
                    rect.y_range(),
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
            });

            let n_cols = app.columns.columns().len();
            let mut first = true;
            for column_ind in 0..n_cols {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    let show_postbox =
                        first && app.columns.column(column_ind).kind().timeline().is_some();
                    if show_postbox {
                        first = false
                    }
                    render_nav(show_postbox, column_ind, app, ui);

                    // vertical line
                    ui.painter().vline(
                        rect.right(),
                        rect.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                });

                //strip.cell(|ui| timeline::timeline_view(ui, app, timeline_ind));
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
