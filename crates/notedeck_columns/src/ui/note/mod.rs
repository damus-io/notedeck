pub mod contents;
pub mod context;
pub mod options;
pub mod post;
pub mod quote_repost;
pub mod reply;
pub mod reply_description;

pub use contents::NoteContents;
use contents::NoteContext;
pub use context::{NoteContextButton, NoteContextSelection};
use notedeck_ui::ImagePulseTint;
pub use options::NoteOptions;
pub use post::{NewPostAction, PostAction, PostResponse, PostType, PostView};
pub use quote_repost::QuoteRepostView;
pub use reply::PostReplyView;
pub use reply_description::reply_desc;

use crate::{
    actionbar::{ContextSelection, NoteAction, ZapAction},
    profile::get_display_name,
    timeline::{ThreadSelection, TimelineKind},
    ui::{self, View},
};

use egui::emath::{pos2, Vec2};
use egui::{Id, Label, Pos2, Rect, Response, RichText, Sense};
use enostr::{KeypairUnowned, NoteId, Pubkey};
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use notedeck::{
    AnyZapState, CachedNote, NoteCache, NoteZapTarget, NoteZapTargetOwned, NotedeckTextStyle,
    ZapTarget, Zaps,
};

use super::{profile::preview::one_line_display_name_widget, widgets::x_button};

pub struct NoteView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
    parent: Option<NoteKey>,
    note: &'a nostrdb::Note<'a>,
    flags: NoteOptions,
}

pub struct NoteResponse {
    pub response: egui::Response,
    pub action: Option<NoteAction>,
}

impl NoteResponse {
    pub fn new(response: egui::Response) -> Self {
        Self {
            response,
            action: None,
        }
    }

    pub fn with_action(mut self, action: Option<NoteAction>) -> Self {
        self.action = action;
        self
    }
}

impl View for NoteView<'_, '_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
}

