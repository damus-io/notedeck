use crate::account_manager::AccountManager;
use crate::actionbar::BarResult;
use crate::app_creation::setup_cc;
use crate::app_style::user_requested_visuals_change;
use crate::args::Args;
use crate::column::ColumnKind;
use crate::draft::Drafts;
use crate::error::{Error, FilterError};
use crate::filter::FilterState;
use crate::frame_history::FrameHistory;
use crate::imgcache::ImageCache;
use crate::key_storage::KeyStorageType;
use crate::note::NoteRef;
use crate::notecache::{CachedNote, NoteCache};
use crate::relay_pool_manager::RelayPoolManager;
use crate::route::Route;
use crate::subscriptions::{SubKind, Subscriptions};
use crate::thread::{DecrementResult, Threads};
use crate::timeline::{Timeline, TimelineSource, ViewFilter};
use crate::ui::note::PostAction;
use crate::ui::{self, AccountSelectionWidget, DesktopGlobalPopup};
use crate::ui::{DesktopSidePanel, RelayView, View};
use crate::unknowns::UnknownIds;
use crate::{filter, Result};
use egui_nav::{Nav, NavAction};
use enostr::{ClientMessage, RelayEvent, RelayMessage, RelayPool};
use std::cell::RefCell;
use std::rc::Rc;
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

    /// global navigation for account management popups, etc.
    pub global_nav: Vec<Route>,

    pub timelines: Vec<Timeline>,
    pub selected_timeline: i32,

    pub ndb: Ndb,
    pub unknown_ids: UnknownIds,
    pub drafts: Drafts,
    pub threads: Threads,
    pub img_cache: ImageCache,
    pub account_manager: AccountManager,
    pub subscriptions: Subscriptions,

    frame_history: crate::frame_history::FrameHistory,

    // TODO: make these bitflags
    pub debug: bool,
    pub since_optimize: bool,
    pub textmode: bool,
    pub show_account_switcher: bool,
    pub show_global_popup: bool,
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

fn send_initial_timeline_filter(damus: &mut Damus, timeline: usize, to: &str) {
    let can_since_optimize = damus.since_optimize;

    let filter_state = damus.timelines[timeline].filter.clone();

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

                let notes = damus.timelines[timeline].notes(ViewFilter::NotesAndReplies);

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

            let sub_id = damus.gen_subid(&SubKind::Initial);
            damus
                .subscriptions()
                .insert(sub_id.clone(), SubKind::Initial);

            let cmd = ClientMessage::req(sub_id, new_filters);
            damus.pool.send_to(&cmd, to);
        }

        // we need some data first
        FilterState::NeedsRemote(filter) => {
            let uid = damus.timelines[timeline].uid;
            let sub_kind = SubKind::FetchingContactList(uid);
            let sub_id = damus.gen_subid(&sub_kind);
            let local_sub = damus.ndb.subscribe(&filter).expect("sub");

            damus.timelines[timeline].filter =
                FilterState::fetching_remote(sub_id.clone(), local_sub);

            damus.subscriptions().insert(sub_id.clone(), sub_kind);

            damus.pool.subscribe(sub_id, filter.to_owned());
        }
    }
}

fn send_initial_filters(damus: &mut Damus, relay_url: &str) {
    info!("Sending initial filters to {}", relay_url);
    let timelines = damus.timelines.len();

    for i in 0..timelines {
        send_initial_timeline_filter(damus, i, relay_url);
    }
}

enum ContextAction {
    SetPixelsPerPoint(f32),
}

fn handle_key_events(
    input: &egui::InputState,
    pixels_per_point: f32,
    damus: &mut Damus,
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
                    damus.select_down();
                }
                egui::Key::K => {
                    damus.select_up();
                }
                egui::Key::H => {
                    damus.select_left();
                }
                egui::Key::L => {
                    damus.select_left();
                }
                _ => {}
            }
        }
    }

    context_action
}

