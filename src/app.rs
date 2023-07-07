use crate::contacts::Contacts;
use crate::fonts::setup_fonts;
use crate::images::fetch_img;
use crate::ui::padding;
use crate::Result;
use egui::containers::scroll_area::ScrollBarVisibility;
use egui::widgets::Spinner;
use egui::{Context, Frame, TextureHandle, TextureId};
use enostr::{ClientMessage, Filter, NoteId, Profile, Pubkey, RelayEvent, RelayMessage};
use poll_promise::Promise;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
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

#[derive(Eq, PartialEq, Clone)]
pub enum DamusState {
    Initializing,
    Initialized,
}

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
pub struct Damus {
    state: DamusState,
    contacts: Contacts,
    n_panels: u32,

    pool: RelayPool,

    all_events: HashMap<NoteId, Event>,
    events: Vec<NoteId>,

    img_cache: ImageCache,
}

impl Default for Damus {
    fn default() -> Self {
        Self {
            state: DamusState::Initializing,
            contacts: Contacts::new(),
            all_events: HashMap::new(),
            pool: RelayPool::default(),
            events: Vec::with_capacity(2000),
            img_cache: HashMap::new(),
            n_panels: 1,
        }
    }
}

pub fn is_mobile(ctx: &egui::Context) -> bool {
    let screen_size = ctx.screen_rect().size();
    screen_size.x < 550.0
}

fn relay_setup(pool: &mut RelayPool, ctx: &egui::Context) {
    let ctx = ctx.clone();
    let wakeup = move || {
        debug!("Woke up");
        ctx.request_repaint();
    };
    if let Err(e) = pool.add_url("wss://relay.damus.io".to_string(), wakeup) {
        error!("{:?}", e)
    }
}

fn send_initial_filters(pool: &mut RelayPool, relay_url: &str) {
    let filter = Filter::new().limit(100).kinds(vec![1, 42]).pubkeys(
        ["32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".into()].into(),
    );

    let subid = "initial";
    for relay in &mut pool.relays {
        if relay.url == relay_url {
            relay.subscribe(subid.to_string(), vec![filter]);
            return;
        }
    }
}

fn try_process_event(damus: &mut Damus, ctx: &egui::Context) {
    let amount = 0.2;
    if ctx.input(|i| i.key_pressed(egui::Key::PlusEquals)) {
        ctx.set_pixels_per_point(ctx.pixels_per_point() + amount);
    } else if ctx.input(|i| i.key_pressed(egui::Key::Minus)) {
        ctx.set_pixels_per_point(ctx.pixels_per_point() - amount);
    }

    // pool stuff
    if let Some(ev) = damus.pool.try_recv() {
        let relay = ev.relay.to_owned();

        match ev.event {
            RelayEvent::Opened => send_initial_filters(&mut damus.pool, &relay),
            // TODO: handle reconnects
            RelayEvent::Closed => warn!("{} connection closed", &relay),
            RelayEvent::Other(msg) => debug!("other event {:?}", &msg),
            RelayEvent::Message(msg) => process_message(damus, &relay, msg),
        }
    }
    //info!("recv {:?}", ev)
}

fn update_damus(damus: &mut Damus, ctx: &egui::Context) {
    if damus.state == DamusState::Initializing {
        setup_fonts(ctx);
        damus.pool = RelayPool::new();
        relay_setup(&mut damus.pool, ctx);
        damus.state = DamusState::Initialized;
    }

    try_process_event(damus, ctx);
}

fn process_metadata_event(damus: &mut Damus, ev: &Event) {
    if let Some(prev_id) = damus.contacts.events.get(&ev.pubkey) {
        if let Some(prev_ev) = damus.all_events.get(prev_id) {
            // This profile event is older, ignore it
            if prev_ev.created_at >= ev.created_at {
                return;
            }
        }
    }

    let profile: core::result::Result<serde_json::Value, serde_json::Error> =
        serde_json::from_str(&ev.content);

    match profile {
        Err(e) => {
            debug!("Invalid profile data '{}': {:?}", &ev.content, &e);
        }
        Ok(v) if !v.is_object() => {
            debug!("Invalid profile data: '{}'", &ev.content);
        }
        Ok(profile) => {
            damus
                .contacts
                .events
                .insert(ev.pubkey.clone(), ev.id.clone());

            damus
                .contacts
                .profiles
                .insert(ev.pubkey.clone(), Profile::new(profile));
        }
    }
}

fn process_event(damus: &mut Damus, _subid: &str, event: Event) {
    if damus.all_events.get(&event.id).is_some() {
        return;
    }

    if event.kind == 0 {
        process_metadata_event(damus, &event);
    }

    let cloned_id = event.id.clone();
    damus.all_events.insert(cloned_id.clone(), event);
    damus.events.push(cloned_id);
}

