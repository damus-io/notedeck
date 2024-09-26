pub mod contents;
pub mod options;
pub mod post;
pub mod quote_repost;
pub mod reply;

pub use contents::NoteContents;
pub use options::NoteOptions;
pub use post::{PostAction, PostResponse, PostView};
pub use quote_repost::QuoteRepostView;
pub use reply::PostReplyView;

use crate::{
    actionbar::BarAction,
    app_style::NotedeckTextStyle,
    colors,
    imgcache::ImageCache,
    note_options::NoteOptionSelection,
    notecache::{CachedNote, NoteCache},
    ui::{self, View},
};
use egui::{menu::BarState, Align, Id, InnerResponse, Label, Layout, Response, RichText, Sense};
use enostr::NoteId;
use nostrdb::{Ndb, Note, NoteKey, NoteReply, Transaction};

use super::profile::preview::{get_display_name, one_line_display_name_widget};

pub struct NoteView<'a> {
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    note: &'a nostrdb::Note<'a>,
    flags: NoteOptions,
}

pub struct NoteResponse {
    pub response: egui::Response,
    pub action: Option<BarAction>,
    pub option_selection: Option<NoteOptionSelection>,
}

impl NoteResponse {
    pub fn new(response: egui::Response) -> Self {
        Self {
            response,
            action: None,
            option_selection: None,
        }
    }

    pub fn with_action(self, action: Option<BarAction>) -> Self {
        Self { action, ..self }
    }

    pub fn select_option(self, option_selection: Option<NoteOptionSelection>) -> Self {
        Self {
            option_selection,
            ..self
        }
    }
}

impl<'a> View for NoteView<'a> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
}

fn reply_desc(
    ui: &mut egui::Ui,
    txn: &Transaction,
    note_reply: &NoteReply,
    ndb: &Ndb,
    img_cache: &mut ImageCache,
) {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let size = 10.0;
    let selectable = false;

    ui.add(
        Label::new(
            RichText::new("replying to")
                .size(size)
                .color(colors::GRAY_SECONDARY),
        )
        .selectable(selectable),
    );

    let reply = if let Some(reply) = note_reply.reply() {
        reply
    } else {
        return;
    };

    let reply_note = if let Ok(reply_note) = ndb.get_note_by_id(txn, reply.id) {
        reply_note
    } else {
        ui.add(
            Label::new(
                RichText::new("a note")
                    .size(size)
                    .color(colors::GRAY_SECONDARY),
            )
            .selectable(selectable),
        );
        return;
    };

    if note_reply.is_reply_to_root() {
        // We're replying to the root, let's show this
        ui.add(
            ui::Mention::new(ndb, img_cache, txn, reply_note.pubkey())
                .size(size)
                .selectable(selectable),
        );
        ui.add(
            Label::new(
                RichText::new("'s note")
                    .size(size)
                    .color(colors::GRAY_SECONDARY),
            )
            .selectable(selectable),
        );
    } else if let Some(root) = note_reply.root() {
        // replying to another post in a thread, not the root

        if let Ok(root_note) = ndb.get_note_by_id(txn, root.id) {
            if root_note.pubkey() == reply_note.pubkey() {
                // simply "replying to bob's note" when replying to bob in his thread
                ui.add(
                    ui::Mention::new(ndb, img_cache, txn, reply_note.pubkey())
                        .size(size)
                        .selectable(selectable),
                );
                ui.add(
                    Label::new(
                        RichText::new("'s note")
                            .size(size)
                            .color(colors::GRAY_SECONDARY),
                    )
                    .selectable(selectable),
                );
            } else {
                // replying to bob in alice's thread

                ui.add(
                    ui::Mention::new(ndb, img_cache, txn, reply_note.pubkey())
                        .size(size)
                        .selectable(selectable),
                );
                ui.add(
                    Label::new(RichText::new("in").size(size).color(colors::GRAY_SECONDARY))
                        .selectable(selectable),
                );
                ui.add(
                    ui::Mention::new(ndb, img_cache, txn, root_note.pubkey())
                        .size(size)
                        .selectable(selectable),
                );
                ui.add(
                    Label::new(
                        RichText::new("'s thread")
                            .size(size)
                            .color(colors::GRAY_SECONDARY),
                    )
                    .selectable(selectable),
                );
            }
        } else {
            ui.add(
                ui::Mention::new(ndb, img_cache, txn, reply_note.pubkey())
                    .size(size)
                    .selectable(selectable),
            );
            ui.add(
                Label::new(
                    RichText::new("in someone's thread")
                        .size(size)
                        .color(colors::GRAY_SECONDARY),
                )
                .selectable(selectable),
            );
        }
    }
}

