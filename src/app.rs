use crate::abbrev;
use crate::contacts::Contacts;
use crate::fonts::{setup_fonts, setup_gossip_fonts};
use crate::frame_history::FrameHistory;
use crate::images::fetch_img;
use crate::timeline;
use crate::ui::padding;
use crate::Result;
use egui::containers::scroll_area::ScrollBarVisibility;
use egui::load::SizedTexture;
use egui::widgets::Spinner;
use egui::{Context, Frame, ImageSource, Margin, TextureHandle, TextureId};
use egui_extras::Size;
use enostr::{ClientMessage, EventId, Filter, Profile, Pubkey, RelayEvent, RelayMessage};
use nostrdb::{
    Config, Ndb, NdbProfile, NdbProfileRecord, NoteKey, ProfileRecord, Subscription, Transaction,
};
use poll_promise::Promise;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use enostr::{Event, RelayPool};

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
enum UrlKey<'a> {
    Orig(&'a str),
    Failed(&'a str),
}

impl UrlKey<'_> {
    fn to_u64(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

type ImageCache = HashMap<u64, Promise<Result<TextureHandle>>>;

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
    pub notes: Vec<NoteRef>,
}

impl Timeline {
    pub fn new() -> Self {
        let mut notes: Vec<NoteRef> = vec![];
        notes.reserve(1000);

        Timeline { notes }
    }
}

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
pub struct Damus {
    state: DamusState,
    contacts: Contacts,
    n_panels: u32,
    compose: String,

    pool: RelayPool,
    home_sub: Option<Subscription>,

    timelines: Vec<Timeline>,

    img_cache: ImageCache,
    ndb: Ndb,

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
    if let Err(e) = pool.add_url("wss://purplepag.es".to_string(), wakeup) {
        error!("{:?}", e)
    }
}

fn get_home_filter() -> Filter {
    Filter::new().limit(100).kinds(vec![1, 42]).pubkeys(
        [
            Pubkey::from_hex("32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245")
                .unwrap(),
        ]
        .into(),
    )
}

fn send_initial_filters(pool: &mut RelayPool, relay_url: &str) {
    let filter = get_home_filter();
    info!("Sending initial filters to {}", relay_url);

    let subid = "initial";
    for relay in &mut pool.relays {
        let relay = &mut relay.relay;
        if relay.url == relay_url {
            relay.subscribe(subid.to_string(), vec![filter]);
            return;
        }
    }
}

fn try_process_event(damus: &mut Damus, ctx: &egui::Context) {
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
            RelayEvent::Opened => send_initial_filters(&mut damus.pool, &relay),
            // TODO: handle reconnects
            RelayEvent::Closed => warn!("{} connection closed", &relay),
            RelayEvent::Error(e) => error!("{}", e),
            RelayEvent::Other(msg) => debug!("other event {:?}", &msg),
            RelayEvent::Message(msg) => process_message(damus, &relay, &msg),
        }
    }

    // do we have any new processed events?
    if let Some(ref sub) = damus.home_sub {
        let new_note_ids = damus.ndb.poll_for_notes(sub, 100);
        if new_note_ids.len() > 0 {
            info!("{} new notes! {:?}", new_note_ids.len(), new_note_ids);
        }

        if let Ok(txn) = Transaction::new(&damus.ndb) {
            let new_refs = new_note_ids
                .iter()
                .map(|key| {
                    let note = damus
                        .ndb
                        .get_note_by_key(&txn, NoteKey::new(*key))
                        .expect("no note??");
                    NoteRef {
                        key: NoteKey::new(*key),
                        created_at: note.created_at(),
                    }
                })
                .collect();

            damus.timelines[0].notes =
                timeline::merge_sorted_vecs(&damus.timelines[0].notes, &new_refs);
        } else {
            error!("Transaction error when polling")
        }
    }
}

#[cfg(feature = "profiling")]
fn setup_profiling() {
    puffin::set_scopes_on(true); // tell puffin to collect data
}

fn setup_initial_nostrdb_subs(damus: &mut Damus) -> Result<()> {
    let filter: nostrdb::Filter = crate::filter::convert_enostr_filter(&get_home_filter());
    let filters = vec![filter];
    damus.home_sub = Some(damus.ndb.subscribe(filters.clone())?);
    let txn = Transaction::new(&damus.ndb)?;
    let res = damus.ndb.query(&txn, filters, 100)?;
    damus.timelines[0].notes = res
        .iter()
        .map(|qr| NoteRef {
            key: qr.note_key,
            created_at: qr.note.created_at(),
        })
        .collect();

    Ok(())
}