impl<'a, 'd> NoteView<'a, 'd> {
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
        note: &'a nostrdb::Note<'a>,
        mut flags: NoteOptions,
    ) -> Self {
        flags.set_actionbar(true);
        flags.set_note_previews(true);

        let parent: Option<NoteKey> = None;
        Self {
            note_context,
            cur_acc,
            parent,
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

    pub fn parent(mut self, parent: NoteKey) -> Self {
        self.parent = Some(parent);
        self
    }

    pub fn is_preview(mut self, is_preview: bool) -> Self {
        self.options_mut().set_is_preview(is_preview);
        self
    }

    fn textmode_ui(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let note_key = self.note.key().expect("todo: implement non-db notes");
        let txn = self.note.txn().expect("todo: implement non-db notes");

        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
            let profile = self
                .note_context
                .ndb
                .get_profile_by_pubkey(txn, self.note.pubkey());

            //ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;

            let cached_note = self
                .note_context
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

            ui.add(&mut NoteContents::new(
                self.note_context,
                self.cur_acc,
                txn,
                self.note,
                self.flags,
            ));
            //});
        })
        .response
    }

    pub fn expand_size() -> i8 {
        5
    }

    fn pfp(
        &mut self,
        note_key: NoteKey,
        profile: &Result<nostrdb::ProfileRecord<'_>, nostrdb::Error>,
        ui: &mut egui::Ui,
    ) -> egui::Response {
        if !self.options().has_wide() {
            ui.spacing_mut().item_spacing.x = 16.0;
        } else {
            ui.spacing_mut().item_spacing.x = 4.0;
        }

        let pfp_size = self.options().pfp_size();

        let sense = Sense::click();
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

                let (rect, size, resp) = ui::anim::hover_expand(
                    ui,
                    egui::Id::new((profile_key, note_key)),
                    pfp_size as f32,
                    ui::NoteView::expand_size() as f32,
                    anim_speed,
                );

                ui.put(
                    rect,
                    ui::ProfilePic::new(self.note_context.img_cache, pic).size(size),
                )
                .on_hover_ui_at_pointer(|ui| {
                    ui.set_max_width(300.0);
                    ui.add(ui::ProfilePreview::new(
                        profile.as_ref().unwrap(),
                        self.note_context.img_cache,
                    ));
                });

                if resp.hovered() || resp.clicked() {
                    ui::show_pointer(ui);
                }

                resp
            }

            None => {
                // This has to match the expand size from the above case to
                // prevent bounciness
                let size = (pfp_size + ui::NoteView::expand_size()) as f32;
                let (rect, _response) = ui.allocate_exact_size(egui::vec2(size, size), sense);

                ui.put(
                    rect,
                    ui::ProfilePic::new(self.note_context.img_cache, ui::ProfilePic::no_pfp_url())
                        .size(pfp_size as f32),
                )
                .interact(sense)
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        if self.options().has_textmode() {
            NoteResponse::new(self.textmode_ui(ui))
        } else {
            let txn = self.note.txn().expect("txn");
            if let Some(note_to_repost) = get_reposted_note(self.note_context.ndb, txn, self.note) {
                let profile = self
                    .note_context
                    .ndb
                    .get_profile_by_pubkey(txn, self.note.pubkey());

                let style = NotedeckTextStyle::Small;
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.add_sized([20.0, 20.0], repost_icon(ui.visuals().dark_mode));
                    });
                    ui.add_space(6.0);
                    let resp = ui.add(one_line_display_name_widget(
                        ui.visuals(),
                        get_display_name(profile.as_ref().ok()),
                        style,
                    ));
                    if let Ok(rec) = &profile {
                        resp.on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(300.0);
                            ui.add(ui::ProfilePreview::new(rec, self.note_context.img_cache));
                        });
                    }
                    let color = ui.style().visuals.noninteractive().fg_stroke.color;
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Reposted")
                            .color(color)
                            .text_style(style.text_style()),
                    );
                });
                NoteView::new(self.note_context, self.cur_acc, &note_to_repost, self.flags).show(ui)
            } else {
                self.show_standard(ui)
            }
        }
    }

    #[profiling::function]
    fn note_header(
        ui: &mut egui::Ui,
        note_cache: &mut NoteCache,
        note: &Note,
        profile: &Result<nostrdb::ProfileRecord<'_>, nostrdb::Error>,
    ) {
        let note_key = note.key().unwrap();

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add(ui::Username::new(profile.as_ref().ok(), note.pubkey()).abbreviated(20));

            let cached_note = note_cache.cached_note_or_insert_mut(note_key, note);
            render_reltime(ui, cached_note, true);
        });
    }

    #[profiling::function]
    fn show_standard(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        let note_key = self.note.key().expect("todo: support non-db notes");
        let txn = self.note.txn().expect("todo: support non-db notes");

        let mut note_action: Option<NoteAction> = None;

        let hitbox_id = note_hitbox_id(note_key, self.options(), self.parent);
        let profile = self
            .note_context
            .ndb
            .get_profile_by_pubkey(txn, self.note.pubkey());
        let maybe_hitbox = maybe_note_hitbox(ui, hitbox_id);

        // wide design
        let response = if self.options().has_wide() {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if self.pfp(note_key, &profile, ui).clicked() {
                        note_action = Some(NoteAction::OpenTimeline(TimelineKind::profile(
                            Pubkey::new(*self.note.pubkey()),
                        )));
                    };

                    let size = ui.available_size();
                    ui.vertical(|ui| {
                        ui.add_sized(
                            [size.x, self.options().pfp_size() as f32],
                            |ui: &mut egui::Ui| {
                                ui.horizontal_centered(|ui| {
                                    NoteView::note_header(
                                        ui,
                                        self.note_context.note_cache,
                                        self.note,
                                        &profile,
                                    );
                                })
                                .response
                            },
                        );

                        let note_reply = self
                            .note_context
                            .note_cache
                            .cached_note_or_insert_mut(note_key, self.note)
                            .reply
                            .borrow(self.note.tags());

                        if note_reply.reply().is_some() {
                            let action = ui
                                .horizontal(|ui| {
                                    reply_desc(
                                        ui,
                                        self.cur_acc,
                                        txn,
                                        &note_reply,
                                        self.note_context,
                                        self.flags,
                                    )
                                })
                                .inner;

                            if action.is_some() {
                                note_action = action;
                            }
                        }
                    });
                });

                let mut contents =
                    NoteContents::new(self.note_context, self.cur_acc, txn, self.note, self.flags);

                ui.add(&mut contents);

                if let Some(action) = contents.action() {
                    note_action = Some(action.clone());
                }

                if self.options().has_actionbar() {
                    if let Some(action) = render_note_actionbar(
                        ui,
                        self.note_context.zaps,
                        self.cur_acc.as_ref(),
                        self.note.id(),
                        self.note.pubkey(),
                        note_key,
                    )
                    .inner
                    {
                        note_action = Some(action);
                    }
                }
            })
            .response
        } else {
            // main design
            ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                if self.pfp(note_key, &profile, ui).clicked() {
                    note_action = Some(NoteAction::OpenTimeline(TimelineKind::Profile(
                        Pubkey::new(*self.note.pubkey()),
                    )));
                };

                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    NoteView::note_header(ui, self.note_context.note_cache, self.note, &profile);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;

                        let note_reply = self
                            .note_context
                            .note_cache
                            .cached_note_or_insert_mut(note_key, self.note)
                            .reply
                            .borrow(self.note.tags());

                        if note_reply.reply().is_some() {
                            let action = reply_desc(
                                ui,
                                self.cur_acc,
                                txn,
                                &note_reply,
                                self.note_context,
                                self.flags,
                            );

                            if action.is_some() {
                                note_action = action;
                            }
                        }
                    });

                    let mut contents = NoteContents::new(
                        self.note_context,
                        self.cur_acc,
                        txn,
                        self.note,
                        self.flags,
                    );
                    ui.add(&mut contents);

                    if let Some(action) = contents.action() {
                        note_action = Some(action.clone());
                    }

                    if self.options().has_actionbar() {
                        if let Some(action) = render_note_actionbar(
                            ui,
                            self.note_context.zaps,
                            self.cur_acc.as_ref(),
                            self.note.id(),
                            self.note.pubkey(),
                            note_key,
                        )
                        .inner
                        {
                            note_action = Some(action);
                        }
                    }
                });
            })
            .response
        };

        if self.options().has_options_button() {
            let context_pos = {
                let size = NoteContextButton::max_width();
                let top_right = response.rect.right_top();
                let min = Pos2::new(top_right.x - size, top_right.y);
                Rect::from_min_size(min, egui::vec2(size, size))
            };

            let resp = ui.add(NoteContextButton::new(note_key).place_at(context_pos));
            if let Some(action) = NoteContextButton::menu(ui, resp.clone()) {
                note_action = Some(NoteAction::Context(ContextSelection { note_key, action }));
            }
        }

        let note_action = if note_hitbox_clicked(ui, hitbox_id, &response.rect, maybe_hitbox) {
            if let Ok(selection) = ThreadSelection::from_note_id(
                self.note_context.ndb,
                self.note_context.note_cache,
                self.note.txn().unwrap(),
                NoteId::new(*self.note.id()),
            ) {
                Some(NoteAction::OpenTimeline(TimelineKind::Thread(selection)))
            } else {
                None
            }
        } else {
            note_action
        };

        NoteResponse::new(response).with_action(note_action)
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

