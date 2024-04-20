pub mod contents;
pub mod options;

pub use contents::NoteContents;
pub use options::NoteOptions;

use crate::{ui, Damus};
use egui::{Color32, Label, RichText, Sense, TextureHandle, Vec2};

pub struct Note<'a> {
    app: &'a mut Damus,
    note: &'a nostrdb::Note<'a>,
    flags: NoteOptions,
}

impl<'a> egui::Widget for Note<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        if self.app.textmode {
            self.textmode_ui(ui)
        } else {
            self.standard_ui(ui)
        }
    }
}

impl<'a> Note<'a> {
    pub fn new(app: &'a mut Damus, note: &'a nostrdb::Note<'a>) -> Self {
        let flags = NoteOptions::actionbar | NoteOptions::note_previews;
        Note { app, note, flags }
    }

    pub fn actionbar(mut self, enable: bool) -> Self {
        self.options_mut().set_actionbar(enable);
        self
    }

    pub fn note_previews(mut self, enable: bool) -> Self {
        self.options_mut().set_note_previews(enable);
        self
    }

    pub fn options(&self) -> NoteOptions {
        self.flags
    }

    pub fn options_mut(&mut self) -> &mut NoteOptions {
        &mut self.flags
    }

    fn textmode_ui(self, ui: &mut egui::Ui) -> egui::Response {
        let note_key = self.note.key().expect("todo: implement non-db notes");
        let txn = self.note.txn().expect("todo: implement non-db notes");

        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
            let profile = self.app.ndb.get_profile_by_pubkey(txn, self.note.pubkey());

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;

                let note_cache = self
                    .app
                    .get_note_cache_mut(note_key, self.note.created_at());

                let (_id, rect) = ui.allocate_space(egui::vec2(50.0, 20.0));
                ui.allocate_rect(rect, Sense::hover());
                ui.put(rect, |ui: &mut egui::Ui| {
                    render_reltime(ui, note_cache, false).response
                });
                let (_id, rect) = ui.allocate_space(egui::vec2(150.0, 20.0));
                ui.allocate_rect(rect, Sense::hover());
                ui.put(rect, |ui: &mut egui::Ui| {
                    ui.add(
                        ui::Username::new(profile.as_ref().ok(), self.note.pubkey())
                            .abbreviated(8)
                            .pk_colored(true),
                    )
                });

                ui.add(NoteContents::new(
                    self.app, txn, self.note, note_key, self.flags,
                ));
            });
        })
        .response
    }

    pub fn standard_ui(self, ui: &mut egui::Ui) -> egui::Response {
        #[cfg(feature = "profiling")]
        puffin::profile_function!();
        let note_key = self.note.key().expect("todo: support non-db notes");
        let txn = self.note.txn().expect("todo: support non-db notes");

        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
            let profile = self.app.ndb.get_profile_by_pubkey(txn, self.note.pubkey());

            /*
            let mut collapse_state =
                egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    false,
                );
                */

            crate::ui::padding(6.0, ui, |ui| {
                match profile
                    .as_ref()
                    .ok()
                    .and_then(|p| p.record().profile()?.picture())
                {
                    // these have different lifetimes and types,
                    // so the calls must be separate
                    Some(pic) => render_pfp(ui, self.app, pic),
                    None => render_pfp(ui, self.app, no_pfp_url()),
                }

                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.add(
                            ui::Username::new(profile.as_ref().ok(), self.note.pubkey())
                                .abbreviated(20),
                        );

                        let note_cache = self
                            .app
                            .get_note_cache_mut(note_key, self.note.created_at());
                        render_reltime(ui, note_cache, true);
                    });

                    ui.add(NoteContents::new(
                        self.app,
                        txn,
                        self.note,
                        note_key,
                        self.options(),
                    ));

                    if self.options().has_actionbar() {
                        render_note_actionbar(ui);
                    }

                    //let header_res = ui.horizontal(|ui| {});
                });
            });

            //let resp = ui.interact(inner_resp.response.rect, id, Sense::hover());

            //if resp.hovered() ^ collapse_state.is_open() {
            //info!("clicked {:?}, {}", self.note_key, collapse_state.is_open());
            //collapse_state.toggle(ui);
            //collapse_state.store(ui.ctx());
            //}
        })
        .response
    }
}

fn render_note_actionbar(ui: &mut egui::Ui) -> egui::InnerResponse<()> {
    ui.horizontal(|ui| {
        let img_data = if ui.style().visuals.dark_mode {
            egui::include_image!("../../../assets/icons/reply.png")
        } else {
            egui::include_image!("../../../assets/icons/reply-dark.png")
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

// TODO: move to widget
fn render_pfp(ui: &mut egui::Ui, damus: &mut Damus, url: &str) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let ui_size = 30.0;

    // We will want to downsample these so it's not blurry on hi res displays
    let img_size = (ui_size * 2.0) as u32;

    let m_cached_promise = damus.img_cache.map().get(url);
    if m_cached_promise.is_none() {
        let res = crate::images::fetch_img(&damus.img_cache, ui.ctx(), url, img_size);
        damus.img_cache.map_mut().insert(url.to_owned(), res);
    }

    match damus.img_cache.map()[url].ready() {
        None => {
            ui.add(egui::Spinner::new().size(ui_size));
        }

        // Failed to fetch profile!
        Some(Err(_err)) => {
            let m_failed_promise = damus.img_cache.map().get(url);
            if m_failed_promise.is_none() {
                let no_pfp =
                    crate::images::fetch_img(&damus.img_cache, ui.ctx(), no_pfp_url(), img_size);
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

fn pfp_image(ui: &mut egui::Ui, img: &TextureHandle, size: f32) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    //img.show_max_size(ui, egui::vec2(size, size))
    ui.add(egui::Image::new(img).max_width(size))
    //.with_options()
}

fn no_pfp_url() -> &'static str {
    "https://damus.io/img/no-profile.svg"
}

fn paint_circle(ui: &mut egui::Ui, size: f32) {
    let (rect, _response) = ui.allocate_at_least(Vec2::new(size, size), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, ui.visuals().weak_text_color());
}

fn render_reltime(
    ui: &mut egui::Ui,
    note_cache: &mut crate::notecache::NoteCache,
    before: bool,
) -> egui::InnerResponse<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        let color = Color32::from_rgb(0x8A, 0x8A, 0x8A);
        if before {
            ui.add(Label::new(RichText::new("⋅").size(10.0).color(color)));
        }
        ui.add(Label::new(
            RichText::new(note_cache.reltime_str())
                .size(10.0)
                .color(color),
        ));
        if !before {
            ui.add(Label::new(RichText::new("⋅").size(10.0).color(color)));
        }
    })
}