fn update_damus(damus: &mut Damus, ctx: &egui::Context) {
    if damus.state == DamusState::Initializing {
        #[cfg(feature = "profiling")]
        setup_profiling();

        setup_gossip_fonts(ctx);
        damus.pool = RelayPool::new();
        relay_setup(&mut damus.pool, ctx);
        damus.state = DamusState::Initialized;
        setup_initial_nostrdb_subs(damus).expect("home subscription failed");
    }

    try_process_event(damus, ctx);
}

fn process_event(damus: &mut Damus, _subid: &str, event: &str) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //info!("processing event {}", event);
    damus.ndb.process_event(&event);

    /*
    let kind = event.kind();
    if kind == 0 {
        process_metadata_event(damus, &event);
    } else if kind == 1 {
        let cloned_id = event.id.clone();
        damus.all_events.insert(cloned_id.clone(), event);
        damus.events.insert(0, cloned_id);
    }
    */
}

fn get_unknown_author_ids<'a>(
    txn: &'a Transaction,
    damus: &Damus,
    timeline: usize,
) -> Result<Vec<&'a [u8; 32]>> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let mut authors: HashSet<&'a [u8; 32]> = HashSet::new();

    for noteref in &damus.timelines[timeline].notes {
        let note = damus.ndb.get_note_by_key(&txn, noteref.key)?;
        let profile = damus.ndb.get_profile_by_pubkey(&txn, note.pubkey());

        if profile.is_err() {
            authors.insert(note.pubkey());
        }
    }

    Ok(authors.into_iter().collect())
}

fn handle_eose(damus: &mut Damus, subid: &str, relay_url: &str) -> Result<()> {
    if subid == "initial" {
        let txn = Transaction::new(&damus.ndb)?;
        let authors = get_unknown_author_ids(&txn, damus, 0)?;
        let n_authors = authors.len();
        let filter = Filter::new()
            .authors(authors.iter().map(|p| Pubkey::new(*p)).collect())
            .kinds(vec![0]);
        info!(
            "Getting {} unknown author profiles from {}",
            n_authors, relay_url
        );
        let msg = ClientMessage::req("profiles".to_string(), vec![filter]);
        damus.pool.send_to(&msg, relay_url);
    } else if subid == "profiles" {
        info!("Got profiles from {}", relay_url);
        let msg = ClientMessage::close("profiles".to_string());
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
            handle_eose(damus, &sid, relay);
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
    pub fn new<P: AsRef<Path>>(cc: &eframe::CreationContext<'_>, data_path: P) -> Self {
        // This is also where you can customized the look at feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        //if let Some(storage) = cc.storage {
        //return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        //}
        //

        cc.egui_ctx
            .set_pixels_per_point(cc.egui_ctx.pixels_per_point() + 0.2);

        let mut config = Config::new();
        config.set_ingester_threads(2);
        Self {
            state: DamusState::Initializing,
            contacts: Contacts::new(),
            pool: RelayPool::new(),
            home_sub: None,
            img_cache: HashMap::new(),
            n_panels: 1,
            timelines: vec![Timeline::new()],
            ndb: Ndb::new(data_path.as_ref().to_str().expect("db path ok"), &config).expect("ndb"),
            compose: "".to_string(),
            frame_history: FrameHistory::default(),
        }
    }
}

fn render_pfp(ui: &mut egui::Ui, img_cache: &mut ImageCache, url: &str) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let urlkey = UrlKey::Orig(url).to_u64();
    let m_cached_promise = img_cache.get(&urlkey);
    if m_cached_promise.is_none() {
        debug!("urlkey: {:?}", &urlkey);
        img_cache.insert(urlkey, fetch_img(ui.ctx(), url));
    }

    let pfp_size = 40.0;

    match img_cache[&urlkey].ready() {
        None => {
            ui.add(Spinner::new().size(40.0));
        }
        Some(Err(_err)) => {
            let failed_key = UrlKey::Failed(url).to_u64();
            //debug!("has failed promise? {}", img_cache.contains_key(&failed_key));
            let m_failed_promise = img_cache.get_mut(&failed_key);
            if m_failed_promise.is_none() {
                warn!("failed key: {:?}", &failed_key);
                let no_pfp = fetch_img(ui.ctx(), no_pfp_url());
                img_cache.insert(failed_key, no_pfp);
            }

            match img_cache[&failed_key].ready() {
                None => {
                    ui.add(Spinner::new().size(40.0));
                }
                Some(Err(e)) => {
                    //error!("Image load error: {:?}", e);
                    ui.label("❌");
                }
                Some(Ok(img)) => {
                    pfp_image(ui, img, pfp_size);
                }
            }
        }
        Some(Ok(img)) => {
            pfp_image(ui, img, pfp_size);
        }
    }
}

