pub mod edit;
pub mod picture;
pub mod preview;

pub use edit::EditProfileView;
use egui::load::TexturePoll;
use egui::{vec2, Color32, Label, Layout, Rect, RichText, Rounding, ScrollArea, Sense, Stroke};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
pub use picture::ProfilePic;
pub use preview::ProfilePreview;
use tracing::error;

use crate::{
    actionbar::NoteAction,
    colors, images,
    profile::get_display_name,
    timeline::{TimelineCache, TimelineKind},
    ui::{
        note::NoteOptions,
        timeline::{tabs_ui, TimelineTabView},
    },
    NostrName,
};

use notedeck::{Accounts, MediaCache, MuteFun, NoteCache, NotedeckTextStyle, UnknownIds};

pub struct ProfileView<'a> {
    pubkey: &'a Pubkey,
    accounts: &'a Accounts,
    col_id: usize,
    timeline_cache: &'a mut TimelineCache,
    note_options: NoteOptions,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut MediaCache,
    unknown_ids: &'a mut UnknownIds,
    is_muted: &'a MuteFun,
}

pub enum ProfileViewAction {
    EditProfile,
    Note(NoteAction),
}

impl<'a> ProfileView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pubkey: &'a Pubkey,
        accounts: &'a Accounts,
        col_id: usize,
        timeline_cache: &'a mut TimelineCache,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut MediaCache,
        unknown_ids: &'a mut UnknownIds,
        is_muted: &'a MuteFun,
        note_options: NoteOptions,
    ) -> Self {
        ProfileView {
            pubkey,
            accounts,
            col_id,
            timeline_cache,
            ndb,
            note_cache,
            img_cache,
            unknown_ids,
            note_options,
            is_muted,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<ProfileViewAction> {
        let scroll_id = egui::Id::new(("profile_scroll", self.col_id, self.pubkey));

        ScrollArea::vertical()
            .id_salt(scroll_id)
            .show(ui, |ui| {
                let mut action = None;
                let txn = Transaction::new(self.ndb).expect("txn");
                if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, self.pubkey.bytes()) {
                    if self.profile_body(ui, profile) {
                        action = Some(ProfileViewAction::EditProfile);
                    }
                }
                let profile_timeline = self
                    .timeline_cache
                    .notes(
                        self.ndb,
                        self.note_cache,
                        &txn,
                        &TimelineKind::Profile(*self.pubkey),
                    )
                    .get_ptr();

                profile_timeline.selected_view =
                    tabs_ui(ui, profile_timeline.selected_view, &profile_timeline.views);

                let reversed = false;
                // poll for new notes and insert them into our existing notes
                if let Err(e) = profile_timeline.poll_notes_into_view(
                    self.ndb,
                    &txn,
                    self.unknown_ids,
                    self.note_cache,
                    reversed,
                ) {
                    error!("Profile::poll_notes_into_view: {e}");
                }

                if let Some(note_action) = TimelineTabView::new(
                    profile_timeline.current_view(),
                    reversed,
                    self.note_options,
                    &txn,
                    self.ndb,
                    self.note_cache,
                    self.img_cache,
                    self.is_muted,
                )
                .show(ui)
                {
                    action = Some(ProfileViewAction::Note(note_action));
                }

                action
            })
            .inner
    }

    fn profile_body(&mut self, ui: &mut egui::Ui, profile: ProfileRecord<'_>) -> bool {
        let mut action = false;
        ui.vertical(|ui| {
            banner(
                ui,
                profile.record().profile().and_then(|p| p.banner()),
                120.0,
            );

            let padding = 12.0;
            crate::ui::padding(padding, ui, |ui| {
                let mut pfp_rect = ui.available_rect_before_wrap();
                let size = 80.0;
                pfp_rect.set_width(size);
                pfp_rect.set_height(size);
                let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

                ui.horizontal(|ui| {
                    ui.put(
                        pfp_rect,
                        ProfilePic::new(self.img_cache, get_profile_url(Some(&profile)))
                            .size(size)
                            .border(ProfilePic::border_stroke(ui)),
                    );

                    if ui.add(copy_key_widget(&pfp_rect)).clicked() {
                        ui.output_mut(|w| {
                            w.copied_text = if let Some(bech) = self.pubkey.to_bech() {
                                bech
                            } else {
                                error!("Could not convert Pubkey to bech");
                                String::new()
                            }
                        });
                    }

                    if self.accounts.contains_full_kp(self.pubkey) {
                        ui.with_layout(Layout::right_to_left(egui::Align::Max), |ui| {
                            if ui.add(edit_profile_button()).clicked() {
                                action = true;
                            }
                        });
                    }
                });

                ui.add_space(18.0);

                ui.add(display_name_widget(get_display_name(Some(&profile)), false));

                ui.add_space(8.0);

                ui.add(about_section_widget(&profile));

                ui.horizontal_wrapped(|ui| {
                    if let Some(website_url) = profile
                        .record()
                        .profile()
                        .and_then(|p| p.website())
                        .filter(|s| !s.is_empty())
                    {
                        handle_link(ui, website_url);
                    }

                    if let Some(lud16) = profile
                        .record()
                        .profile()
                        .and_then(|p| p.lud16())
                        .filter(|s| !s.is_empty())
                    {
                        handle_lud16(ui, lud16);
                    }
                });
            });
        });

        action
    }
}

