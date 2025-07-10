pub mod contents;
pub mod context;
pub mod media;
pub mod options;
pub mod reply_description;

use crate::app_images;
use crate::jobs::JobsCache;
use crate::{
    profile::name::one_line_display_name_widget, widgets::x_button, ProfilePic, ProfilePreview,
    PulseAlpha, Username,
};

pub use contents::{render_note_contents, render_note_preview, NoteContents};
pub use context::NoteContextButton;
use notedeck::note::MediaAction;
use notedeck::note::ZapTargetAmount;
use notedeck::ui::is_narrow;
use notedeck::Images;
pub use options::NoteOptions;
pub use reply_description::reply_desc;

use egui::emath::{pos2, Vec2};
use egui::{Id, Label, Pos2, Rect, Response, RichText, Sense};
use enostr::{KeypairUnowned, NoteId, Pubkey};
use nostrdb::{Ndb, Note, NoteKey, ProfileRecord, Transaction};
use notedeck::{
    name::get_display_name,
    note::{NoteAction, NoteContext, ZapAction},
    AnyZapState, CachedNote, ContextSelection, NoteCache, NoteZapTarget, NoteZapTargetOwned,
    NotedeckTextStyle, ZapTarget, Zaps,
};

pub struct NoteView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    zapping_acc: Option<&'a KeypairUnowned<'a>>,
    parent: Option<NoteKey>,
    note: &'a nostrdb::Note<'a>,
    framed: bool,
    flags: NoteOptions,
    jobs: &'a mut JobsCache,
    show_unread_indicator: bool,
}

pub struct NoteResponse {
    pub response: egui::Response,
    pub action: Option<NoteAction>,
    pub pfp_rect: Option<egui::Rect>,
}

impl NoteResponse {
    pub fn new(response: egui::Response) -> Self {
        Self {
            response,
            action: None,
            pfp_rect: None,
        }
    }

    pub fn with_action(mut self, action: Option<NoteAction>) -> Self {
        self.action = action;
        self
    }

    pub fn with_pfp(mut self, pfp_rect: egui::Rect) -> Self {
        self.pfp_rect = Some(pfp_rect);
        self
    }
}

/*
impl View for NoteView<'_, '_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.show(ui);
    }
}
*/

impl egui::Widget for &mut NoteView<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        self.show(ui).response
    }
}

