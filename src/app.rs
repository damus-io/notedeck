use crate::abbrev;
use crate::app_creation::setup_cc;
use crate::colors;
use crate::error::Error;
use crate::fonts::NamedFontFamily;
use crate::frame_history::FrameHistory;
use crate::images::fetch_img;
use crate::imgcache::ImageCache;
use crate::notecache::NoteCache;
use crate::timeline;
use crate::ui::padding;
use crate::widgets::note::NoteContents;
use crate::Result;
use egui::containers::scroll_area::ScrollBarVisibility;

use egui::widgets::Spinner;
use egui::{Color32, Context, Frame, Label, Margin, RichText, Sense, Style, TextureHandle, Vec2};

use enostr::{ClientMessage, Filter, Pubkey, RelayEvent, RelayMessage};
use nostrdb::{
    BlockType, Config, Mention, Ndb, Note, NoteKey, ProfileRecord, Subscription, Transaction,
};

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

    timelines: Vec<Timeline>,

    img_cache: ImageCache,
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
    let mut seen_pubkeys: HashSet<&[u8; 32]> = HashSet::new();
    for timeline in 0..damus.timelines.len() {
        if let Err(err) = poll_notes_for_timeline(damus, &txn, timeline, &mut seen_pubkeys) {
            error!("{}", err);
        }
    }

    let mut pubkeys_to_fetch: Vec<&[u8; 32]> = vec![];
    for pubkey in seen_pubkeys {
        if let Err(_) = damus.ndb.get_profile_by_pubkey(&txn, pubkey) {
            pubkeys_to_fetch.push(pubkey)
        }
    }

    if pubkeys_to_fetch.len() > 0 {
        let filter = Filter::new()
            .authors(pubkeys_to_fetch.iter().map(|p| Pubkey::new(*p)).collect())
            .kinds(vec![0]);
        info!(
            "Getting {} unknown author profiles from relays",
            pubkeys_to_fetch.len()
        );
        let msg = ClientMessage::req("profiles".to_string(), vec![filter]);
        damus.pool.send(&msg);
    }

    Ok(())
}

fn get_unknown_note_pubkeys<'a>(
    ndb: &Ndb,
    txn: &'a Transaction,
    note: &Note<'a>,
    note_key: NoteKey,
    pubkeys: &mut HashSet<&'a [u8; 32]>,
) -> Result<()> {
    // the author pubkey

    if let Err(_) = ndb.get_profile_by_pubkey(txn, note.pubkey()) {
        pubkeys.insert(note.pubkey());
    }

    let blocks = ndb.get_blocks_by_key(txn, note_key)?;
    for block in blocks.iter(note) {
        let _blocktype = block.blocktype();
        match block.blocktype() {
            BlockType::MentionBech32 => match block.as_mention().unwrap() {
                Mention::Pubkey(npub) => {
                    if let Err(_) = ndb.get_profile_by_pubkey(txn, npub.pubkey()) {
                        pubkeys.insert(npub.pubkey());
                    }
                }
                Mention::Profile(nprofile) => {
                    if let Err(_) = ndb.get_profile_by_pubkey(txn, nprofile.pubkey()) {
                        pubkeys.insert(nprofile.pubkey());
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
    pubkeys: &mut HashSet<&'a [u8; 32]>,
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

            let _ = get_unknown_note_pubkeys(&damus.ndb, txn, &note, *key, pubkeys);

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
        info!(
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
        let _ = get_unknown_note_pubkeys(&damus.ndb, txn, &note, note.key().unwrap(), &mut authors);
    }

    Ok(authors.into_iter().collect())
}

fn handle_eose(damus: &mut Damus, subid: &str, relay_url: &str) -> Result<()> {
    if subid.starts_with("initial") {
        let txn = Transaction::new(&damus.ndb)?;
        let authors = get_unknown_author_ids(&txn, damus, 0)?;
        let n_authors = authors.len();
        if n_authors > 0 {
            let filter = Filter::new()
                .authors(authors.iter().map(|p| Pubkey::new(*p)).collect())
                .kinds(vec![0]);
            info!(
                "Getting {} unknown author profiles from {}",
                n_authors, relay_url
            );
            let msg = ClientMessage::req("profiles".to_string(), vec![filter]);
            damus.pool.send_to(&msg, relay_url);
        }
    } else if subid == "profiles" {
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
            if let Err(err) = handle_eose(damus, &sid, relay) {
                error!("error handling eose: {}", err);
            }
        }
    }
}

fn render_damus(damus: &mut Damus, ctx: &Context) {
    //ctx.style_mut(|s| set_app_style(s, is_mobile(ctx)));
    ctx.style_mut(|s| set_app_style(s, true));

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

fn paint_circle(ui: &mut egui::Ui, size: f32) {
    let (rect, _response) = ui.allocate_at_least(Vec2::new(size, size), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, ui.visuals().weak_text_color());
}

fn render_pfp(ui: &mut egui::Ui, damus: &mut Damus, url: &str) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let ui_size = 30.0;

    // We will want to downsample these so it's not blurry on hi res displays
    let img_size = (ui_size * 2.0) as u32;

    let m_cached_promise = damus.img_cache.map().get(url);
    if m_cached_promise.is_none() {
        let res = fetch_img(&damus.img_cache, ui.ctx(), url, img_size);
        damus.img_cache.map_mut().insert(url.to_owned(), res);
    }

    match damus.img_cache.map()[url].ready() {
        None => {
            ui.add(Spinner::new().size(ui_size));
        }

        // Failed to fetch profile!
        Some(Err(_err)) => {
            let m_failed_promise = damus.img_cache.map().get(url);
            if m_failed_promise.is_none() {
                let no_pfp = fetch_img(&damus.img_cache, ui.ctx(), no_pfp_url(), img_size);
                damus.img_cache.map_mut().insert(url.to_owned(), no_pfp);
            }

            match damus.img_cache.map().get(url).unwrap().ready() {
                None => {
                    paint_circle(ui, ui_size);
                }
                Some(Err(_e)) => {
                    //error!("Image load error: {:?}", e);
                    paint_circle(ui, ui_size);
                }
                Some(Ok(img)) => {
                    pfp_image(ui, img, ui_size);
                }
            }
        }
        Some(Ok(img)) => {
            pfp_image(ui, img, ui_size);
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
        ui.add(Label::new(
            RichText::new(name).family(NamedFontFamily::Medium.as_family()),
        ));
    }
}

fn render_username(ui: &mut egui::Ui, profile: Option<&ProfileRecord>, _pk: &[u8; 32]) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        //ui.spacing_mut().item_spacing.x = 0.0;
        if let Some(profile) = profile {
            if let Some(prof) = profile.record.profile() {
                if prof.display_name().is_some() && prof.display_name().unwrap() != "" {
                    ui_abbreviate_name(ui, prof.display_name().unwrap(), 20);
                } else if let Some(name) = prof.name() {
                    ui_abbreviate_name(ui, name, 20);
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

fn render_reltime(ui: &mut egui::Ui, note_cache: &mut NoteCache) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let color = Color32::from_rgb(0x8A, 0x8A, 0x8A);
    ui.add(Label::new(RichText::new("⋅").size(10.0).color(color)));
    ui.add(Label::new(
        RichText::new(note_cache.reltime_str())
            .size(10.0)
            .color(color),
    ));
}

/*
fn circle_icon(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let stroke = ui.style().interact(&response).fg_stroke;
    let radius = egui::lerp(2.0..=3.0, openness);
    ui.painter()
        .circle_filled(response.rect.center(), radius, stroke.color);
}
*/

#[derive(Hash, Clone, Copy)]
struct NoteTimelineKey {
    timeline: usize,
    note_key: NoteKey,
}

fn render_note(
    ui: &mut egui::Ui,
    damus: &mut Damus,
    note_key: NoteKey,
    timeline: usize,
) -> Result<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let txn = Transaction::new(&damus.ndb)?;
    let note = damus.ndb.get_note_by_key(&txn, note_key)?;
    let id = egui::Id::new(NoteTimelineKey { note_key, timeline });

    ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
        let profile = damus.ndb.get_profile_by_pubkey(&txn, note.pubkey());

        let mut collapse_state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);

        let inner_resp = padding(6.0, ui, |ui| {
            match profile
                .as_ref()
                .ok()
                .and_then(|p| p.record.profile()?.picture())
            {
                // these have different lifetimes and types,
                // so the calls must be separate
                Some(pic) => render_pfp(ui, damus, pic),
                None => render_pfp(ui, damus, no_pfp_url()),
            }

            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    render_username(ui, profile.as_ref().ok(), note.pubkey());

                    let note_cache = damus.get_note_cache_mut(note_key, note.created_at());
                    render_reltime(ui, note_cache);
                });

                ui.add(NoteContents::new(damus, &txn, &note, note_key));

                render_note_actionbar(ui);

                //let header_res = ui.horizontal(|ui| {});
            });
        });

        let resp = ui.interact(inner_resp.response.rect, id, Sense::hover());

        if resp.hovered() ^ collapse_state.is_open() {
            //info!("clicked {:?}, {}", note_key, collapse_state.is_open());
            collapse_state.toggle(ui);
            collapse_state.store(ui.ctx());
        }
    });

    Ok(())
}

