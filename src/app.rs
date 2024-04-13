use crate::app_creation::setup_cc;
use crate::app_style::user_requested_visuals_change;
use crate::error::Error;
use crate::frame_history::FrameHistory;
use crate::imgcache::ImageCache;
use crate::notecache::NoteCache;
use crate::timeline;
use crate::ui;
use crate::Result;
use egui::containers::scroll_area::ScrollBarVisibility;

use egui::{Context, Frame, Margin, Style};

use enostr::{ClientMessage, Filter, Pubkey, RelayEvent, RelayMessage};
use nostrdb::{BlockType, Config, Mention, Ndb, Note, NoteKey, Subscription, Transaction};

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use enostr::RelayPool;

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum DamusState {
    Initializing,
    Initialized,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct NoteRef {
    pub key: NoteKey,
    pub created_at: u64,
}

impl PartialOrd for NoteRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match self.created_at.cmp(&other.created_at) {
            Ordering::Equal => self.key.cmp(&other.key).into(),
            Ordering::Less => Some(Ordering::Greater),
            Ordering::Greater => Some(Ordering::Less),
        }
    }
}

impl Ord for NoteRef {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap()
    }
}

struct Timeline {
    pub filter: Vec<Filter>,
    pub notes: Vec<NoteRef>,
    pub subscription: Option<Subscription>,
}

impl Timeline {
    pub fn new(filter: Vec<Filter>) -> Self {
        let mut notes: Vec<NoteRef> = vec![];
        notes.reserve(1000);
        let subscription: Option<Subscription> = None;

        Timeline {
            filter,
            notes,
            subscription,
        }
    }
}

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
pub struct Damus {
    state: DamusState,
    //compose: String,
    note_cache: HashMap<NoteKey, NoteCache>,
    pool: RelayPool,
    pub textmode: bool,

    timelines: Vec<Timeline>,

    pub img_cache: ImageCache,
    pub ndb: Ndb,

    frame_history: crate::frame_history::FrameHistory,
}

pub fn is_mobile(ctx: &egui::Context) -> bool {
    //true
    let screen_size = ctx.screen_rect().size();
    screen_size.x < 550.0
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
    if let Err(e) = pool.add_url("wss://pyramid.fiatjaf.com".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
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

fn send_initial_filters(damus: &mut Damus, relay_url: &str) {
    info!("Sending initial filters to {}", relay_url);
    let mut c: u32 = 1;

    for relay in &mut damus.pool.relays {
        let relay = &mut relay.relay;
        if relay.url == relay_url {
            for timeline in &damus.timelines {
                relay.subscribe(format!("initial{}", c), timeline.filter.clone());
                c += 1;
            }
            return;
        }
    }
}

fn try_process_event(damus: &mut Damus, ctx: &egui::Context) -> Result<()> {
    let amount = 0.2;
    if ctx.input(|i| i.key_pressed(egui::Key::Equals)) {
        ctx.set_pixels_per_point(ctx.pixels_per_point() + amount);
    } else if ctx.input(|i| i.key_pressed(egui::Key::Minus)) {
        ctx.set_pixels_per_point(ctx.pixels_per_point() - amount);
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
            RelayEvent::Error(e) => error!("wsev->relayev: {}", e),
            RelayEvent::Other(msg) => debug!("other event {:?}", &msg),
            RelayEvent::Message(msg) => process_message(damus, &relay, &msg),
        }
    }

    let txn = Transaction::new(&damus.ndb)?;
    let mut unknown_ids: HashSet<UnknownId> = HashSet::new();
    for timeline in 0..damus.timelines.len() {
        if let Err(err) = poll_notes_for_timeline(damus, &txn, timeline, &mut unknown_ids) {
            error!("{}", err);
        }
    }

    let unknown_ids: Vec<UnknownId> = unknown_ids.into_iter().collect();
    if let Some(filters) = get_unknown_ids_filter(&unknown_ids) {
        info!(
            "Getting {} unknown author profiles from relays",
            unknown_ids.len()
        );
        let msg = ClientMessage::req("unknown_ids".to_string(), filters);
        damus.pool.send(&msg);
    }

    Ok(())
}

#[derive(Hash, Clone, Copy, PartialEq, Eq)]
enum UnknownId<'a> {
    Pubkey(&'a [u8; 32]),
    Id(&'a [u8; 32]),
}

impl<'a> UnknownId<'a> {
    pub fn is_pubkey(&self) -> Option<&'a [u8; 32]> {
        match self {
            UnknownId::Pubkey(pk) => Some(pk),
            _ => None,
        }
    }

    pub fn is_id(&self) -> Option<&'a [u8; 32]> {
        match self {
            UnknownId::Id(id) => Some(id),
            _ => None,
        }
    }
}