impl<'a, 'd> NoteView<'a, 'd> {
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        zapping_acc: Option<&'a KeypairUnowned<'a>>,
        note: &'a nostrdb::Note<'a>,
        mut flags: NoteOptions,
        jobs: &'a mut JobsCache,
    ) -> Self {
        flags.set_actionbar(true);
        flags.set_note_previews(true);

        let framed = false;
        let parent: Option<NoteKey> = None;

        Self {
            note_context,
            zapping_acc,
            parent,
            note,
            flags,
            framed,
            jobs,
            show_unread_indicator: false,
        }
    }

    pub fn preview_style(self) -> Self {
        self.actionbar(false)
            .small_pfp(true)
            .frame(true)
            .wide(true)
            .note_previews(false)
            .options_button(true)
            .is_preview(true)
    }

    pub fn textmode(mut self, enable: bool) -> Self {
        self.options_mut().set_textmode(enable);
        self
    }

    pub fn actionbar(mut self, enable: bool) -> Self {
        self.options_mut().set_actionbar(enable);
        self
    }

    pub fn hide_media(mut self, enable: bool) -> Self {
        self.options_mut().set_hide_media(enable);
        self
    }

    pub fn frame(mut self, enable: bool) -> Self {
        self.framed = enable;
        self
    }

    pub fn truncate(mut self, enable: bool) -> Self {
        self.options_mut().set_truncate(enable);
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

    pub fn unread_indicator(mut self, show_unread_indicator: bool) -> Self {
        self.show_unread_indicator = show_unread_indicator;
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
                    Username::new(profile.as_ref().ok(), self.note.pubkey())
                        .abbreviated(6)
                        .pk_colored(true),
                )
            });

            ui.add(&mut NoteContents::new(
                self.note_context,
                self.zapping_acc,
                txn,
                self.note,
                self.flags,
                self.jobs,
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
    ) -> PfpResponse {
        if !self.options().has_wide() {
            ui.spacing_mut().item_spacing.x = 16.0;
        } else {
            ui.spacing_mut().item_spacing.x = 4.0;
        }

        if is_narrow(ui.ctx()) {
            ui.spacing_mut().item_spacing.x = 1.0
        }

        let pfp_size = self.options().pfp_size();

        match profile
            .as_ref()
            .ok()
            .and_then(|p| p.record().profile()?.picture())
        {
            // these have different lifetimes and types,
            // so the calls must be separate
            Some(pic) => show_actual_pfp(
                ui,
                self.note_context.img_cache,
                pic,
                pfp_size,
                note_key,
                profile,
            ),

            None => show_fallback_pfp(ui, self.note_context.img_cache, pfp_size),
        }
    }

    fn show_repost(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        note_to_repost: Note<'_>,
    ) -> NoteResponse {
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
                    ui.add(ProfilePreview::new(rec, self.note_context.img_cache));
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
        NoteView::new(
            self.note_context,
            self.zapping_acc,
            &note_to_repost,
            self.flags,
            self.jobs,
        )
        .show(ui)
    }

    pub fn show_impl(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        let txn = self.note.txn().expect("txn");
        if let Some(note_to_repost) = get_reposted_note(self.note_context.ndb, txn, self.note) {
            self.show_repost(ui, txn, note_to_repost)
        } else {
            self.show_standard(ui)
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        if self.options().has_textmode() {
            NoteResponse::new(self.textmode_ui(ui))
        } else if self.framed {
            egui::Frame::new()
                .fill(ui.visuals().noninteractive().weak_bg_fill)
                .inner_margin(egui::Margin::same(8))
                .outer_margin(egui::Margin::symmetric(0, 8))
                .corner_radius(egui::CornerRadius::same(10))
                .stroke(egui::Stroke::new(
                    1.0,
                    ui.visuals().noninteractive().bg_stroke.color,
                ))
                .show(ui, |ui| self.show_impl(ui))
                .inner
        } else {
            self.show_impl(ui)
        }
    }

    #[profiling::function]
    fn note_header(
        ui: &mut egui::Ui,
        note_cache: &mut NoteCache,
        note: &Note,
        profile: &Result<nostrdb::ProfileRecord<'_>, nostrdb::Error>,
        show_unread_indicator: bool,
    ) {
        let note_key = note.key().unwrap();

        let horiz_resp = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = if is_narrow(ui.ctx()) { 1.0 } else { 2.0 };
                ui.add(Username::new(profile.as_ref().ok(), note.pubkey()).abbreviated(20));

                let cached_note = note_cache.cached_note_or_insert_mut(note_key, note);
                render_reltime(ui, cached_note, true);
            })
            .response;

        if !show_unread_indicator {
            return;
        }

        let radius = 4.0;
        let circle_center = {
            let mut center = horiz_resp.rect.right_center();
            center.x += radius + 4.0;
            center
        };

        ui.painter()
            .circle_filled(circle_center, radius, crate::colors::PINK);
    }

    fn wide_ui(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        note_key: NoteKey,
        profile: &Result<ProfileRecord, nostrdb::Error>,
    ) -> egui::InnerResponse<NoteUiResponse> {
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            let mut note_action: Option<NoteAction> = None;
            let pfp_rect = ui
                .horizontal(|ui| {
                    let pfp_resp = self.pfp(note_key, profile, ui);
                    let pfp_rect = pfp_resp.bounding_rect;
                    note_action = pfp_resp
                        .into_action(self.note.pubkey())
                        .or(note_action.take());

                    let size = ui.available_size();
                    ui.vertical(|ui| 's: {
                        ui.add_sized(
                            [size.x, self.options().pfp_size() as f32],
                            |ui: &mut egui::Ui| {
                                ui.horizontal_centered(|ui| {
                                    NoteView::note_header(
                                        ui,
                                        self.note_context.note_cache,
                                        self.note,
                                        profile,
                                        self.show_unread_indicator,
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

                        if note_reply.reply().is_none() {
                            break 's;
                        }

                        ui.horizontal(|ui| {
                            note_action = reply_desc(
                                ui,
                                self.zapping_acc,
                                txn,
                                &note_reply,
                                self.note_context,
                                self.flags,
                                self.jobs,
                            )
                            .or(note_action.take());
                        });
                    });

                    pfp_rect
                })
                .inner;

            let mut contents = NoteContents::new(
                self.note_context,
                self.zapping_acc,
                txn,
                self.note,
                self.flags,
                self.jobs,
            );

            ui.add(&mut contents);

            note_action = contents.action.or(note_action);

            if self.options().has_actionbar() {
                note_action = render_note_actionbar(
                    ui,
                    self.zapping_acc.as_ref().map(|c| Zapper {
                        zaps: self.note_context.zaps,
                        cur_acc: c,
                    }),
                    self.note.id(),
                    self.note.pubkey(),
                    note_key,
                )
                .inner
                .or(note_action);
            }

            NoteUiResponse {
                action: note_action,
                pfp_rect,
            }
        })
    }

    fn standard_ui(
        &mut self,
        ui: &mut egui::Ui,
        txn: &Transaction,
        note_key: NoteKey,
        profile: &Result<ProfileRecord, nostrdb::Error>,
    ) -> egui::InnerResponse<NoteUiResponse> {
        // main design
        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
            let pfp_resp = self.pfp(note_key, profile, ui);
            let pfp_rect = pfp_resp.bounding_rect;
            let mut note_action: Option<NoteAction> = pfp_resp.into_action(self.note.pubkey());

            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                NoteView::note_header(
                    ui,
                    self.note_context.note_cache,
                    self.note,
                    profile,
                    self.show_unread_indicator,
                );
                ui.horizontal(|ui| 's: {
                    ui.spacing_mut().item_spacing.x = if is_narrow(ui.ctx()) { 1.0 } else { 2.0 };

                    let note_reply = self
                        .note_context
                        .note_cache
                        .cached_note_or_insert_mut(note_key, self.note)
                        .reply
                        .borrow(self.note.tags());

                    if note_reply.reply().is_none() {
                        break 's;
                    }

                    note_action = reply_desc(
                        ui,
                        self.zapping_acc,
                        txn,
                        &note_reply,
                        self.note_context,
                        self.flags,
                        self.jobs,
                    )
                    .or(note_action.take());
                });

                let mut contents = NoteContents::new(
                    self.note_context,
                    self.zapping_acc,
                    txn,
                    self.note,
                    self.flags,
                    self.jobs,
                );
                ui.add(&mut contents);

                note_action = contents.action.or(note_action);

                if self.options().has_actionbar() {
                    note_action = render_note_actionbar(
                        ui,
                        self.zapping_acc.as_ref().map(|c| Zapper {
                            zaps: self.note_context.zaps,
                            cur_acc: c,
                        }),
                        self.note.id(),
                        self.note.pubkey(),
                        note_key,
                    )
                    .inner
                    .or(note_action);
                }

                NoteUiResponse {
                    action: note_action,
                    pfp_rect,
                }
            })
            .inner
        })
    }

    #[profiling::function]
    fn show_standard(&mut self, ui: &mut egui::Ui) -> NoteResponse {
        let note_key = self.note.key().expect("todo: support non-db notes");
        let txn = self.note.txn().expect("todo: support non-db notes");

        let profile = self
            .note_context
            .ndb
            .get_profile_by_pubkey(txn, self.note.pubkey());

        let hitbox_id = note_hitbox_id(note_key, self.options(), self.parent);
        let maybe_hitbox = maybe_note_hitbox(ui, hitbox_id);

        // wide design
        let response = if self.options().has_wide() {
            self.wide_ui(ui, txn, note_key, &profile)
        } else {
            self.standard_ui(ui, txn, note_key, &profile)
        };

        let note_ui_resp = response.inner;
        let mut note_action = note_ui_resp.action;

        if self.options().has_options_button() {
            let context_pos = {
                let size = NoteContextButton::max_width();
                let top_right = response.response.rect.right_top();
                let min = Pos2::new(top_right.x - size, top_right.y);
                Rect::from_min_size(min, egui::vec2(size, size))
            };

            let resp = ui.add(NoteContextButton::new(note_key).place_at(context_pos));
            if let Some(action) = NoteContextButton::menu(ui, resp.clone()) {
                note_action = Some(NoteAction::Context(ContextSelection { note_key, action }));
            }
        }

        note_action = note_hitbox_clicked(ui, hitbox_id, &response.response.rect, maybe_hitbox)
            .then_some(NoteAction::note(NoteId::new(*self.note.id())))
            .or(note_action);

        NoteResponse::new(response.response)
            .with_action(note_action)
            .with_pfp(note_ui_resp.pfp_rect)
    }
}