fn note_hitbox_id(
    note_key: NoteKey,
    note_options: NoteOptions,
    parent: Option<NoteKey>,
) -> egui::Id {
    Id::new(("note_size", note_key, note_options, parent))
}

fn maybe_note_hitbox(ui: &mut egui::Ui, hitbox_id: egui::Id) -> Option<Response> {
    ui.ctx()
        .data_mut(|d| d.get_persisted(hitbox_id))
        .map(|note_size: Vec2| {
            // The hitbox should extend the entire width of the
            // container.  The hitbox height was cached last layout.
            let container_rect = ui.max_rect();
            let rect = Rect {
                min: pos2(container_rect.min.x, container_rect.min.y),
                max: pos2(container_rect.max.x, container_rect.min.y + note_size.y),
            };

            let response = ui.interact(rect, ui.id().with(hitbox_id), egui::Sense::click());

            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "hitbox"));

            response
        })
}

fn note_hitbox_clicked(
    ui: &mut egui::Ui,
    hitbox_id: egui::Id,
    note_rect: &Rect,
    maybe_hitbox: Option<Response>,
) -> bool {
    // Stash the dimensions of the note content so we can render the
    // hitbox in the next frame
    ui.ctx().data_mut(|d| {
        d.insert_persisted(hitbox_id, note_rect.size());
    });

    // If there was an hitbox and it was clicked open the thread
    match maybe_hitbox {
        Some(hitbox) => hitbox.clicked(),
        _ => false,
    }
}