fn get_unknown_author_ids(damus: &Damus) -> Vec<Pubkey> {
    let mut authors: HashSet<Pubkey> = HashSet::new();

    for (_evid, ev) in damus.all_events.iter() {
        if !damus.contacts.profiles.contains_key(&ev.pubkey) {
            authors.insert(ev.pubkey.clone());
        }
    }

    authors.into_iter().collect()
}

fn handle_eose(damus: &mut Damus, subid: &str, relay_url: &str) {
    if subid == "initial" {
        let authors = get_unknown_author_ids(damus);
        let n_authors = authors.len();
        let filter = Filter::new().authors(authors).kinds(vec![0]);
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
    }
}

fn process_message(damus: &mut Damus, relay: &str, msg: RelayMessage) {
    match msg {
        RelayMessage::Event(subid, ev) => process_event(damus, &subid, ev),
        RelayMessage::Notice(msg) => warn!("Notice from {}: {}", relay, msg),
        RelayMessage::OK(cr) => info!("OK {:?}", cr),
        RelayMessage::Eose(sid) => handle_eose(damus, &sid, relay),
    }
}

fn render_damus(damus: &mut Damus, ctx: &Context) {
    if is_mobile(ctx) {
        render_damus_mobile(ctx, damus);
    } else {
        render_damus_desktop(ctx, damus);
    }
}

impl Damus {
    pub fn add_test_events(&mut self) {
        add_test_events(self);
    }

    /// Called once before the first frame.
    pub fn new() -> Self {
        // This is also where you can customized the look at feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        //if let Some(storage) = cc.storage {
        //return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        //}

        Default::default()
    }
}

fn render_pfp(ui: &mut egui::Ui, img_cache: &mut ImageCache, url: &str) {
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
            debug!(
                "has failed promise? {}",
                img_cache.contains_key(&failed_key)
            );
            let m_failed_promise = img_cache.get_mut(&failed_key);
            if m_failed_promise.is_none() {
                warn!("failed key: {:?}", &failed_key);
                let no_pfp = fetch_img(ui.ctx(), no_pfp_url());
                img_cache.insert(failed_key, no_pfp);
            }

            match img_cache[&failed_key].ready() {
                None => {
                    ui.spinner(); // still loading
                }
                Some(Err(e)) => {
                    error!("Image load error: {:?}", e);
                    ui.label("❌");
                }
                Some(Ok(img)) => {
                    pfp_image(ui, img.into(), pfp_size);
                }
            }
        }
        Some(Ok(img)) => {
            pfp_image(ui, img.into(), pfp_size);
        }
    }
}

fn pfp_image(ui: &mut egui::Ui, img: TextureId, size: f32) -> egui::Response {
    //img.show_max_size(ui, egui::vec2(size, size))
    ui.image(img, egui::vec2(size, size))
    //.with_options()
}

fn render_username(ui: &mut egui::Ui, contacts: &Contacts, pk: &Pubkey) {
    ui.horizontal(|ui| {
        //ui.spacing_mut().item_spacing.x = 0.0;
        if let Some(prof) = contacts.profiles.get(pk) {
            if let Some(display_name) = prof.display_name() {
                ui.strong(display_name);
            }
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

fn render_events(ui: &mut egui::Ui, damus: &mut Damus) {
    for evid in &damus.events {
        if !damus.all_events.contains_key(evid) {
            return;
        }

        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
            let ev = damus.all_events.get(evid).unwrap();

            padding(10.0, ui, |ui| {
                match damus
                    .contacts
                    .profiles
                    .get(&ev.pubkey)
                    .and_then(|p| p.picture())
                {
                    // these have different lifetimes and types,
                    // so the calls must be separate
                    Some(pic) => render_pfp(ui, &mut damus.img_cache, pic),
                    None => render_pfp(ui, &mut damus.img_cache, no_pfp_url()),
                }

                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    render_username(ui, &damus.contacts, &ev.pubkey);

                    ui.weak(&ev.content);
                })
            })
        });

        ui.separator();
    }
}

fn timeline_view(ui: &mut egui::Ui, app: &mut Damus) {
    padding(10.0, ui, |ui| ui.heading("Timeline"));

    egui::ScrollArea::vertical()
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            render_events(ui, app);
        });
}

fn render_panel(ctx: &egui::Context, app: &mut Damus) {
    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
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
        });
    });
}