fn get_reposted_note<'a>(ndb: &Ndb, txn: &'a Transaction, note: &Note) -> Option<Note<'a>> {
    if note.kind() != 6 {
        return None;
    }

    let new_note_id: &[u8; 32] = {
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
    };

    let note = ndb.get_note_by_id(txn, new_note_id).ok();
    note.filter(|note| note.kind() == 1)
}

struct NoteUiResponse {
    action: Option<NoteAction>,
    pfp_rect: egui::Rect,
}

struct PfpResponse {
    action: Option<MediaAction>,
    response: egui::Response,
    bounding_rect: egui::Rect,
}

impl PfpResponse {
    fn into_action(self, note_pk: &[u8; 32]) -> Option<NoteAction> {
        if self.response.clicked() {
            return Some(NoteAction::Profile(Pubkey::new(*note_pk)));
        }

        self.action.map(NoteAction::Media)
    }
}

fn show_actual_pfp(
    ui: &mut egui::Ui,
    images: &mut Images,
    pic: &str,
    pfp_size: i8,
    note_key: NoteKey,
    profile: &Result<nostrdb::ProfileRecord<'_>, nostrdb::Error>,
) -> PfpResponse {
    let anim_speed = 0.05;
    let profile_key = profile.as_ref().unwrap().record().note_key();
    let note_key = note_key.as_u64();

    let (rect, size, resp) = crate::anim::hover_expand(
        ui,
        egui::Id::new((profile_key, note_key)),
        pfp_size as f32,
        NoteView::expand_size() as f32,
        anim_speed,
    );

    let mut pfp = ProfilePic::new(images, pic).size(size);
    let pfp_resp = ui.put(rect, &mut pfp);
    let action = pfp.action;

    if resp.hovered() || resp.clicked() {
        crate::show_pointer(ui);
    }

    pfp_resp.on_hover_ui_at_pointer(|ui| {
        ui.set_max_width(300.0);
        ui.add(ProfilePreview::new(profile.as_ref().unwrap(), images));
    });

    PfpResponse {
        response: resp,
        action,
        bounding_rect: rect.shrink((rect.width() - size) / 2.0),
    }
}

