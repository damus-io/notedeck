pub mod contents;
pub mod options;

pub use contents::NoteContents;
pub use options::NoteOptions;

use crate::{colors, notecache::CachedNote, ui, ui::is_mobile, Damus};
use egui::{Label, RichText, Sense};
use nostrdb::{NoteKey, Transaction};
use std::hash::{Hash, Hasher};

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

#[derive(Eq, PartialEq, Debug, Clone, Copy)]
struct ProfileAnimId {
    profile_key: u64,
    note_key: u64,
}

impl Hash for ProfileAnimId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u8(0x12);
        self.profile_key.hash(state);
        self.note_key.hash(state);
    }
}

fn reply_desc(
    ui: &mut egui::Ui,
    txn: &Transaction,
    app: &mut Damus,
    note_key: NoteKey,
    note: &nostrdb::Note<'_>,
) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let note_reply = app
        .note_cache_mut()
        .cached_note_or_insert_mut(note_key, note)
        .reply
        .borrow(note.tags());

    let reply = if let Some(reply) = note_reply.reply() {
        reply
    } else {
        // not a reply, nothing to do here
        return;
    };

    ui.add(Label::new(
        RichText::new("replying to")
            .size(10.0)
            .color(colors::GRAY_SECONDARY),
    ));

    let reply_note = if let Ok(reply_note) = app.ndb.get_note_by_id(txn, reply.id) {
        reply_note
    } else {
        ui.add(Label::new(
            RichText::new("a note")
                .size(10.0)
                .color(colors::GRAY_SECONDARY),
        ));
        return;
    };

    if note_reply.is_reply_to_root() {
        // We're replying to the root, let's show this
        ui.add(ui::Mention::new(app, txn, reply_note.pubkey()).size(10.0));
        ui.add(Label::new(
            RichText::new("'s note")
                .size(10.0)
                .color(colors::GRAY_SECONDARY),
        ));
    } else if let Some(root) = note_reply.root() {
        // replying to another post in a thread, not the root

        if let Ok(root_note) = app.ndb.get_note_by_id(txn, root.id) {
            if root_note.pubkey() == reply_note.pubkey() {
                // simply "replying to bob's note" when replying to bob in his thread
                ui.add(ui::Mention::new(app, txn, reply_note.pubkey()).size(10.0));
                ui.add(Label::new(
                    RichText::new("'s note")
                        .size(10.0)
                        .color(colors::GRAY_SECONDARY),
                ));
            } else {
                // replying to bob in alice's thread

                ui.add(ui::Mention::new(app, txn, reply_note.pubkey()).size(10.0));
                ui.add(Label::new(
                    RichText::new("in").size(10.0).color(colors::GRAY_SECONDARY),
                ));
                ui.add(ui::Mention::new(app, txn, root_note.pubkey()).size(10.0));
                ui.add(Label::new(
                    RichText::new("'s thread")
                        .size(10.0)
                        .color(colors::GRAY_SECONDARY),
                ));
            }
        } else {
            ui.add(ui::Mention::new(app, txn, reply_note.pubkey()).size(10.0));
            ui.add(Label::new(
                RichText::new("in someone's thread")
                    .size(10.0)
                    .color(colors::GRAY_SECONDARY),
            ));
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

            //ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;

            let cached_note = self
                .app
                .note_cache_mut()
                .cached_note_or_insert_mut(note_key, self.note);

            let (_id, rect) = ui.allocate_space(egui::vec2(50.0, 20.0));
            ui.allocate_rect(rect, Sense::hover());
            ui.put(rect, |ui: &mut egui::Ui| {
                render_reltime(ui, cached_note, false).response
            });
            let (_id, rect) = ui.allocate_space(egui::vec2(150.0, 20.0));
            ui.allocate_rect(rect, Sense::hover());
            ui.put(rect, |ui: &mut egui::Ui| {
                ui.add(
                    ui::Username::new(profile.as_ref().ok(), self.note.pubkey())
                        .abbreviated(6)
                        .pk_colored(true),
                )
            });

            ui.add(NoteContents::new(
                self.app, txn, self.note, note_key, self.flags,
            ));
            //});
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
                        let expand_size = 5.0;
                        let anim_speed = 0.05;
                        let profile_key = profile.as_ref().unwrap().record().note_key();
                        let note_key = note_key.as_u64();

                        if is_mobile() {
                            ui.add(ui::ProfilePic::new(&mut self.app.img_cache, pic));
                        } else {
                            let (rect, size) = ui::anim::hover_expand(
                                ui,
                                egui::Id::new(ProfileAnimId {
                                    profile_key,
                                    note_key,
                                }),
                                ui::ProfilePic::default_size(),
                                expand_size,
                                anim_speed,
                            );

                            ui.put(
                                rect,
                                ui::ProfilePic::new(&mut self.app.img_cache, pic).size(size),
                            )
                            .on_hover_ui_at_pointer(|ui| {
                                ui.set_max_width(300.0);
                                ui.add(ui::ProfilePreview::new(
                                    profile.as_ref().unwrap(),
                                    &mut self.app.img_cache,
                                ));
                            });
                        }
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

                        let cached_note = self
                            .app
                            .note_cache_mut()
                            .cached_note_or_insert_mut(note_key, self.note);
                        render_reltime(ui, cached_note, true);
                    });

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        reply_desc(ui, txn, self.app, note_key, self.note);
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

fn secondary_label(ui: &mut egui::Ui, s: impl Into<String>) {
    ui.add(Label::new(
        RichText::new(s).size(10.0).color(colors::GRAY_SECONDARY),
    ));
}

fn render_reltime(
    ui: &mut egui::Ui,
    note_cache: &mut CachedNote,
    before: bool,
) -> egui::InnerResponse<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        if before {
            secondary_label(ui, "⋅");
        }

        secondary_label(ui, note_cache.reltime_str());

        if !before {
            secondary_label(ui, "⋅");
        }
    })
}