fn get_unknown_note_ids<'a>(
    ndb: &Ndb,
    txn: &'a Transaction,
    note: &Note<'a>,
    note_key: NoteKey,
    ids: &mut HashSet<UnknownId<'a>>,
) -> Result<()> {
    // the author pubkey

    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
        ids.insert(UnknownId::Pubkey(note.pubkey()));
    }

    let blocks = ndb.get_blocks_by_key(txn, note_key)?;
    for block in blocks.iter(note) {
        let _blocktype = block.blocktype();
        match block.blocktype() {
            BlockType::MentionBech32 => match block.as_mention().unwrap() {
                Mention::Pubkey(npub) => {
                    if ndb.get_profile_by_pubkey(txn, npub.pubkey()).is_err() {
                        ids.insert(UnknownId::Pubkey(npub.pubkey()));
                    }
                }
                Mention::Profile(nprofile) => {
                    if ndb.get_profile_by_pubkey(txn, nprofile.pubkey()).is_err() {
                        ids.insert(UnknownId::Pubkey(nprofile.pubkey()));
                    }
                }
                Mention::Event(ev) => {
                    if ndb.get_note_by_id(txn, ev.id()).is_err() {
                        ids.insert(UnknownId::Id(ev.id()));
                    }
                }
                Mention::Note(note) => {
                    if ndb.get_note_by_id(txn, note.id()).is_err() {
                        ids.insert(UnknownId::Id(note.id()));
                    }
                }
                _ => {}
            },

            _ => {}
        }
    }

    Ok(())
}

fn poll_notes_for_timeline<'a>(
    damus: &mut Damus,
    txn: &'a Transaction,
    timeline: usize,
    ids: &mut HashSet<UnknownId<'a>>,
) -> Result<()> {
    let sub = if let Some(sub) = &damus.timelines[timeline].subscription {
        sub
    } else {
        return Err(Error::NoActiveSubscription);
    };

    let new_note_ids = damus.ndb.poll_for_notes(&sub, 100);
    if new_note_ids.len() > 0 {
        debug!("{} new notes! {:?}", new_note_ids.len(), new_note_ids);
    }

    let new_refs = new_note_ids
        .iter()
        .map(|key| {
            let note = damus.ndb.get_note_by_key(&txn, *key).expect("no note??");

            let _ = get_unknown_note_ids(&damus.ndb, txn, &note, *key, ids);

            NoteRef {
                key: *key,
                created_at: note.created_at(),
            }
        })
        .collect();

    damus.timelines[timeline].notes =
        timeline::merge_sorted_vecs(&damus.timelines[timeline].notes, &new_refs);

    Ok(())
}

#[cfg(feature = "profiling")]
fn setup_profiling() {
    puffin::set_scopes_on(true); // tell puffin to collect data
}

