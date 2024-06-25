pub mod contents;
pub mod options;
pub mod post;
pub mod reply;

pub use contents::NoteContents;
pub use options::NoteOptions;
pub use post::PostView;
pub use reply::PostReplyView;

use crate::{colors, notecache::CachedNote, ui, ui::View, Damus};
use egui::{Label, RichText, Sense};
use nostrdb::{NoteKey, Transaction};

pub struct Note<'a> {
    app: &'a mut Damus,
    note: &'a nostrdb::Note<'a>,
    flags: NoteOptions,
}

pub struct NoteResponse {
    pub response: egui::Response,
    pub action: Option<BarAction>,
}

impl<'a> View for Note<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
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

    pub fn small_pfp(mut self, enable: bool) -> Self {
        self.options_mut().set_small_pfp(enable);
        self
    }

    pub fn medium_pfp(mut self, enable: bool) -> Self {
        self.options_mut().set_medium_pfp(enable);
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

    fn textmode_ui(&mut self, ui: &mut egui::Ui) -> egui::Response {
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

    pub fn expand_size() -> f32 {
        5.0
    }

    fn pfp(
        &mut self,
        note_key: NoteKey,
        profile: &Result<nostrdb::ProfileRecord<'_>, nostrdb::Error>,
        ui: &mut egui::Ui,
    ) {
        ui.spacing_mut().item_spacing.x = 16.0;

        let pfp_size = if self.options().has_small_pfp() {
            ui::ProfilePic::small_size()
        } else if self.options().has_medium_pfp() {
            ui::ProfilePic::medium_size()
        } else {
            ui::ProfilePic::default_size()
        };

        match profile
            .as_ref()
            .ok()
            .and_then(|p| p.record().profile()?.picture())
        {
            // these have different lifetimes and types,
            // so the calls must be separate
            Some(pic) => {
                let anim_speed = 0.05;
                let profile_key = profile.as_ref().unwrap().record().note_key();
                let note_key = note_key.as_u64();

                if self.app.is_mobile() {
                    ui.add(ui::ProfilePic::new(&mut self.app.img_cache, pic));
                } else {
                    let (rect, size, _resp) = ui::anim::hover_expand(
                        ui,
                        egui::Id::new((profile_key, note_key)),
                        pfp_size,
                        ui::Note::expand_size(),
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
                ui.add(
                    ui::ProfilePic::new(&mut self.app.img_cache, ui::ProfilePic::no_pfp_url())
                        .size(pfp_size),
                );
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        if self.app.textmode {
            NoteResponse {
                response: self.textmode_ui(ui),
                action: None,
            }
        } else {
            self.show_standard(ui)
        }
    }

    fn show_standard(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        #[cfg(feature = "profiling")]
        puffin::profile_function!();
        let note_key = self.note.key().expect("todo: support non-db notes");
        let txn = self.note.txn().expect("todo: support non-db notes");
        let mut note_action: Option<BarAction> = None;
        let profile = self.app.ndb.get_profile_by_pubkey(txn, self.note.pubkey());

        if self.options().has_wide() {
            ui.horizontal_centered(|ui| {
                self.pfp(note_key, &profile, ui);
            });
        }

        let response = ui
            .with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                self.pfp(note_key, &profile, ui);

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
                        note_action = render_note_actionbar(ui, note_key).inner;
                    }
                });
            })
            .response;

        NoteResponse {
            response,
            action: note_action,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BarAction {
    Reply,
}

fn render_note_actionbar(
    ui: &mut egui::Ui,
    note_key: NoteKey,
) -> egui::InnerResponse<Option<BarAction>> {
    ui.horizontal(|ui| {
        let img_data = if ui.style().visuals.dark_mode {
            egui::include_image!("../../../assets/icons/reply.png")
        } else {
            egui::include_image!("../../../assets/icons/reply-dark.png")
        };

        ui.spacing_mut().button_padding = egui::vec2(0.0, 0.0);

        let button_size = 10.0;
        let expand_size = 5.0;
        let anim_speed = 0.05;

        let (rect, size, resp) = ui::anim::hover_expand(
            ui,
            ui.id().with(("reply_anim", note_key)),
            button_size,
            expand_size,
            anim_speed,
        );

        // align rect to note contents
        let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

        ui.put(rect, egui::Image::new(img_data).max_width(size));

        if resp.clicked() {
            Some(BarAction::Reply)
        } else {
            None
        }
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

        secondary_label(ui, note_cache.reltime_str_mut());

        if !before {
            secondary_label(ui, "⋅");
        }
    })
}