fn show_fallback_pfp(ui: &mut egui::Ui, images: &mut Images, pfp_size: i8) -> PfpResponse {
    let sense = Sense::click();
    // This has to match the expand size from the above case to
    // prevent bounciness
    let size = (pfp_size + NoteView::expand_size()) as f32;
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(size, size), sense);

    let mut pfp = ProfilePic::new(images, notedeck::profile::no_pfp_url()).size(pfp_size as f32);
    let response = ui.put(rect, &mut pfp).interact(sense);

    PfpResponse {
        action: pfp.action,
        response,
        bounding_rect: rect.shrink((rect.width() - size) / 2.0),
    }
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
        d.insert_temp(hitbox_id, note_rect.size());
    });

    // If there was an hitbox and it was clicked open the thread
    match maybe_hitbox {
        Some(hitbox) => hitbox.clicked(),
        _ => false,
    }
}

struct Zapper<'a> {
    zaps: &'a Zaps,
    cur_acc: &'a KeypairUnowned<'a>,
}

#[profiling::function]
fn render_note_actionbar(
    ui: &mut egui::Ui,
    zapper: Option<Zapper>,
    note_id: &[u8; 32],
    note_pubkey: &[u8; 32],
    note_key: NoteKey,
) -> egui::InnerResponse<Option<NoteAction>> {
    ui.horizontal(|ui| 's: {
        let reply_resp = reply_button(ui, note_key);
        let quote_resp = quote_repost_button(ui, note_key);

        let to_noteid = |id: &[u8; 32]| NoteId::new(*id);
        if reply_resp.clicked() {
            break 's Some(NoteAction::Reply(to_noteid(note_id)));
        } else if reply_resp.hovered() {
            crate::show_pointer(ui);
        }

        if quote_resp.clicked() {
            break 's Some(NoteAction::Quote(to_noteid(note_id)));
        } else if quote_resp.hovered() {
            crate::show_pointer(ui);
        }

        let Some(Zapper { zaps, cur_acc }) = zapper else {
            break 's None;
        };

        let zap_target = ZapTarget::Note(NoteZapTarget {
            note_id,
            zap_recipient: note_pubkey,
        });

        let zap_state = zaps.any_zap_state_for(cur_acc.pubkey.bytes(), zap_target);

        let target = NoteZapTargetOwned {
            note_id: to_noteid(note_id),
            zap_recipient: Pubkey::new(*note_pubkey),
        };

        if zap_state.is_err() {
            break 's Some(NoteAction::Zap(ZapAction::ClearError(target)));
        }

        let zap_resp = {
            cur_acc.secret_key.as_ref()?;

            match zap_state {
                Ok(any_zap_state) => ui.add(zap_button(any_zap_state, note_id)),
                Err(err) => {
                    let (rect, _) =
                        ui.allocate_at_least(egui::vec2(10.0, 10.0), egui::Sense::click());
                    ui.add(x_button(rect)).on_hover_text(err.to_string())
                }
            }
        };

        if zap_resp.hovered() {
            crate::show_pointer(ui);
        }

        if zap_resp.secondary_clicked() {
            break 's Some(NoteAction::Zap(ZapAction::CustomizeAmount(target)));
        }

        if !zap_resp.clicked() {
            break 's None;
        }

        Some(NoteAction::Zap(ZapAction::Send(ZapTargetAmount {
            target,
            specified_msats: None,
        })))
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
    let img = if ui.style().visuals.dark_mode {
        app_images::reply_dark_image()
    } else {
        app_images::reply_light_image()
    };

    let (rect, size, resp) =
        crate::anim::hover_expand_small(ui, ui.id().with(("reply_anim", note_key)));

    // align rect to note contents
    let expand_size = 5.0; // from hover_expand_small
    let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

    let put_resp = ui
        .put(rect, img.max_width(size))
        .on_hover_text("Reply to this note");

    resp.union(put_resp)
}

fn repost_icon(dark_mode: bool) -> egui::Image<'static> {
    if dark_mode {
        app_images::repost_dark_image()
    } else {
        app_images::repost_light_image()
    }
}

