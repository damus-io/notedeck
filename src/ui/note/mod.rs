pub mod contents;
pub mod options;

pub use contents::NoteContents;
pub use options::NoteOptions;

use crate::{colors, ui, Damus};
use egui::{Label, RichText, Sense};

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

        crate::ui::padding(12.0, ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;

                let profile = self.app.ndb.get_profile_by_pubkey(txn, self.note.pubkey());

                match profile
                    .as_ref()
                    .ok()
                    .and_then(|p| p.record().profile()?.picture())
                {
                    // these have different lifetimes and types,
                    // so the calls must be separate
                    Some(pic) => {
                        ui.add(ui::ProfilePic::new(&mut self.app.img_cache, pic));
                    }
                    None => {
                        ui.add(ui::ProfilePic::new(
                            &mut self.app.img_cache,
                            ui::ProfilePic::no_pfp_url(),
                        ));
                    }
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
                });
            });
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

fn render_reltime(
    ui: &mut egui::Ui,
    note_cache: &mut crate::notecache::NoteCache,
    before: bool,
) -> egui::InnerResponse<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        if before {
            ui.add(Label::new(
                RichText::new("⋅").size(10.0).color(colors::GRAY_SECONDARY),
            ));
        }
        ui.add(Label::new(
            RichText::new(note_cache.reltime_str())
                .size(10.0)
                .color(colors::GRAY_SECONDARY),
        ));
        if !before {
            ui.add(Label::new(
                RichText::new("⋅").size(10.0).color(colors::GRAY_SECONDARY),
            ));
        }
    })
}