fn handle_link(ui: &mut egui::Ui, website_url: &str) {
    ui.image(egui::include_image!(
        "../../../../../assets/icons/links_4x.png"
    ));
    if ui
        .label(RichText::new(website_url).color(colors::PINK))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .interact(Sense::click())
        .clicked()
    {
        if let Err(e) = open::that(website_url) {
            error!("Failed to open URL {} because: {}", website_url, e);
        };
    }
}

fn handle_lud16(ui: &mut egui::Ui, lud16: &str) {
    ui.image(egui::include_image!(
        "../../../../../assets/icons/zap_4x.png"
    ));

    let _ = ui.label(RichText::new(lud16).color(colors::PINK));
}

fn copy_key_widget(pfp_rect: &egui::Rect) -> impl egui::Widget + '_ {
    |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        let copy_key_rect = painter.round_rect_to_pixels(egui::Rect::from_center_size(
            pfp_rect.center_bottom(),
            egui::vec2(48.0, 28.0),
        ));
        let resp = ui.interact(
            copy_key_rect,
            ui.id().with("custom_painter"),
            Sense::click(),
        );

        let copy_key_rounding = Rounding::same(100.0);
        let fill_color = if resp.hovered() {
            ui.visuals().widgets.inactive.weak_bg_fill
        } else {
            ui.visuals().noninteractive().bg_stroke.color
        };
        painter.rect_filled(copy_key_rect, copy_key_rounding, fill_color);

        let stroke_color = ui.visuals().widgets.inactive.weak_bg_fill;
        painter.rect_stroke(
            copy_key_rect.shrink(1.0),
            copy_key_rounding,
            Stroke::new(1.0, stroke_color),
        );
        egui::Image::new(egui::include_image!(
            "../../../../../assets/icons/key_4x.png"
        ))
        .paint_at(
            ui,
            painter.round_rect_to_pixels(egui::Rect::from_center_size(
                copy_key_rect.center(),
                egui::vec2(16.0, 16.0),
            )),
        );

        resp
    }
}

fn edit_profile_button() -> impl egui::Widget + 'static {
    |ui: &mut egui::Ui| -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(vec2(124.0, 32.0), Sense::click());
        let painter = ui.painter_at(rect);
        let rect = painter.round_rect_to_pixels(rect);

        painter.rect_filled(
            rect,
            Rounding::same(8.0),
            if resp.hovered() {
                ui.visuals().widgets.active.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            },
        );
        painter.rect_stroke(
            rect.shrink(1.0),
            Rounding::same(8.0),
            if resp.hovered() {
                ui.visuals().widgets.active.bg_stroke
            } else {
                ui.visuals().widgets.inactive.bg_stroke
            },
        );

        let edit_icon_size = vec2(16.0, 16.0);
        let galley = painter.layout(
            "Edit Profile".to_owned(),
            NotedeckTextStyle::Button.get_font_id(ui.ctx()),
            ui.visuals().text_color(),
            rect.width(),
        );

        let space_between_icon_galley = 8.0;
        let half_icon_size = edit_icon_size.x / 2.0;
        let galley_rect = {
            let galley_rect = Rect::from_center_size(rect.center(), galley.rect.size());
            galley_rect.translate(vec2(half_icon_size + space_between_icon_galley / 2.0, 0.0))
        };

        let edit_icon_rect = {
            let mut center = galley_rect.left_center();
            center.x -= half_icon_size + space_between_icon_galley;
            painter.round_rect_to_pixels(Rect::from_center_size(
                painter.round_pos_to_pixel_center(center),
                edit_icon_size,
            ))
        };

        painter.galley(galley_rect.left_top(), galley, Color32::WHITE);

        egui::Image::new(egui::include_image!(
            "../../../../../assets/icons/edit_icon_4x_dark.png"
        ))
        .paint_at(ui, edit_icon_rect);

        resp
    }
}