impl<'a> NoteView<'a> {
    pub fn new(
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        note: &'a nostrdb::Note<'a>,
    ) -> Self {
        let flags = NoteOptions::actionbar | NoteOptions::note_previews;
        Self {
            ndb,
            note_cache,
            img_cache,
            note,
            flags,
        }
    }

    pub fn textmode(mut self, enable: bool) -> Self {
        self.options_mut().set_textmode(enable);
        self
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

    pub fn selectable_text(mut self, enable: bool) -> Self {
        self.options_mut().set_selectable_text(enable);
        self
    }

    pub fn wide(mut self, enable: bool) -> Self {
        self.options_mut().set_wide(enable);
        self
    }

    pub fn options_button(mut self, enable: bool) -> Self {
        self.options_mut().set_options_button(enable);
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
            let profile = self.ndb.get_profile_by_pubkey(txn, self.note.pubkey());

            //ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;

            let cached_note = self
                .note_cache
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
                self.ndb,
                self.img_cache,
                self.note_cache,
                txn,
                self.note,
                note_key,
                self.flags,
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
        if !self.options().has_wide() {
            ui.spacing_mut().item_spacing.x = 16.0;
        } else {
            ui.spacing_mut().item_spacing.x = 4.0;
        }

        let pfp_size = self.options().pfp_size();

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

                let (rect, size, _resp) = ui::anim::hover_expand(
                    ui,
                    egui::Id::new((profile_key, note_key)),
                    pfp_size,
                    ui::NoteView::expand_size(),
                    anim_speed,
                );

                ui.put(rect, ui::ProfilePic::new(self.img_cache, pic).size(size))
                    .on_hover_ui_at_pointer(|ui| {
                        ui.set_max_width(300.0);
                        ui.add(ui::ProfilePreview::new(
                            profile.as_ref().unwrap(),
                            self.img_cache,
                        ));
                    });
            }
            None => {
                ui.add(
                    ui::ProfilePic::new(self.img_cache, ui::ProfilePic::no_pfp_url())
                        .size(pfp_size),
                );
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        if self.options().has_textmode() {
            NoteResponse::new(self.textmode_ui(ui))
        } else {
            let txn = self.note.txn().expect("txn");
            if let Some(note_to_repost) = get_reposted_note(self.ndb, txn, self.note) {
                let profile = self.ndb.get_profile_by_pubkey(txn, self.note.pubkey());

                let style = NotedeckTextStyle::Small;
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.add_sized([20.0, 20.0], repost_icon());
                    });
                    ui.add_space(6.0);
                    let resp = ui.add(one_line_display_name_widget(
                        get_display_name(profile.as_ref().ok()),
                        style,
                    ));
                    if let Ok(rec) = &profile {
                        resp.on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(300.0);
                            ui.add(ui::ProfilePreview::new(rec, self.img_cache));
                        });
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Reposted")
                            .color(colors::GRAY_SECONDARY)
                            .text_style(style.text_style()),
                    );
                });
                NoteView::new(self.ndb, self.note_cache, self.img_cache, &note_to_repost).show(ui)
            } else {
                self.show_standard(ui)
            }
        }
    }

    fn note_header(
        ui: &mut egui::Ui,
        note_cache: &mut NoteCache,
        note: &Note,
        profile: &Result<nostrdb::ProfileRecord<'_>, nostrdb::Error>,
        options: NoteOptions,
    ) -> NoteResponse {
        let note_key = note.key().unwrap();

        let inner_response = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add(ui::Username::new(profile.as_ref().ok(), note.pubkey()).abbreviated(20));

            let cached_note = note_cache.cached_note_or_insert_mut(note_key, note);
            render_reltime(ui, cached_note, true);

            if options.has_options_button() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let more_options_resp = more_options_button(ui, note_key, 8.0);
                    options_context_menu(ui, more_options_resp)
                })
                .inner
            } else {
                None
            }
        });

        NoteResponse::new(inner_response.response).select_option(inner_response.inner)
    }

    fn show_standard(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        #[cfg(feature = "profiling")]
        puffin::profile_function!();
        let note_key = self.note.key().expect("todo: support non-db notes");
        let txn = self.note.txn().expect("todo: support non-db notes");
        let mut note_action: Option<BarAction> = None;
        let mut selected_option: Option<NoteOptionSelection> = None;
        let profile = self.ndb.get_profile_by_pubkey(txn, self.note.pubkey());
        let maybe_hitbox = maybe_note_hitbox(ui, note_key);

        // wide design
        let response = if self.options().has_wide() {
            ui.horizontal(|ui| {
                self.pfp(note_key, &profile, ui);

                let size = ui.available_size();
                ui.vertical(|ui| {
                    ui.add_sized([size.x, self.options().pfp_size()], |ui: &mut egui::Ui| {
                        ui.horizontal_centered(|ui| {
                            selected_option = NoteView::note_header(
                                ui,
                                self.note_cache,
                                self.note,
                                &profile,
                                self.options(),
                            )
                            .option_selection;
                        })
                        .response
                    });

                    let note_reply = self
                        .note_cache
                        .cached_note_or_insert_mut(note_key, self.note)
                        .reply
                        .borrow(self.note.tags());

                    if note_reply.reply().is_some() {
                        ui.horizontal(|ui| {
                            reply_desc(ui, txn, &note_reply, self.ndb, self.img_cache);
                        });
                    }
                });
            });

            let resp = ui.add(NoteContents::new(
                self.ndb,
                self.img_cache,
                self.note_cache,
                txn,
                self.note,
                note_key,
                self.options(),
            ));

            if self.options().has_actionbar() {
                note_action = render_note_actionbar(ui, self.note.id(), note_key).inner;
            }

            resp
        } else {
            // main design
            ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                self.pfp(note_key, &profile, ui);

                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    selected_option = NoteView::note_header(
                        ui,
                        self.note_cache,
                        self.note,
                        &profile,
                        self.options(),
                    )
                    .option_selection;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;

                        let note_reply = self
                            .note_cache
                            .cached_note_or_insert_mut(note_key, self.note)
                            .reply
                            .borrow(self.note.tags());

                        if note_reply.reply().is_some() {
                            reply_desc(ui, txn, &note_reply, self.ndb, self.img_cache);
                        }
                    });

                    ui.add(NoteContents::new(
                        self.ndb,
                        self.img_cache,
                        self.note_cache,
                        txn,
                        self.note,
                        note_key,
                        self.options(),
                    ));

                    if self.options().has_actionbar() {
                        note_action = render_note_actionbar(ui, self.note.id(), note_key).inner;
                    }
                });
            })
            .response
        };

        note_action = check_note_hitbox(
            ui,
            self.note.id(),
            note_key,
            &response,
            maybe_hitbox,
            note_action,
        );

        NoteResponse::new(response)
            .with_action(note_action)
            .select_option(selected_option)
    }
}

