//use egui::{Align, Layout, RichText, WidgetText};
use egui_extras::RetainedImage;
//use nostr_rust::events::Event;
use poll_promise::Promise;
//use std::borrow::{Borrow, Cow};
use std::collections::HashMap;

use crate::Event;

type ImageCache = HashMap<String, Promise<ehttp::Result<RetainedImage>>>;

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct Damus {
    // Example stuff:
    label: String,

    n_panels: u32,

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
            n_panels: 1,
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

    if content_type.starts_with("image/svg") {
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

fn render_pfp(ui: &mut egui::Ui, img_cache: &mut ImageCache, url: String) {
    let m_cached_promise = img_cache.get_mut(&url);
    if m_cached_promise.is_none() {
        img_cache.insert(url.clone(), fetch_img(ui.ctx(), &url));
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

fn render_username(ui: &mut egui::Ui, pk: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label(&pk[0..8]);
        ui.label(":");
        ui.label(&pk[64 - 8..]);
    });
}

fn render_event(ui: &mut egui::Ui, img_cache: &mut ImageCache, ev: &Event) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
        let damus_pic = "https://damus.io/img/damus.svg".into();
        let jb55_pic = "https://damus.io/img/red-me.jpg".into();
        let pic =
            if ev.pub_key == "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245" {
                jb55_pic
            } else {
                damus_pic
            };

        render_pfp(ui, img_cache, pic);

        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            render_username(ui, &ev.pub_key);

            ui.label(&ev.content);
        })
    });
}

fn timeline_view(ui: &mut egui::Ui, app: &mut Damus) {
    ui.heading("Timeline");

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for ev in &app.events {
                ui.separator();
                render_event(ui, &mut app.img_cache, ev);
            }
        });
}

fn render_damus(ctx: &egui::Context, _frame: &mut eframe::Frame, app: &mut Damus) {
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

            if app.n_panels != 1 {
                if ui
                    .add(egui::Button::new("-").frame(false))
                    .on_hover_text("Add Timeline")
                    .clicked()
                {
                    app.n_panels -= 1;
                }
            }
        });
    });

    let screen_size = ctx.input().screen_rect.width();
    let calc_panel_width = (screen_size / app.n_panels as f32) - 30.0;
    let min_width = 300.0;
    let need_scroll = calc_panel_width < min_width;
    let panel_width = if need_scroll {
        min_width
    } else {
        calc_panel_width
    };

    egui::CentralPanel::default().show(ctx, |ui| {
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
        .max_width(panel_width)
        .min_width(panel_width)
        .show_inside(ui, |ui| {
            timeline_view(ui, app);
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
            content: LOREM_IPSUM.into(),
            sig: "af02c971015995f79e07fa98aaf98adeeb6a56d0005e451ee4e78844cff712a6bc0f2109f72a878975f162dcefde4173b65ebd4c3d3ab3b520a9dcac6acf092d".to_string(),
        };

        let test_event2 = Event {
            id: "6938e3cd841f3111dbdbd909f87fd52c3d1f1e4a07fd121d1243196e532811cb".to_string(),
            pub_key: "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".to_string(),
            created_at: 1667781968,
            kind: 1,
            tags: vec![],
            content: LOREM_IPSUM_LONG.into(),
            sig: "af02c971015995f79e07fa98aaf98adeeb6a56d0005e451ee4e78844cff712a6bc0f2109f72a878975f162dcefde4173b65ebd4c3d3ab3b520a9dcac6acf092d".to_string(),
        };

        if self.events.len() == 0 {
            self.events.push(test_event.clone());
            self.events.push(test_event2.clone());
            self.events.push(test_event.clone());
            self.events.push(test_event2.clone());
            self.events.push(test_event.clone());
            self.events.push(test_event2.clone());
            self.events.push(test_event.clone());
            self.events.push(test_event2.clone());
            self.events.push(test_event.clone());
        }

        render_damus(ctx, _frame, self);
    }
}

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

pub const LOREM_IPSUM_LONG: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.

Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra, est eros bibendum elit, nec luctus magna felis sollicitudin mauris. Integer in mauris eu nibh euismod gravida. Duis ac tellus et risus vulputate vehicula. Donec lobortis risus a elit. Etiam tempor. Ut ullamcorper, ligula eu tempor congue, eros est euismod turpis, id tincidunt sapien risus a quam. Maecenas fermentum consequat mi. Donec fermentum. Pellentesque malesuada nulla a mi. Duis sapien sem, aliquet nec, commodo eget, consequat quis, neque. Aliquam faucibus, elit ut dictum aliquet, felis nisl adipiscing sapien, sed malesuada diam lacus eget erat. Cras mollis scelerisque nunc. Nullam arcu. Aliquam consequat. Curabitur augue lorem, dapibus quis, laoreet et, pretium ac, nisi. Aenean magna nisl, mollis quis, molestie eu, feugiat in, orci. In hac habitasse platea dictumst.";