fn setup_initial_nostrdb_subs(damus: &mut Damus) -> Result<()> {
    for timeline in &mut damus.timelines {
        let filters: Vec<nostrdb::Filter> = timeline
            .filter
            .iter()
            .map(|f| crate::filter::convert_enostr_filter(f))
            .collect();
        timeline.subscription = Some(damus.ndb.subscribe(filters.clone())?);
        let txn = Transaction::new(&damus.ndb)?;
        debug!(
            "querying sub {} {:?}",
            timeline.subscription.as_ref().unwrap().id,
            timeline.filter
        );
        let res = damus.ndb.query(
            &txn,
            filters,
            timeline.filter[0].limit.unwrap_or(200) as i32,
        )?;
        timeline.notes = res
            .iter()
            .map(|qr| NoteRef {
                key: qr.note_key,
                created_at: qr.note.created_at(),
            })
            .collect();
    }

    Ok(())
}

fn update_damus(damus: &mut Damus, ctx: &egui::Context) {
    if damus.state == DamusState::Initializing {
        #[cfg(feature = "profiling")]
        setup_profiling();

        damus.pool = RelayPool::new();
        relay_setup(&mut damus.pool, ctx);
        damus.state = DamusState::Initialized;
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
    if let Err(_err) = damus.ndb.process_event(&event) {
        error!("error processing event {}", event);
    }
}

fn get_unknown_ids<'a>(txn: &'a Transaction, damus: &Damus) -> Result<Vec<UnknownId<'a>>> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let mut ids: HashSet<UnknownId> = HashSet::new();

    for timeline in &damus.timelines {
        for noteref in &timeline.notes {
            let note = damus.ndb.get_note_by_key(&txn, noteref.key)?;
            let _ = get_unknown_note_ids(&damus.ndb, txn, &note, note.key().unwrap(), &mut ids);
        }
    }

    Ok(ids.into_iter().collect())
}

fn get_unknown_ids_filter<'a>(ids: &[UnknownId<'a>]) -> Option<Vec<Filter>> {
    if ids.is_empty() {
        return None;
    }

    let mut filters: Vec<Filter> = vec![];

    let pks: Vec<Pubkey> = ids
        .iter()
        .flat_map(|id| id.is_pubkey().map(Pubkey::new))
        .collect();
    if !pks.is_empty() {
        let pk_filter = Filter::new().authors(pks).kinds(vec![0]);

        filters.push(pk_filter);
    }

    let note_ids: Vec<enostr::EventId> = ids
        .iter()
        .flat_map(|id| id.is_id().map(|id| enostr::EventId::new(*id)))
        .collect();
    if !note_ids.is_empty() {
        filters.push(Filter::new().ids(note_ids));
    }

    Some(filters)
}

fn handle_eose(damus: &mut Damus, subid: &str, relay_url: &str) -> Result<()> {
    if subid.starts_with("initial") {
        let txn = Transaction::new(&damus.ndb)?;
        let ids = get_unknown_ids(&txn, damus)?;
        if let Some(filters) = get_unknown_ids_filter(&ids) {
            info!("Getting {} unknown ids from {}", ids.len(), relay_url);
            let msg = ClientMessage::req("unknown_ids".to_string(), filters);
            damus.pool.send_to(&msg, relay_url);
        }
    } else if subid == "unknown_ids" {
        let msg = ClientMessage::close("unknown_ids".to_string());
        damus.pool.send_to(&msg, relay_url);
    } else {
        warn!("got unknown eose subid {}", subid);
    }

    Ok(())
}

fn process_message(damus: &mut Damus, relay: &str, msg: &RelayMessage) {
    match msg {
        RelayMessage::Event(subid, ev) => process_event(damus, &subid, ev),
        RelayMessage::Notice(msg) => warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(sid) => {
            if let Err(err) = handle_eose(damus, &sid, relay) {
                error!("error handling eose: {}", err);
            }
        }
    }
}

fn render_damus(damus: &mut Damus, ctx: &Context) {
    if is_mobile(ctx) {
        render_damus_mobile(ctx, damus);
    } else {
        render_damus_desktop(ctx, damus);
    }

    ctx.request_repaint_after(Duration::from_secs(1));

    #[cfg(feature = "profiling")]
    puffin_egui::profiler_window(ctx);
}

