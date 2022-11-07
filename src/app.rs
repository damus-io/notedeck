use egui::{Align, Layout, RichText, WidgetText};
use egui_extras::RetainedImage;
use nostr_rust::events::Event;
use poll_promise::Promise;
use std::borrow::{Borrow, Cow};
use std::collections::HashMap;

type ImageCache = HashMap<String, Promise<ehttp::Result<RetainedImage>>>;

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct Damus {
    // Example stuff:
    label: String,

    #[serde(skip)]
    events: Vec<Event>,

    #[serde(skip)]
    img_cache: ImageCache,

    // this how you opt-out of serialization of a member
    #[serde(skip)]
    value: f32,
}

impl Default for Damus {
    fn default() -> Self {
        Self {
            // Example stuff:
            label: "Hello World!".to_owned(),
            events: vec![],
            img_cache: HashMap::new(),
            value: 2.7,
        }
    }
}

impl Damus {
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // This is also where you can customized the look at feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        if let Some(storage) = cc.storage {
            return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        }

        Default::default()
    }
}

#[allow(clippy::needless_pass_by_value)]
fn parse_response(response: ehttp::Response) -> Result<RetainedImage, String> {
    let content_type = response.content_type().unwrap_or_default();

    if content_type.starts_with("image/svg+xml") {
        RetainedImage::from_svg_bytes(&response.url, &response.bytes)
    } else if content_type.starts_with("image/") {
        RetainedImage::from_image_bytes(&response.url, &response.bytes)
    } else {
        Err(format!(
            "Expected image, found content-type {:?}",
            content_type
        ))
    }
}

fn fetch_img(ctx: &egui::Context, url: &str) -> Promise<ehttp::Result<RetainedImage>> {
    let (sender, promise) = Promise::new();
    let request = ehttp::Request::get(url);
    let ctx = ctx.clone();
    ehttp::fetch(request, move |response| {
        let image = response.and_then(parse_response);
        sender.send(image); // send the results back to the UI thread. ctx.request_repaint();
    });
    promise
}

fn render_pfp(ctx: &egui::Context, img_cache: &mut ImageCache, ui: &mut egui::Ui, url: String) {
    let m_cached_promise = img_cache.get_mut(&url);
    if m_cached_promise.is_none() {
        img_cache.insert(url.clone(), fetch_img(ctx, &url));
    }

    match img_cache[&url].ready() {
        None => {
            ui.spinner(); // still loading
        }
        Some(Err(err)) => {
            ui.colored_label(ui.visuals().error_fg_color, err); // something went wrong
        }
        Some(Ok(image)) => {
            image.show_max_size(ui, egui::vec2(64.0, 64.0));
        }
    }
}

fn render_event(ctx: &egui::Context, img_cache: &mut ImageCache, ui: &mut egui::Ui, ev: &Event) {
    render_pfp(
        ctx,
        img_cache,
        ui,
        //"https://damus.io/img/damus.svg".into(),
        "http://cdn.jb55.com/img/red-me.jpg".into(),
    );
    ui.label(&ev.content);
}

fn render_events(ctx: &egui::Context, app: &mut Damus, ui: &mut egui::Ui) {
    for ev in &app.events {
        ui.spacing_mut().item_spacing.y = 10.0;
        render_event(ctx, &mut app.img_cache, ui, ev);
    }
}

fn timeline_view(ctx: &egui::Context, app: &mut Damus, ui: &mut egui::Ui) {
    ui.heading("Timeline");

    egui::ScrollArea::vertical().show(ui, |ui| {
        render_events(ctx, app, ui);
    });

    /*
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("powered by ");
            ui.hyperlink_to("egui", "https://github.com/emilk/egui");
            ui.label(" and ");
            ui.hyperlink_to(
                "eframe",
                "https://github.com/emilk/egui/tree/master/crates/eframe",
            );
            ui.label(".");
        });
    });
    */
}

fn render_damus(ctx: &egui::Context, _frame: &mut eframe::Frame, app: &mut Damus) {
    #[cfg(not(target_arch = "wasm32"))] // no File->Quit on web pages!
    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
        // The top panel is often a good place for a menu bar:
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Quit").clicked() {
                    _frame.close();
                }
            });
        });
    });

    egui::SidePanel::left("side_panel").show(ctx, |ui| timeline_view(ctx, app, ui));

    egui::CentralPanel::default().show(ctx, |ui| {
        // The central panel the region left after adding TopPanel's and SidePanel's

        ui.heading("eframe template");
        ui.hyperlink("https://github.com/emilk/eframe_template");
        ui.add(egui::github_link_file!(
            "https://github.com/emilk/eframe_template/blob/master/",
            "Source code."
        ));
        egui::warn_if_debug_build(ui);
    });
}

impl eframe::App for Damus {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    /// Put your widgets into a `SidePanel`, `TopPanel`, `CentralPanel`, `Window` or `Area`.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Examples of how to create different panels and windows.
        // Pick whichever suits you.
        // Tip: a good default choice is to just keep the `CentralPanel`.
        // For inspiration and more examples, go to https://emilk.github.io/egui

        let test_event = Event {
            id: "6938e3cd841f3111dbdbd909f87fd52c3d1f1e4a07fd121d1243196e532811cb".to_string(),
            pub_key: "f0a6ff7f70b872de6d82c8daec692a433fd23b6a49f25923c6f034df715cdeec".to_string(),
            created_at: 1667781968,
            kind: 1,
            tags: vec![],
            content: "yello\nthere".to_string(),
            sig: "af02c971015995f79e07fa98aaf98adeeb6a56d0005e451ee4e78844cff712a6bc0f2109f72a878975f162dcefde4173b65ebd4c3d3ab3b520a9dcac6acf092d".to_string(),
        };

        if self.events.len() == 0 {
            self.events.push(test_event.clone());
            println!("{}", &self.events[0].content);
            self.events.push(test_event.clone());
            println!("{}", &self.events[1].content);
            self.events.push(test_event.clone());
            println!("{}", &self.events[2].content);
        }

        render_damus(ctx, _frame, self);
    }
}