fn get_reposted_note<'a>(ndb: &Ndb, txn: &'a Transaction, note: &Note) -> Option<Note<'a>> {
    let new_note_id: &[u8; 32] = if note.kind() == 6 {
        let mut res = None;
        for tag in note.tags().iter() {
            if tag.count() == 0 {
                continue;
            }

            if let Some("e") = tag.get(0).and_then(|t| t.variant().str()) {
                if let Some(note_id) = tag.get(1).and_then(|f| f.variant().id()) {
                    res = Some(note_id);
                    break;
                }
            }
        }
        res?
    } else {
        return None;
    };

    let note = ndb.get_note_by_id(txn, new_note_id).ok();
    note.filter(|note| note.kind() == 1)
}

fn note_hitbox_id(note_key: NoteKey) -> egui::Id {
    Id::new(("note_rect", note_key))
}

fn maybe_note_hitbox(ui: &mut egui::Ui, note_key: NoteKey) -> Option<Response> {
    ui.ctx()
        .data_mut(|d| d.get_persisted(note_hitbox_id(note_key)))
        .map(|rect| {
            let id = ui.make_persistent_id(("under_button_interact", note_key));
            ui.interact(rect, id, egui::Sense::click())
        })
}

fn check_note_hitbox(
    ui: &mut egui::Ui,
    note_id: &[u8; 32],
    note_key: NoteKey,
    note_response: &Response,
    maybe_hitbox: Option<Response>,
    prior_action: Option<BarAction>,
) -> Option<BarAction> {
    // Stash the dimensions of the note content so we can render the
    // underbutton in the next frame
    ui.ctx().data_mut(|d| {
        d.insert_persisted(note_hitbox_id(note_key), note_response.rect);
    });

    // If there was an underbutton and it was clicked open the thread
    match maybe_hitbox {
        Some(underbutt) if underbutt.clicked() => {
            Some(BarAction::OpenThread(NoteId::new(*note_id)))
        }
        _ => prior_action,
    }
}