impl Damus {
    /// Called once before the first frame.
    pub fn new<P: AsRef<Path>>(
        cc: &eframe::CreationContext<'_>,
        data_path: P,
        args: Vec<String>,
    ) -> Self {
        // This is also where you can customized the look at feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        //if let Some(storage) = cc.storage {
        //return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        //}
        //

        setup_cc(cc);

        let mut timelines: Vec<Timeline> = vec![];
        let _initial_limit = 100;
        if args.len() > 1 {
            for arg in &args[1..] {
                let filter = serde_json::from_str(&arg).unwrap();
                timelines.push(Timeline::new(filter));
            }
        } else {
            let filter = serde_json::from_str(&include_str!("../queries/global.json")).unwrap();
            timelines.push(Timeline::new(filter));
        };

        let imgcache_dir = data_path.as_ref().join("cache/img");
        let _ = std::fs::create_dir_all(imgcache_dir.clone());

        let mut config = Config::new();
        config.set_ingester_threads(2);
        Self {
            state: DamusState::Initializing,
            pool: RelayPool::new(),
            img_cache: ImageCache::new(imgcache_dir),
            note_cache: HashMap::new(),
            timelines,
            textmode: false,
            ndb: Ndb::new(data_path.as_ref().to_str().expect("db path ok"), &config).expect("ndb"),
            //compose: "".to_string(),
            frame_history: FrameHistory::default(),
        }
    }

    pub fn get_note_cache_mut(&mut self, note_key: NoteKey, created_at: u64) -> &mut NoteCache {
        self.note_cache
            .entry(note_key)
            .or_insert_with(|| NoteCache::new(created_at))
    }
}

/*
fn render_notes_in_viewport(
    ui: &mut egui::Ui,
    _damus: &mut Damus,
    viewport: egui::Rect,
    row_height: f32,
    font_id: egui::FontId,
) {
    let num_rows = 10_000;
    ui.set_height(row_height * num_rows as f32);

    let first_item = (viewport.min.y / row_height).floor().max(0.0) as usize;
    let last_item = (viewport.max.y / row_height).ceil() as usize + 1;
    let last_item = last_item.min(num_rows);

    let mut used_rect = egui::Rect::NOTHING;

    for i in first_item..last_item {
        let _padding = (i % 100) as f32;
        let indent = (((i as f32) / 10.0).sin() * 20.0) + 10.0;
        let x = ui.min_rect().left() + indent;
        let y = ui.min_rect().top() + i as f32 * row_height;
        let text = format!(
            "This is row {}/{}, indented by {} pixels",
            i + 1,
            num_rows,
            indent
        );
        let text_rect = ui.painter().text(
            egui::pos2(x, y),
            egui::Align2::LEFT_TOP,
            text,
            font_id.clone(),
            ui.visuals().text_color(),
        );
        used_rect = used_rect.union(text_rect);
    }

    ui.allocate_rect(used_rect, egui::Sense::hover()); // make sure it is visible!
}
*/

/*
fn circle_icon(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let stroke = ui.style().interact(&response).fg_stroke;
    let radius = egui::lerp(2.0..=3.0, openness);
    ui.painter()
        .circle_filled(response.rect.center(), radius, stroke.color);
}
*/

fn render_notes(ui: &mut egui::Ui, damus: &mut Damus, timeline: usize) -> Result<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let num_notes = damus.timelines[timeline].notes.len();
    let txn = Transaction::new(&damus.ndb)?;

    for i in 0..num_notes {
        let note_key = damus.timelines[timeline].notes[i].key;
        let note = if let Ok(note) = damus.ndb.get_note_by_key(&txn, note_key) {
            note
        } else {
            warn!("failed to query note {:?}", note_key);
            continue;
        };

        let note_ui = ui::Note::new(damus, &note);
        ui.add(note_ui);
        ui.add(egui::Separator::default().spacing(0.0));
    }

    Ok(())
}