fn set_app_style(ui: &mut egui::Ui) {
    if ui.visuals().dark_mode {
        ui.visuals_mut().override_text_color = Some(egui::Color32::WHITE);
        ui.visuals_mut().panel_fill = egui::Color32::from_rgb(30, 30, 30);
    } else {
        ui.visuals_mut().override_text_color = Some(egui::Color32::BLACK);
    };
}

fn render_damus_mobile(ctx: &egui::Context, app: &mut Damus) {
    let panel_width = ctx.screen_rect().width();
    egui::CentralPanel::default().show(ctx, |ui| {
        set_app_style(ui);
        timeline_panel(ui, app, panel_width, 0);
    });
}

fn render_damus_desktop(ctx: &egui::Context, app: &mut Damus) {
    render_panel(ctx, app);

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
            timeline_panel(ui, app, panel_width, 0);
        });

        return;
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        set_app_style(ui);
        egui::ScrollArea::horizontal()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for ind in 0..app.n_panels {
                    timeline_panel(ui, app, panel_width, ind);
                }
            });
    });
}

fn timeline_panel(ui: &mut egui::Ui, app: &mut Damus, panel_width: f32, ind: u32) {
    egui::SidePanel::left(format!("l{}", ind))
        .resizable(false)
        .frame(Frame::none())
        .max_width(panel_width)
        .min_width(panel_width)
        .show_inside(ui, |ui| {
            timeline_view(ui, app);
        });
}

fn add_test_events(damus: &mut Damus) {
    // Examples of how to create different panels and windows.
    // Pick whichever suits you.
    // Tip: a good default choice is to just keep the `CentralPanel`.
    // For inspiration and more examples, go to https://emilk.github.io/egui

    let test_event = Event {
        id: "6938e3cd841f3111dbdbd909f87fd52c3d1f1e4a07fd121d1243196e532811cb".try_into().unwrap(),
        pubkey: "f0a6ff7f70b872de6d82c8daec692a433fd23b6a49f25923c6f034df715cdeec".try_into().unwrap(),
        created_at: 1667781968,
        kind: 1,
        tags: vec![],
        content: LOREM_IPSUM.into(),
        sig: "af02c971015995f79e07fa98aaf98adeeb6a56d0005e451ee4e78844cff712a6bc0f2109f72a878975f162dcefde4173b65ebd4c3d3ab3b520a9dcac6acf092d".to_string(),
    };

    let test_event2 = Event {
        id: "6938e3cd841f3111dbdbd909f87fd52c3d1f1e4a07fd121d1243196e532811cb".try_into().unwrap(),
        pubkey: "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".try_into().unwrap(),
        created_at: 1667781968,
        kind: 1,
        tags: vec![],
        content: LOREM_IPSUM_LONG.into(),
        sig: "af02c971015995f79e07fa98aaf98adeeb6a56d0005e451ee4e78844cff712a6bc0f2109f72a878975f162dcefde4173b65ebd4c3d3ab3b520a9dcac6acf092d".to_string(),
    };

    damus
        .all_events
        .insert(test_event.id.clone(), test_event.clone());
    damus
        .all_events
        .insert(test_event2.id.clone(), test_event2.clone());

    if damus.events.is_empty() {
        damus.events.push(test_event.id.clone());
        damus.events.push(test_event2.id.clone());
        damus.events.push(test_event.id.clone());
        damus.events.push(test_event2.id.clone());
        damus.events.push(test_event.id.clone());
        damus.events.push(test_event2.id.clone());
        damus.events.push(test_event.id.clone());
        damus.events.push(test_event2.id);
        damus.events.push(test_event.id);
    }
}

impl eframe::App for Damus {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        //eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    /// Put your widgets into a `SidePanel`, `TopPanel`, `CentralPanel`, `Window` or `Area`.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        update_damus(self, ctx);
        render_damus(self, ctx);
    }
}

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

pub const LOREM_IPSUM_LONG: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.

Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra, est eros bibendum elit, nec luctus magna felis sollicitudin mauris. Integer in mauris eu nibh euismod gravida. Duis ac tellus et risus vulputate vehicula. Donec lobortis risus a elit. Etiam tempor. Ut ullamcorper, ligula eu tempor congue, eros est euismod turpis, id tincidunt sapien risus a quam. Maecenas fermentum consequat mi. Donec fermentum. Pellentesque malesuada nulla a mi. Duis sapien sem, aliquet nec, commodo eget, consequat quis, neque. Aliquam faucibus, elit ut dictum aliquet, felis nisl adipiscing sapien, sed malesuada diam lacus eget erat. Cras mollis scelerisque nunc. Nullam arcu. Aliquam consequat. Curabitur augue lorem, dapibus quis, laoreet et, pretium ac, nisi. Aenean magna nisl, mollis quis, molestie eu, feugiat in, orci. In hac habitasse platea dictumst.";