fn render_note_actionbar(
    ui: &mut egui::Ui,
    note_id: &[u8; 32],
    note_key: NoteKey,
) -> egui::InnerResponse<Option<BarAction>> {
    ui.horizontal(|ui| {
        let reply_resp = reply_button(ui, note_key);
        let quote_resp = quote_repost_button(ui, note_key);

        if reply_resp.clicked() {
            Some(BarAction::Reply(NoteId::new(*note_id)))
        } else if quote_resp.clicked() {
            Some(BarAction::Quote(NoteId::new(*note_id)))
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

fn reply_button(ui: &mut egui::Ui, note_key: NoteKey) -> egui::Response {
    let img_data = if ui.style().visuals.dark_mode {
        egui::include_image!("../../../assets/icons/reply.png")
    } else {
        egui::include_image!("../../../assets/icons/reply-dark.png")
    };

    let (rect, size, resp) =
        ui::anim::hover_expand_small(ui, ui.id().with(("reply_anim", note_key)));

    // align rect to note contents
    let expand_size = 5.0; // from hover_expand_small
    let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

    let put_resp = ui.put(rect, egui::Image::new(img_data).max_width(size));

    resp.union(put_resp)
}

fn repost_icon() -> egui::Image<'static> {
    let img_data = egui::include_image!("../../../assets/icons/repost_icon_4x.png");
    egui::Image::new(img_data)
}

fn quote_repost_button(ui: &mut egui::Ui, note_key: NoteKey) -> egui::Response {
    let (rect, size, resp) =
        ui::anim::hover_expand_small(ui, ui.id().with(("repost_anim", note_key)));

    let expand_size = 5.0;
    let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

    let put_resp = ui.put(rect, repost_icon().max_width(size));

    resp.union(put_resp)
}

fn more_options_button(ui: &mut egui::Ui, note_key: NoteKey, max_height: f32) -> egui::Response {
    let id = ui.id().with(("more_options_anim", note_key));

    let expansion_multiple = 2.0;
    let max_radius = max_height;
    let min_radius = max_radius / expansion_multiple;
    let max_distance_between_circles = 2.0;
    let min_distance_between_circles = max_distance_between_circles / expansion_multiple;
    let max_width = max_radius * 3.0 + max_distance_between_circles * 2.0;

    let anim_speed = 0.05;
    let expanded_size = egui::vec2(max_width, max_height);
    let (rect, response) = ui.allocate_exact_size(expanded_size, egui::Sense::click());

    let animation_progress = ui
        .ctx()
        .animate_bool_with_time(id, response.hovered(), anim_speed);
    let cur_distance = min_distance_between_circles
        + (max_distance_between_circles - min_distance_between_circles) * animation_progress;
    let cur_radius = min_radius + (max_radius - min_radius) * animation_progress;

    let center = rect.center();
    let left_circle_center = center - egui::vec2(cur_distance + cur_radius, 0.0);
    let right_circle_center = center + egui::vec2(cur_distance + cur_radius, 0.0);

    let translated_radius = (cur_radius - 1.0) / 2.0;

    let color = if ui.style().visuals.dark_mode {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };

    // Draw circles
    let painter = ui.painter_at(rect);
    painter.circle_filled(left_circle_center, translated_radius, color);
    painter.circle_filled(center, translated_radius, color);
    painter.circle_filled(right_circle_center, translated_radius, color);

    response
}

fn options_context_menu(
    ui: &mut egui::Ui,
    more_options_button_resp: egui::Response,
) -> Option<NoteOptionSelection> {
    let mut selected_option: Option<NoteOptionSelection> = None;

    stationary_arbitrary_menu_button(ui, more_options_button_resp, |ui| {
        ui.set_max_width(200.0);
        if ui.button("Copy text").clicked() {
            selected_option = Some(NoteOptionSelection::CopyText);
            ui.close_menu();
        }
        if ui.button("Copy user public key").clicked() {
            selected_option = Some(NoteOptionSelection::CopyPubkey);
            ui.close_menu();
        }
        if ui.button("Copy note id").clicked() {
            selected_option = Some(NoteOptionSelection::CopyNoteId);
            ui.close_menu();
        }
    });

    selected_option
}

fn stationary_arbitrary_menu_button<R>(
    ui: &mut egui::Ui,
    button_response: egui::Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> InnerResponse<Option<R>> {
    let bar_id = ui.id();
    let mut bar_state = BarState::load(ui.ctx(), bar_id);

    let inner = bar_state.bar_menu(&button_response, add_contents);

    bar_state.store(ui.ctx(), bar_id);
    InnerResponse::new(inner.map(|r| r.inner), button_response)
}