fn timeline_view(ui: &mut egui::Ui, app: &mut Damus, timeline: usize) {
    //padding(4.0, ui, |ui| ui.heading("Notifications"));
    /*
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts(|f| f.row_height(&font_id)) + ui.spacing().item_spacing.y;
    */

    egui::ScrollArea::vertical()
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false; 2])
        /*
        .show_viewport(ui, |ui, viewport| {
            render_notes_in_viewport(ui, app, viewport, row_height, font_id);
        });
        */
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let _ = render_notes(ui, app, timeline);
        });
}

fn top_panel(ctx: &egui::Context) -> egui::TopBottomPanel {
    let mut top_margin = Margin::default();
    top_margin.top = 4.0;
    top_margin.left = 8.0;
    top_margin.right = 8.0;
    //top_margin.bottom = -20.0;

    let frame = Frame {
        inner_margin: top_margin,
        fill: ctx.style().visuals.panel_fill,
        ..Default::default()
    };

    egui::TopBottomPanel::top("top_panel")
        .frame(frame)
        .show_separator_line(false)
}

fn render_panel<'a>(ctx: &egui::Context, app: &'a mut Damus, timeline_ind: usize) {
    top_panel(ctx).show(ctx, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            ui.visuals_mut().button_frame = false;

            if let Some(new_visuals) =
                user_requested_visuals_change(is_mobile(ctx), ctx.style().visuals.dark_mode, ui)
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

                ui.weak(format!(
                    "{} notes",
                    &app.timelines[timeline_ind].notes.len()
                ));
            }
        });
    });
}

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    render_panel(ctx, app, 0);

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let panel_width = ctx.screen_rect().width();

    main_panel(&ctx.style()).show(ctx, |ui| {
        timeline_panel(ui, panel_width, 0, |ui| {
            timeline_view(ui, app, 0);
        });
    });
}

fn main_panel(style: &Style) -> egui::CentralPanel {
    egui::CentralPanel::default().frame(Frame {
        inner_margin: Margin::same(0.0),
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
    let min_width = 300.0;
    let need_scroll = calc_panel_width < min_width;
    let panel_width = if need_scroll {
        min_width
    } else {
        calc_panel_width
    };

    if app.timelines.len() == 1 {
        let panel_width = ctx.screen_rect().width();
        main_panel(&ctx.style()).show(ctx, |ui| {
            timeline_panel(ui, panel_width, 0, |ui| {
                //postbox(ui, app);
                timeline_view(ui, app, 0);
            });

            /*
            egui::Area::new("test")
                .fixed_pos(egui::pos2(50.0, 50.0))
                //.resizable(false)
                //.title_bar(false)
                .show(ctx, |ui| {
                    ui.label("Test");
                });
                */
        });

        return;
    }

    main_panel(&ctx.style()).show(ctx, |ui| {
        egui::ScrollArea::horizontal()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for timeline_ind in 0..app.timelines.len() {
                    if timeline_ind == 0 {
                        //postbox(ui, app);
                    }
                    timeline_panel(ui, panel_width, timeline_ind as u32, |ui| {
                        // TODO: add new timeline to each panel
                        timeline_view(ui, app, timeline_ind);
                    });
                }
            });
    });
}

/*
fn postbox(ui: &mut egui::Ui, app: &mut Damus) {
    let _output = egui::TextEdit::multiline(&mut app.compose)
        .hint_text("Type something!")
        .show(ui);

    let width = ui.available_width();
    let height = 100.0;
    let shapes = [Shape::Rect(RectShape {
        rect: epaint::Rect::from_min_max(pos2(10.0, 10.0), pos2(width, height)),
        rounding: epaint::Rounding::same(10.0),
        fill: Color32::from_rgb(0x25, 0x25, 0x25),
        stroke: Stroke::new(2.0, Color32::from_rgb(0x39, 0x39, 0x39)),
    })];

    ui.painter().extend(shapes);
}
    */

fn timeline_panel<R>(
    ui: &mut egui::Ui,
    panel_width: f32,
    ind: u32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::SidePanel::left(format!("l{}", ind))
        .resizable(false)
        .frame(Frame::none())
        .max_width(panel_width)
        .min_width(panel_width)
        .show_inside(ui, add_contents)
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
