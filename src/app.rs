use crate::abbrev;
use crate::error::Error;
use crate::fonts::setup_gossip_fonts;
use crate::frame_history::FrameHistory;
use crate::images::fetch_img;
use crate::timeline;
use crate::ui::padding;
use crate::Result;
use egui::containers::scroll_area::ScrollBarVisibility;

use egui::widgets::Spinner;
use egui::{Color32, Context, Frame, Hyperlink, Image, Margin, RichText, TextureHandle};

use enostr::{ClientMessage, Filter, Pubkey, RelayEvent, RelayMessage};
use nostrdb::{
    Block, BlockType, Blocks, Config, Mention, Ndb, Note, NoteKey, ProfileRecord, Subscription,
    Transaction,
};
use poll_promise::Promise;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use enostr::RelayPool;

const PURPLE: Color32 = Color32::from_rgb(0xCC, 0x43, 0xC5);

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
    pub subscription: Option<Subscription>,
}

impl Timeline {
    pub fn new() -> Self {
        let mut notes: Vec<NoteRef> = vec![];
        notes.reserve(1000);
        let subscription: Option<Subscription> = None;

        Timeline {
            notes,
            subscription,
        }
    }
}

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
pub struct Damus {
    state: DamusState,
    n_panels: u32,
    compose: String,
    initial_filter: Vec<enostr::Filter>,

    pool: RelayPool,

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

fn get_home_filter() -> Filter {
    Filter::new().limit(100).kinds(vec![1, 42]).pubkeys(
        [
            Pubkey::from_hex("32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245")
                .unwrap(),
        ]
        .into(),
    )
}

fn send_initial_filters(damus: &mut Damus, relay_url: &str) {
    info!("Sending initial filters to {}", relay_url);

    let subid = "initial";
    for relay in &mut damus.pool.relays {
        let relay = &mut relay.relay;
        if relay.url == relay_url {
            relay.subscribe(subid.to_string(), damus.initial_filter.clone());
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
            RelayEvent::Error(e) => error!("{}", e),
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
        let blocktype = block.blocktype();
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
        info!("{} new notes! {:?}", new_note_ids.len(), new_note_ids);
    }

    let new_refs = new_note_ids
        .iter()
        .map(|key| {
            let note_key = NoteKey::new(*key);
            let note = damus
                .ndb
                .get_note_by_key(&txn, note_key)
                .expect("no note??");

            let _ = get_unknown_note_pubkeys(&damus.ndb, txn, &note, note_key, pubkeys);

            NoteRef {
                key: NoteKey::new(*key),
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
    let filters: Vec<nostrdb::Filter> = damus
        .initial_filter
        .iter()
        .map(|f| crate::filter::convert_enostr_filter(f))
        .collect();
    damus.timelines[0].subscription = Some(damus.ndb.subscribe(filters.clone())?);
    let txn = Transaction::new(&damus.ndb)?;
    let res = damus.ndb.query(&txn, filters, 200)?;
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

        cc.egui_ctx
            .set_pixels_per_point(cc.egui_ctx.pixels_per_point() + 0.2);

        egui_extras::install_image_loaders(&cc.egui_ctx);

        let initial_filter = if args.len() > 1 {
            serde_json::from_str(&args[1]).unwrap()
        } else {
            vec![get_home_filter()]
        };

        let mut config = Config::new();
        config.set_ingester_threads(2);
        Self {
            state: DamusState::Initializing,
            pool: RelayPool::new(),
            img_cache: HashMap::new(),
            initial_filter,
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
                Some(Err(_e)) => {
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

fn get_profile_name<'a>(record: &'a ProfileRecord) -> Option<&'a str> {
    let profile = record.record.profile()?;
    let display_name = profile.display_name();
    let name = profile.name();

    if display_name.is_some() && display_name.unwrap() != "" {
        return display_name;
    }

    if name.is_some() && name.unwrap() != "" {
        return name;
    }

    None
}

fn render_note_contents(
    ui: &mut egui::Ui,
    damus: &mut Damus,
    txn: &Transaction,
    note: &Note,
    note_key: NoteKey,
) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let mut images: Vec<String> = vec![];

    ui.horizontal_wrapped(|ui| {
        let blocks = if let Ok(blocks) = damus.ndb.get_blocks_by_key(txn, note_key) {
            blocks
        } else {
            warn!("note content '{}'", note.content());
            ui.weak(note.content());
            return;
        };

        ui.spacing_mut().item_spacing.x = 0.0;

        for block in blocks.iter(note) {
            match block.blocktype() {
                BlockType::MentionBech32 => {
                    ui.colored_label(PURPLE, "@");
                    match block.as_mention().unwrap() {
                        Mention::Pubkey(npub) => {
                            let profile = damus.ndb.get_profile_by_pubkey(txn, npub.pubkey()).ok();
                            if let Some(name) = profile.as_ref().and_then(|p| get_profile_name(p)) {
                                ui.colored_label(PURPLE, name);
                            } else {
                                ui.colored_label(PURPLE, "nostrich");
                            }
                        }
                        _ => {
                            ui.colored_label(PURPLE, block.as_str());
                        }
                    }
                }

                BlockType::Hashtag => {
                    ui.colored_label(PURPLE, "#");
                    ui.colored_label(PURPLE, block.as_str());
                }

                BlockType::Url => {
                    /*
                    let url = block.as_str().to_lowercase();
                    if url.ends_with("png") || url.ends_with("jpg") {
                        images.push(url);
                    } else {
                    */
                    ui.add(Hyperlink::from_label_and_url(
                        RichText::new(block.as_str()).color(PURPLE),
                        block.as_str(),
                    ));
                    //}
                }

                BlockType::Text => {
                    ui.weak(block.as_str());
                }

                _ => {
                    ui.colored_label(PURPLE, block.as_str());
                }
            }
        }
    });

    for image in images {
        let resp = ui.add(Image::new(image.clone()));
        resp.context_menu(|ui| {
            if ui.button("Copy Link").clicked() {
                ui.ctx().copy_text(image);
                ui.close_menu();
            }
        });
    }
}

fn render_note(ui: &mut egui::Ui, damus: &mut Damus, note_key: NoteKey) -> Result<()> {
    let txn = Transaction::new(&damus.ndb)?;
    let note = damus.ndb.get_note_by_key(&txn, note_key)?;

    ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
        let profile = damus.ndb.get_profile_by_pubkey(&txn, note.pubkey());

        padding(6.0, ui, |ui| {
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
                render_username(ui, profile.as_ref().ok(), note.pubkey());

                render_note_contents(ui, damus, &txn, &note, note_key);
            })
        });
    });

    Ok(())
}

fn render_notes(ui: &mut egui::Ui, damus: &mut Damus, timeline: usize) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let num_notes = damus.timelines[timeline].notes.len();

    for i in 0..num_notes {
        let _ = render_note(ui, damus, damus.timelines[timeline].notes[i].key);

        ui.separator();
    }
}

fn timeline_view(ui: &mut egui::Ui, app: &mut Damus, timeline: usize) {
    padding(4.0, ui, |ui| ui.heading("Notifications"));

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
        set_app_style(ui);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
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

            #[cfg(feature = "profiling")]
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

fn set_app_style(ui: &mut egui::Ui) {
    ui.visuals_mut().hyperlink_color = PURPLE;
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
    let _output = egui::TextEdit::multiline(&mut app.compose)
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