fn display_name_widget(name: NostrName<'_>, add_placeholder_space: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let disp_resp = name.display_name.map(|disp_name| {
            ui.add(
                Label::new(
                    RichText::new(disp_name).text_style(NotedeckTextStyle::Heading3.text_style()),
                )
                .selectable(false),
            )
        });

        let (username_resp, nip05_resp) = ui
            .horizontal(|ui| {
                let username_resp = name.username.map(|username| {
                    ui.add(
                        Label::new(
                            RichText::new(format!("@{}", username))
                                .size(16.0)
                                .color(colors::MID_GRAY),
                        )
                        .selectable(false),
                    )
                });

                let nip05_resp = name.nip05.map(|nip05| {
                    ui.image(egui::include_image!(
                        "../../../../../assets/icons/verified_4x.png"
                    ));
                    ui.add(Label::new(
                        RichText::new(nip05).size(16.0).color(colors::TEAL),
                    ))
                });

                (username_resp, nip05_resp)
            })
            .inner;

        let resp = match (disp_resp, username_resp, nip05_resp) {
            (Some(disp), Some(username), Some(nip05)) => disp.union(username).union(nip05),
            (Some(disp), Some(username), None) => disp.union(username),
            (Some(disp), None, None) => disp,
            (None, Some(username), Some(nip05)) => username.union(nip05),
            (None, Some(username), None) => username,
            _ => ui.add(Label::new(RichText::new(name.name()))),
        };

        if add_placeholder_space {
            ui.add_space(16.0);
        }

        resp
    }
}

pub fn get_profile_url<'a>(profile: Option<&ProfileRecord<'a>>) -> &'a str {
    unwrap_profile_url(profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())))
}

pub fn unwrap_profile_url(maybe_url: Option<&str>) -> &str {
    if let Some(url) = maybe_url {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}

fn about_section_widget<'a, 'b>(profile: &'b ProfileRecord<'a>) -> impl egui::Widget + 'b
where
    'b: 'a,
{
    move |ui: &mut egui::Ui| {
        if let Some(about) = profile.record().profile().and_then(|p| p.about()) {
            let resp = ui.label(about);
            ui.add_space(8.0);
            resp
        } else {
            // need any Response so we dont need an Option
            ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
        }
    }
}

fn banner_texture(ui: &mut egui::Ui, banner_url: &str) -> Option<egui::load::SizedTexture> {
    // TODO: cache banner
    if !banner_url.is_empty() {
        let texture_load_res =
            egui::Image::new(banner_url).load_for_size(ui.ctx(), ui.available_size());
        if let Ok(texture_poll) = texture_load_res {
            match texture_poll {
                TexturePoll::Pending { .. } => {}
                TexturePoll::Ready { texture, .. } => return Some(texture),
            }
        }
    }

    None
}

fn banner(ui: &mut egui::Ui, banner_url: Option<&str>, height: f32) -> egui::Response {
    ui.add_sized([ui.available_size().x, height], |ui: &mut egui::Ui| {
        banner_url
            .and_then(|url| banner_texture(ui, url))
            .map(|texture| {
                images::aspect_fill(
                    ui,
                    Sense::hover(),
                    texture.id,
                    texture.size.x / texture.size.y,
                )
            })
            .unwrap_or_else(|| ui.label(""))
    })
}