fn try_process_event(damus: &mut Damus, ctx: &egui::Context) -> Result<()> {
    let ppp = ctx.pixels_per_point();
    let res = ctx.input(|i| handle_key_events(i, ppp, damus));
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

    for timeline in 0..damus.timelines.len() {
        let src = TimelineSource::column(timeline);

        if let Ok(true) = is_timeline_ready(damus, timeline) {
            let txn = Transaction::new(&damus.ndb).expect("txn");
            if let Err(err) = src.poll_notes_into_view(&txn, damus) {
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
fn is_timeline_ready(damus: &mut Damus, timeline: usize) -> Result<bool> {
    let sub = match &damus.timelines[timeline].filter {
        FilterState::GotRemote(sub) => *sub,
        FilterState::Ready(_f) => return Ok(true),
        _ => return Ok(false),
    };

    // We got at least one eose for our filter request. Let's see
    // if nostrdb is done processing it yet.
    let res = damus.ndb.poll_for_notes(sub, 1);
    if res.is_empty() {
        debug!("check_timeline_filter_state: no notes found (yet?) for timeline {timeline}");
        return Ok(false);
    }

    info!("notes found for contact timeline after GotRemote!");

    let note_key = res[0];

    let filter = {
        let txn = Transaction::new(&damus.ndb).expect("txn");
        let note = damus.ndb.get_note_by_key(&txn, note_key).expect("note");
        filter::filter_from_tags(&note).map(|f| f.into_follow_filter())
    };

    // TODO: into_follow_filter is hardcoded to contact lists, let's generalize
    match filter {
        Err(Error::Filter(e)) => {
            error!("got broken when building filter {e}");
            damus.timelines[timeline].filter = FilterState::broken(e);
        }
        Err(err) => {
            error!("got broken when building filter {err}");
            damus.timelines[timeline].filter = FilterState::broken(FilterError::EmptyContactList);
            return Err(err);
        }
        Ok(filter) => {
            // we just switched to the ready state, we should send initial
            // queries and setup the local subscription
            info!("Found contact list! Setting up local and remote contact list query");
            setup_initial_timeline(damus, timeline, &filter).expect("setup init");
            damus.timelines[timeline].filter = FilterState::ready(filter.clone());

            let ck = &damus.timelines[timeline].kind;
            let subid = damus.gen_subid(&SubKind::Column(ck.clone()));
            damus.pool.subscribe(subid, filter)
        }
    }

    Ok(true)
}

#[cfg(feature = "profiling")]
fn setup_profiling() {
    puffin::set_scopes_on(true); // tell puffin to collect data
}

fn setup_initial_timeline(damus: &mut Damus, timeline: usize, filters: &[Filter]) -> Result<()> {
    damus.timelines[timeline].subscription = Some(damus.ndb.subscribe(filters)?);
    let txn = Transaction::new(&damus.ndb)?;
    debug!(
        "querying nostrdb sub {:?} {:?}",
        damus.timelines[timeline].subscription, damus.timelines[timeline].filter
    );
    let lim = filters[0].limit().unwrap_or(crate::filter::default_limit()) as i32;
    let results = damus.ndb.query(&txn, filters, lim)?;

    let filters = {
        let views = &damus.timelines[timeline].views;
        let filters: Vec<fn(&CachedNote, &Note) -> bool> =
            views.iter().map(|v| v.filter.filter()).collect();
        filters
    };

    for result in results {
        for (view, filter) in filters.iter().enumerate() {
            if filter(
                damus
                    .note_cache_mut()
                    .cached_note_or_insert_mut(result.note_key, &result.note),
                &result.note,
            ) {
                damus.timelines[timeline].views[view].notes.push(NoteRef {
                    key: result.note_key,
                    created_at: result.note.created_at(),
                })
            }
        }
    }

    Ok(())
}

fn setup_initial_nostrdb_subs(damus: &mut Damus) -> Result<()> {
    let timelines = damus.timelines.len();
    for i in 0..timelines {
        let filter = damus.timelines[i].filter.clone();
        match filter {
            FilterState::Ready(filters) => setup_initial_timeline(damus, i, &filters)?,

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
        setup_initial_nostrdb_subs(damus).expect("home subscription failed");
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
        SubKind::Column(_) => {
            // eose on column? whatevs
        }
        SubKind::Initial => {
            let txn = Transaction::new(&damus.ndb)?;
            UnknownIds::update(&txn, damus);
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
            let timeline_ind = if let Some(i) = damus.find_timeline(timeline_uid) {
                i
            } else {
                error!(
                    "timeline uid:{} not found for FetchingContactList",
                    timeline_uid
                );
                return Ok(());
            };

            let local_sub = if let FilterState::FetchingRemote(unisub) =
                &damus.timelines[timeline_ind].filter
            {
                unisub.local
            } else {
                // TODO: we could have multiple contact list results, we need
                // to check to see if this one is newer and use that instead
                warn!(
                    "Expected timeline to have FetchingRemote state but was {:?}",
                    damus.timelines[timeline_ind].filter
                );
                return Ok(());
            };

            damus.timelines[timeline_ind].filter = FilterState::got_remote(local_sub);

            /*
            // see if we're fast enough to catch a processed contact list
            let note_keys = damus.ndb.poll_for_notes(local_sub, 1);
            if !note_keys.is_empty() {
                debug!("fast! caught contact list from {relay_url} right away");
                let txn = Transaction::new(&damus.ndb)?;
                let note_key = note_keys[0];
                let nr = damus.ndb.get_note_by_key(&txn, note_key)?;
                let filter = filter::filter_from_tags(&nr)?.into_follow_filter();
                setup_initial_timeline(damus, timeline, &filter)
                damus.timelines[timeline_ind].filter = FilterState::ready(filter);
            }
            */
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

        let mut account_manager = AccountManager::new(
            // TODO: should pull this from settings
            None,
            // TODO: use correct KeyStorage mechanism for current OS arch
            KeyStorageType::None,
        );

        for key in parsed_args.keys {
            info!("adding account: {}", key.pubkey);
            account_manager.add_account(key);
        }

        // TODO: pull currently selected account from settings
        if account_manager.num_accounts() > 0 {
            account_manager.select_account(0);
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

        let account = account_manager
            .get_selected_account()
            .as_ref()
            .map(|a| a.pubkey.bytes());
        let ndb = Ndb::new(&dbpath, &config).expect("ndb");

        let mut timelines: Vec<Timeline> = Vec::with_capacity(parsed_args.columns.len());
        for col in parsed_args.columns {
            if let Some(timeline) = col.into_timeline(&ndb, account) {
                timelines.push(timeline);
            }
        }

        let debug = parsed_args.debug;

        if timelines.is_empty() {
            let filter = Filter::from_json(include_str!("../queries/timeline.json")).unwrap();
            timelines.push(Timeline::new(
                ColumnKind::Generic,
                FilterState::ready(vec![filter]),
            ));
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
            selected_timeline: 0,
            timelines,
            textmode: parsed_args.textmode,
            ndb,
            account_manager,
            frame_history: FrameHistory::default(),
            show_account_switcher: false,
            show_global_popup: false,
            global_nav: Vec::new(),
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
        let mut timelines: Vec<Timeline> = vec![];
        let filter = Filter::from_json(include_str!("../queries/global.json")).unwrap();
        timelines.push(Timeline::new(
            ColumnKind::Universe,
            FilterState::ready(vec![filter]),
        ));

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
            selected_timeline: 0,
            timelines,
            textmode: false,
            ndb: Ndb::new(data_path.as_ref().to_str().expect("db path ok"), &config).expect("ndb"),
            account_manager: AccountManager::new(None, KeyStorageType::None),
            frame_history: FrameHistory::default(),
            show_account_switcher: false,
            show_global_popup: true,
            global_nav: Vec::new(),
        }
    }

    pub fn find_timeline(&self, uid: u32) -> Option<usize> {
        for (i, timeline) in self.timelines.iter().enumerate() {
            if timeline.uid == uid {
                return Some(i);
            }
        }

        None
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

    pub fn selected_timeline(&mut self) -> &mut Timeline {
        &mut self.timelines[self.selected_timeline as usize]
    }

    pub fn select_down(&mut self) {
        self.selected_timeline().current_view_mut().select_down();
    }

    pub fn select_up(&mut self) {
        self.selected_timeline().current_view_mut().select_up();
    }

    pub fn select_left(&mut self) {
        if self.selected_timeline - 1 < 0 {
            return;
        }
        self.selected_timeline -= 1;
    }

    pub fn select_right(&mut self) {
        if self.selected_timeline + 1 >= self.timelines.len() as i32 {
            return;
        }
        self.selected_timeline += 1;
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

fn render_panel(ctx: &egui::Context, app: &mut Damus, timeline_ind: usize) {
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

                if !app.timelines.is_empty() {
                    ui.weak(format!(
                        "{} notes",
                        &app.timelines[timeline_ind]
                            .notes(ViewFilter::NotesAndReplies)
                            .len()
                    ));
                }
            }
        });
    });
}

/// Local thread unsubscribe
fn thread_unsubscribe(app: &mut Damus, id: &[u8; 32]) {
    let (unsubscribe, remote_subid) = {
        let txn = Transaction::new(&app.ndb).expect("txn");
        let root_id = crate::note::root_note_id_from_selected_id(app, &txn, id);

        let thread = app.threads.thread_mut(&app.ndb, &txn, root_id).get_ptr();
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
            if let Err(e) = app.ndb.unsubscribe(sub) {
                error!(
                    "failed to unsubscribe from thread: {e}, subid:{}, {} active subscriptions",
                    sub.id(),
                    app.ndb.subscription_count()
                );
            } else {
                info!(
                    "Unsubscribed from thread subid:{}. {} active subscriptions",
                    sub.id(),
                    app.ndb.subscription_count()
                );
            }

            // unsub from remote
            if let Some(subid) = remote_subid {
                app.pool.unsubscribe(subid);
            }
        }

        Ok(DecrementResult::ActiveSubscribers) => {
            info!(
                "Keeping thread subscription. {} active subscriptions.",
                app.ndb.subscription_count()
            );
            // do nothing
        }

        Err(e) => {
            // something is wrong!
            error!(
                "Thread unsubscribe error: {e}. {} active subsciptions.",
                app.ndb.subscription_count()
            );
        }
    }
}

fn render_nav(routes: Vec<Route>, timeline_ind: usize, app: &mut Damus, ui: &mut egui::Ui) {
    let navigating = app.timelines[timeline_ind].navigating;
    let returning = app.timelines[timeline_ind].returning;
    let app_ctx = Rc::new(RefCell::new(app));

    let nav_response = Nav::new(routes)
        .navigating(navigating)
        .returning(returning)
        .title(false)
        .show(ui, |ui, nav| match nav.top() {
            Route::Timeline(_n) => {
                let app = &mut app_ctx.borrow_mut();
                ui::TimelineView::new(app, timeline_ind).ui(ui);
                None
            }

            Route::ManageAccount => {
                ui.label("account management view");
                None
            }

            Route::Relays => {
                let pool = &mut app_ctx.borrow_mut().pool;
                let manager = RelayPoolManager::new(pool);
                RelayView::new(manager).ui(ui);
                None
            }

            Route::Thread(id) => {
                let app = &mut app_ctx.borrow_mut();
                let result = ui::ThreadView::new(app, timeline_ind, id.bytes()).ui(ui);

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
                let mut app = app_ctx.borrow_mut();

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

                let id = egui::Id::new(("post", timeline_ind, note.key().unwrap()));
                let response = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui::PostReplyView::new(&mut app, &note)
                        .id_source(id)
                        .show(ui)
                });

                Some(response)
            }
        });

    let mut app = app_ctx.borrow_mut();
    if let Some(reply_response) = nav_response.inner {
        if let Some(PostAction::Post(_np)) = reply_response.inner.action {
            app.timelines[timeline_ind].returning = true;
        }
    }

    if let Some(NavAction::Returned) = nav_response.action {
        let popped = app.timelines[timeline_ind].routes.pop();
        if let Some(Route::Thread(id)) = popped {
            thread_unsubscribe(&mut app, id.bytes());
        }
        app.timelines[timeline_ind].returning = false;
    } else if let Some(NavAction::Navigated) = nav_response.action {
        app.timelines[timeline_ind].navigating = false;
    }
}

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    //render_panel(ctx, app, 0);

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //let routes = app.timelines[0].routes.clone();

    main_panel(&ctx.style(), ui::is_narrow(ctx)).show(ctx, |ui| {
        render_nav(app.timelines[0].routes.clone(), 0, app, ui);
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
    render_panel(ctx, app, 0);
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let screen_size = ctx.screen_rect().width();
    let calc_panel_width = (screen_size / app.timelines.len() as f32) - 30.0;
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
        DesktopGlobalPopup::show(app.global_nav.clone(), app, ui);
        if need_scroll {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                timelines_view(ui, panel_sizes, app, app.timelines.len());
            });
        } else {
            timelines_view(ui, panel_sizes, app, app.timelines.len());
        }
    });
}

fn timelines_view(ui: &mut egui::Ui, sizes: Size, app: &mut Damus, timelines: usize) {
    StripBuilder::new(ui)
        .size(Size::exact(40.0))
        .sizes(sizes, timelines)
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

            for timeline_ind in 0..timelines {
                strip.cell(|ui| {
                    let rect = ui.available_rect_before_wrap();
                    render_nav(
                        app.timelines[timeline_ind].routes.clone(),
                        timeline_ind,
                        app,
                        ui,
                    );

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