fn pfp_image<'a>(ui: &mut egui::Ui, img: &TextureHandle, size: f32) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //img.show_max_size(ui, egui::vec2(size, size))
    ui.add(egui::Image::new(img).max_width(size))
    //.with_options()
}

fn ui_abbreviate_name(ui: &mut egui::Ui, name: &str, len: usize) {
    if name.len() > len {
        let closest = abbrev::floor_char_boundary(name, len);
        ui.strong(&name[..closest]);
        ui.strong("...");
    } else {
        ui.strong(name);
    }
}

fn render_username(ui: &mut egui::Ui, profile: Option<&ProfileRecord>, pk: &[u8; 32]) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        //ui.spacing_mut().item_spacing.x = 0.0;
        if let Some(profile) = profile {
            if let Some(prof) = profile.record.profile() {
                if let Some(display_name) = prof.display_name() {
                    ui_abbreviate_name(ui, display_name, 20);
                }
            }
        } else {
            ui.strong("nostrich");
        }

        /*
        ui.label(&pk.as_ref()[0..8]);
        ui.label(":");
        ui.label(&pk.as_ref()[64 - 8..]);
        */
    });
}

fn no_pfp_url() -> &'static str {
    "https://damus.io/img/no-profile.svg"
}

fn render_notes_in_viewport(
    ui: &mut egui::Ui,
    damus: &mut Damus,
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
        let padding = (i % 100) as f32;
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

fn render_note(ui: &mut egui::Ui, damus: &mut Damus, note_key: NoteKey) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
        let txn = if let Ok(txn) = Transaction::new(&damus.ndb) {
            txn
        } else {
            return;
        };

        let ev = if let Ok(ev) = damus.ndb.get_note_by_key(&txn, note_key) {
            ev
        } else {
            return;
        };

        let profile = damus.ndb.get_profile_by_pubkey(&txn, ev.pubkey());

        padding(10.0, ui, |ui| {
            match profile
                .as_ref()
                .ok()
                .and_then(|p| p.record.profile()?.picture())
            {
                // these have different lifetimes and types,
                // so the calls must be separate
                Some(pic) => render_pfp(ui, &mut damus.img_cache, pic),
                None => render_pfp(ui, &mut damus.img_cache, no_pfp_url()),
            }

            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                render_username(ui, profile.as_ref().ok(), ev.pubkey());

                ui.weak(ev.content());
            })
        });
    });
}

fn render_notes(ui: &mut egui::Ui, damus: &mut Damus, timeline: usize) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let num_notes = damus.timelines[timeline].notes.len();

    for i in 0..num_notes {
        render_note(ui, damus, damus.timelines[timeline].notes[i].key);

        ui.separator();
    }
}

fn timeline_view(ui: &mut egui::Ui, app: &mut Damus, timeline: usize) {
    padding(10.0, ui, |ui| ui.heading("Timeline"));

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
            render_notes(ui, app, timeline);
        });
}

fn top_panel(ctx: &egui::Context) -> egui::TopBottomPanel {
    // mobile needs padding, at least on android
    if is_mobile(ctx) {
        let mut top_margin = Margin::default();
        top_margin.top = 20.0;

        let frame = Frame {
            inner_margin: top_margin,
            fill: ctx.style().visuals.panel_fill,
            ..Default::default()
        };

        return egui::TopBottomPanel::top("top_panel").frame(frame);
    }

    egui::TopBottomPanel::top("top_panel").frame(Frame::none())
}

#[inline]
fn horizontal_centered() -> egui::Layout {
    egui::Layout::left_to_right(egui::Align::Center)
}