fn quote_repost_button(ui: &mut egui::Ui, note_key: NoteKey) -> egui::Response {
    let size = 14.0;
    let expand_size = 5.0;
    let anim_speed = 0.05;
    let id = ui.id().with(("repost_anim", note_key));

    let (rect, size, resp) = crate::anim::hover_expand(ui, id, size, expand_size, anim_speed);

    let rect = rect.translate(egui::vec2(-(expand_size / 2.0), -1.0));

    let put_resp = ui
        .put(rect, repost_icon(ui.visuals().dark_mode).max_width(size))
        .on_hover_text("Repost this note");

    resp.union(put_resp)
}

fn zap_button(state: AnyZapState, noteid: &[u8; 32]) -> impl egui::Widget + use<'_> {
    move |ui: &mut egui::Ui| -> egui::Response {
        let (rect, size, resp) = crate::anim::hover_expand_small(ui, ui.id().with("zap"));

        let mut img = app_images::zap_image().max_width(size);
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
                let cur_alpha = PulseAlpha::new(&ctx, id, alpha_min, 255)
                    .with_speed(0.35)
                    .animate();

                let cur_color = egui::Color32::from_rgba_unmultiplied(0xFF, 0xB7, 0x57, cur_alpha);
                img = img.tint(cur_color);
            }
            AnyZapState::LocalOnly => {
                img = img.tint(egui::Color32::from_rgb(0xFF, 0xB7, 0x57));
            }
            AnyZapState::Confirmed => {}
        }

        // align rect to note contents
        let expand_size = 5.0; // from hover_expand_small
        let rect = rect.translate(egui::vec2(-(expand_size / 2.0), 0.0));

        let put_resp = ui.put(rect, img).on_hover_text("Zap this note");

        resp.union(put_resp)
    }
}