#[profiling::function]
fn render_note_actionbar(
    ui: &mut egui::Ui,
    zaps: &Zaps,
    cur_acc: Option<&KeypairUnowned>,
    note_id: &[u8; 32],
    note_pubkey: &[u8; 32],
    note_key: NoteKey,
) -> egui::InnerResponse<Option<NoteAction>> {
    ui.horizontal(|ui| 's: {
        let reply_resp = reply_button(ui, note_key);
        let quote_resp = quote_repost_button(ui, note_key);

        let zap_target = ZapTarget::Note(NoteZapTarget {
            note_id,
            zap_recipient: note_pubkey,
        });

        let zap_state = cur_acc.map_or_else(
            || Ok(AnyZapState::None),
            |kp| zaps.any_zap_state_for(kp.pubkey.bytes(), zap_target),
        );
        let zap_resp = cur_acc
            .filter(|k| k.secret_key.is_some())
            .map(|_| match &zap_state {
                Ok(any_zap_state) => ui.add(zap_button(any_zap_state.clone(), note_id)),
                Err(zapping_error) => {
                    let (rect, _) =
                        ui.allocate_at_least(egui::vec2(10.0, 10.0), egui::Sense::click());
                    ui.add(x_button(rect))
                        .on_hover_text(format!("{zapping_error}"))
                }
            });

        let to_noteid = |id: &[u8; 32]| NoteId::new(*id);

        if reply_resp.clicked() {
            break 's Some(NoteAction::Reply(to_noteid(note_id)));
        }

        if quote_resp.clicked() {
            break 's Some(NoteAction::Quote(to_noteid(note_id)));
        }

        let Some(zap_resp) = zap_resp else {
            break 's None;
        };

        if !zap_resp.clicked() {
            break 's None;
        }

        let target = NoteZapTargetOwned {
            note_id: to_noteid(note_id),
            zap_recipient: Pubkey::new(*note_pubkey),
        };

        if zap_state.is_err() {
            break 's Some(NoteAction::Zap(ZapAction::ClearError(target)));
        }

        Some(NoteAction::Zap(ZapAction::Send(target)))
    })
}

fn secondary_label(ui: &mut egui::Ui, s: impl Into<String>) {
    let color = ui.style().visuals.noninteractive().fg_stroke.color;
    ui.add(Label::new(RichText::new(s).size(10.0).color(color)));
}

#[profiling::function]
fn render_reltime(
    ui: &mut egui::Ui,
    note_cache: &mut CachedNote,
    before: bool,
) -> egui::InnerResponse<()> {
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
        egui::include_image!("../../../../../assets/icons/reply.png")
    } else {
        egui::include_image!("../../../../../assets/icons/reply-dark.png")
    };

    let (rect, size, resp) =
        ui::anim::hover_expand_small(ui, ui.id().with(("reply_anim", note_key)));

    // align rect to note contents
    let expand_size = 5.0; // from hover_expand_small
    let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

    let put_resp = ui.put(rect, egui::Image::new(img_data).max_width(size));

    resp.union(put_resp)
}

fn repost_icon(dark_mode: bool) -> egui::Image<'static> {
    let img_data = if dark_mode {
        egui::include_image!("../../../../../assets/icons/repost_icon_4x.png")
    } else {
        egui::include_image!("../../../../../assets/icons/repost_light_4x.png")
    };
    egui::Image::new(img_data)
}

fn quote_repost_button(ui: &mut egui::Ui, note_key: NoteKey) -> egui::Response {
    let size = 14.0;
    let expand_size = 5.0;
    let anim_speed = 0.05;
    let id = ui.id().with(("repost_anim", note_key));

    let (rect, size, resp) = ui::anim::hover_expand(ui, id, size, expand_size, anim_speed);

    let rect = rect.translate(egui::vec2(-(expand_size / 2.0), -1.0));

    let put_resp = ui.put(rect, repost_icon(ui.visuals().dark_mode).max_width(size));

    resp.union(put_resp)
}

fn zap_button(state: AnyZapState, noteid: &[u8; 32]) -> impl egui::Widget + use<'_> {
    move |ui: &mut egui::Ui| -> egui::Response {
        let img_data = egui::include_image!("../../../../../assets/icons/zap_4x.png");

        let (rect, size, resp) = ui::anim::hover_expand_small(ui, ui.id().with("zap"));

        let mut img = egui::Image::new(img_data).max_width(size);
        let id = ui.id().with(("pulse", noteid));
        let ctx = ui.ctx().clone();

        match state {
            AnyZapState::None => {
                if !ui.visuals().dark_mode {
                    img = img.tint(egui::Color32::BLACK);
                }
            }
            AnyZapState::Pending => {
                let alpha_min = if ui.visuals().dark_mode { 50 } else { 180 };
                img = ImagePulseTint::new(&ctx, id, img, &[0xFF, 0xB7, 0x57], alpha_min, 255)
                    .with_speed(0.35)
                    .animate();
            }
            AnyZapState::LocalOnly => {
                img = img.tint(egui::Color32::from_rgb(0xFF, 0xB7, 0x57));
            }
            AnyZapState::Confirmed => {}
        }

        // align rect to note contents
        let expand_size = 5.0; // from hover_expand_small
        let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

        let put_resp = ui.put(rect, img);

        resp.union(put_resp)
    }
}