fn render_panel<'a>(ctx: &egui::Context, app: &'a mut Damus, timeline_ind: usize) {
    top_panel(ctx).show(ctx, |ui| {
        set_app_style(ui);

        ui.horizontal_wrapped(|ui| {
            ui.visuals_mut().button_frame = false;
            egui::widgets::global_dark_light_mode_switch(ui);

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

            ui.label(format!(
                "FPS: {:.2}, {:10.1}ms",
                app.frame_history.fps(),
                app.frame_history.mean_frame_time() * 1e3
            ));

            let timeline = &app.timelines[timeline_ind];
            ui.label(format!("{} notes", timeline.notes.len()));
        });
    });
}

fn set_app_style(ui: &mut egui::Ui) {
    if ui.visuals().dark_mode {
        ui.visuals_mut().override_text_color = Some(egui::Color32::from_rgb(250, 250, 250));
        ui.visuals_mut().panel_fill = egui::Color32::from_rgb(30, 30, 30);
    } else {
        ui.visuals_mut().override_text_color = Some(egui::Color32::BLACK);
    };
}

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    render_panel(ctx, app, 0);

    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let panel_width = ctx.screen_rect().width();

    egui::CentralPanel::default().show(ctx, |ui| {
        set_app_style(ui);
        timeline_panel(ui, panel_width, 0, |ui| {
            timeline_view(ui, app, 0);
        });
    });
}

fn render_damus_desktop(ctx: &egui::Context, app: &mut Damus) {
    render_panel(ctx, app, 0);
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let screen_size = ctx.screen_rect().width();
    let calc_panel_width = (screen_size / app.n_panels as f32) - 30.0;
    let min_width = 300.0;
    let need_scroll = calc_panel_width < min_width;
    let panel_width = if need_scroll {
        min_width
    } else {
        calc_panel_width
    };

    if app.n_panels == 1 {
        let panel_width = ctx.screen_rect().width();
        egui::CentralPanel::default().show(ctx, |ui| {
            set_app_style(ui);
            timeline_panel(ui, panel_width, 0, |ui| {
                //postbox(ui, app);
                timeline_view(ui, app, 0);
            });
        });

        return;
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        set_app_style(ui);
        egui::ScrollArea::horizontal()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for ind in 0..app.n_panels {
                    if ind == 0 {
                        //postbox(ui, app);
                    }
                    timeline_panel(ui, panel_width, ind, |ui| {
                        // TODO: add new timeline to each panel
                        timeline_view(ui, app, 0);
                    });
                }
            });
    });
}

fn postbox(ui: &mut egui::Ui, app: &mut Damus) {
    let output = egui::TextEdit::multiline(&mut app.compose)
        .hint_text("Type something!")
        .show(ui);

    /*
    let width = ui.available_width();
    let height = 100.0;
    let shapes = [Shape::Rect(RectShape {
        rect: epaint::Rect::from_min_max(pos2(10.0, 10.0), pos2(width, height)),
        rounding: epaint::Rounding::same(10.0),
        fill: Color32::from_rgb(0x25, 0x25, 0x25),
        stroke: Stroke::new(2.0, Color32::from_rgb(0x39, 0x39, 0x39)),
    })];

    ui.painter().extend(shapes);
    */
}

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

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

pub const LOREM_IPSUM_LONG: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.

Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra, est eros bibendum elit, nec luctus magna felis sollicitudin mauris. Integer in mauris eu nibh euismod gravida. Duis ac tellus et risus vulputate vehicula. Donec lobortis risus a elit. Etiam tempor. Ut ullamcorper, ligula eu tempor congue, eros est euismod turpis, id tincidunt sapien risus a quam. Maecenas fermentum consequat mi. Donec fermentum. Pellentesque malesuada nulla a mi. Duis sapien sem, aliquet nec, commodo eget, consequat quis, neque. Aliquam faucibus, elit ut dictum aliquet, felis nisl adipiscing sapien, sed malesuada diam lacus eget erat. Cras mollis scelerisque nunc. Nullam arcu. Aliquam consequat. Curabitur augue lorem, dapibus quis, laoreet et, pretium ac, nisi. Aenean magna nisl, mollis quis, molestie eu, feugiat in, orci. In hac habitasse platea dictumst.";