fn render_note_actionbar(ui: &mut egui::Ui) -> egui::InnerResponse<()> {
    ui.horizontal(|ui| {
        let img_data = if ui.style().visuals.dark_mode {
            egui::include_image!("../assets/icons/reply.png")
        } else {
            egui::include_image!("../assets/icons/reply-dark.png")
        };

        ui.spacing_mut().button_padding = egui::vec2(0.0, 0.0);
        if ui
            .add(
                egui::Button::image(egui::Image::new(img_data).max_width(10.0))
                    //.stroke(egui::Stroke::NONE)
                    .frame(false)
                    .fill(ui.style().visuals.panel_fill),
            )
            .clicked()
        {}

        //if ui.add(egui::Button::new("like")).clicked() {}
    })
}

fn render_notes(ui: &mut egui::Ui, damus: &mut Damus, timeline: usize) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let num_notes = damus.timelines[timeline].notes.len();

    for i in 0..num_notes {
        let _ = render_note(ui, damus, damus.timelines[timeline].notes[i].key, timeline);

        ui.add(egui::Separator::default().spacing(0.0));
    }
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
            render_notes(ui, app, timeline);
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
            egui::widgets::global_dark_light_mode_switch(ui);

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

fn set_app_style(style: &mut Style, is_mobile: bool) {
    let visuals = &mut style.visuals;
    visuals.hyperlink_color = colors::PURPLE;
    if visuals.dark_mode {
        visuals.override_text_color = Some(egui::Color32::from_rgb(250, 250, 250));
        //visuals.panel_fill = egui::Color32::from_rgb(31, 31, 31);
        if is_mobile {
            visuals.panel_fill = egui::Color32::from_rgb(0, 0, 0);
        } else {
            visuals.panel_fill = egui::Color32::from_rgb(31, 31, 31);
        }
        //visuals.override_text_color = Some(egui::Color32::from_rgb(170, 177, 190));
        //visuals.panel_fill = egui::Color32::from_rgb(40, 44, 52);
    } else {
        visuals.override_text_color = Some(egui::Color32::BLACK);
    };
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
