pub mod edit;

pub use edit::EditProfileView;
use egui::{Color32, CornerRadius, Layout, Rect, RichText, ScrollArea, Sense, Stroke, vec2};
use enostr::Pubkey;
use nostrdb::{ProfileRecord, Transaction};
use notedeck_ui::profile::follow_button;
use tracing::error;

use crate::{
    timeline::{TimelineCache, TimelineKind},
    ui::timeline::{TimelineTabView, tabs_ui},
};
use notedeck::{
    IsFollowing, NoteAction, NoteContext, NotedeckTextStyle, name::get_display_name,
    profile::get_profile_url,
};
use notedeck_ui::{
    NoteOptions, ProfilePic, app_images,
    jobs::JobsCache,
    profile::{about_section_widget, banner, display_name_widget},
};

pub struct ProfileView<'a, 'd> {
    pubkey: &'a Pubkey,
    col_id: usize,
    timeline_cache: &'a mut TimelineCache,
    note_options: NoteOptions,
    note_context: &'a mut NoteContext<'d>,
    jobs: &'a mut JobsCache,
}

pub enum ProfileViewAction {
    EditProfile,
    Note(NoteAction),
    Unfollow(Pubkey),
    Follow(Pubkey),
}

impl<'a, 'd> ProfileView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pubkey: &'a Pubkey,
        col_id: usize,
        timeline_cache: &'a mut TimelineCache,
        note_options: NoteOptions,
        note_context: &'a mut NoteContext<'d>,
        jobs: &'a mut JobsCache,
    ) -> Self {
        ProfileView {
            pubkey,
            col_id,
            timeline_cache,
            note_options,
            note_context,
            jobs,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<ProfileViewAction> {
        let scroll_id = egui::Id::new(("profile_scroll", self.col_id, self.pubkey));
        let offset_id = scroll_id.with("scroll_offset");

        let mut scroll_area = ScrollArea::vertical().id_salt(scroll_id);

        if let Some(offset) = ui.data(|i| i.get_temp::<f32>(offset_id)) {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }

        let output = scroll_area.show(ui, |ui| {
            let mut action = None;
            let txn = Transaction::new(self.note_context.ndb).expect("txn");
            let profile = self
                .note_context
                .ndb
                .get_profile_by_pubkey(&txn, self.pubkey.bytes())
                .ok();

            if let Some(profile_view_action) = self.profile_body(ui, profile.as_ref()) {
                action = Some(profile_view_action);
            }
            let profile_timeline = self
                .timeline_cache
                .notes(
                    self.note_context.ndb,
                    self.note_context.note_cache,
                    &txn,
                    &TimelineKind::Profile(*self.pubkey),
                )
                .get_ptr();

            profile_timeline.selected_view =
                tabs_ui(ui, profile_timeline.selected_view, &profile_timeline.views);

            let reversed = false;
            // poll for new notes and insert them into our existing notes
            if let Err(e) = profile_timeline.poll_notes_into_view(
                self.note_context.ndb,
                &txn,
                self.note_context.unknown_ids,
                self.note_context.note_cache,
                reversed,
            ) {
                error!("Profile::poll_notes_into_view: {e}");
            }

            if let Some(note_action) = TimelineTabView::new(
                profile_timeline.current_view(),
                reversed,
                self.note_options,
                &txn,
                self.note_context,
                self.jobs,
            )
            .show(ui)
            {
                action = Some(ProfileViewAction::Note(note_action));
            }

            action
        });

        ui.data_mut(|d| d.insert_temp(offset_id, output.state.offset.y));

        output.inner
    }

    fn profile_body(
        &mut self,
        ui: &mut egui::Ui,
        profile: Option<&ProfileRecord<'_>>,
    ) -> Option<ProfileViewAction> {
        let mut action = None;
        ui.vertical(|ui| {
            banner(
                ui,
                profile
                    .map(|p| p.record().profile())
                    .and_then(|p| p.and_then(|p| p.banner())),
                120.0,
            );

            let padding = 12.0;
            notedeck_ui::padding(padding, ui, |ui| {
                let mut pfp_rect = ui.available_rect_before_wrap();
                let size = 80.0;
                pfp_rect.set_width(size);
                pfp_rect.set_height(size);
                let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

                ui.horizontal(|ui| {
                    ui.put(
                        pfp_rect,
                        &mut ProfilePic::new(self.note_context.img_cache, get_profile_url(profile))
                            .size(size)
                            .border(ProfilePic::border_stroke(ui)),
                    );

                    if ui.add(copy_key_widget(&pfp_rect)).clicked() {
                        let to_copy = if let Some(bech) = self.pubkey.npub() {
                            bech
                        } else {
                            error!("Could not convert Pubkey to bech");
                            String::new()
                        };
                        ui.ctx().copy_text(to_copy)
                    }

                    ui.with_layout(Layout::right_to_left(egui::Align::RIGHT), |ui| {
                        ui.add_space(24.0);

                        let target_key = self.pubkey;
                        let selected = self.note_context.accounts.get_selected_account();

                        let profile_type = if selected.key.secret_key.is_none() {
                            ProfileType::ReadOnly
                        } else if &selected.key.pubkey == self.pubkey {
                            ProfileType::MyProfile
                        } else {
                            ProfileType::Followable(selected.is_following(target_key.bytes()))
                        };

                        match profile_type {
                            ProfileType::MyProfile => {
                                if ui.add(edit_profile_button()).clicked() {
                                    action = Some(ProfileViewAction::EditProfile);
                                }
                            }
                            ProfileType::Followable(is_following) => {
                                let follow_button = ui.add(follow_button(is_following));

                                if follow_button.clicked() {
                                    action = match is_following {
                                        IsFollowing::Unknown => {
                                            // don't do anything, we don't have contact list
                                            None
                                        }

                                        IsFollowing::Yes => {
                                            Some(ProfileViewAction::Unfollow(target_key.to_owned()))
                                        }

                                        IsFollowing::No => {
                                            Some(ProfileViewAction::Follow(target_key.to_owned()))
                                        }
                                    };
                                }
                            }
                            ProfileType::ReadOnly => {}
                        }
                    });
                });

                ui.add_space(18.0);

                ui.add(display_name_widget(&get_display_name(profile), false));

                ui.add_space(8.0);

                ui.add(about_section_widget(profile));

                ui.horizontal_wrapped(|ui| {
                    let website_url = profile
                        .as_ref()
                        .map(|p| p.record().profile())
                        .and_then(|p| p.and_then(|p| p.website()).filter(|s| !s.is_empty()));

                    let lud16 = profile
                        .as_ref()
                        .map(|p| p.record().profile())
                        .and_then(|p| p.and_then(|p| p.lud16()).filter(|s| !s.is_empty()));

                    if let Some(website_url) = website_url {
                        ui.horizontal(|ui| {
                            handle_link(ui, website_url);
                        });
                    }

                    if let Some(lud16) = lud16 {
                        if website_url.is_some() {
                            ui.end_row();
                        }
                        ui.horizontal(|ui| {
                            handle_lud16(ui, lud16);
                        });
                    }
                });
            });
        });

        action
    }
}

enum ProfileType {
    MyProfile,
    ReadOnly,
    Followable(IsFollowing),
}

fn handle_link(ui: &mut egui::Ui, website_url: &str) {
    let img = if ui.visuals().dark_mode {
        app_images::link_dark_image()
    } else {
        app_images::link_light_image()
    };

    ui.add(img);
    if ui
        .label(RichText::new(website_url).color(notedeck_ui::colors::PINK))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(website_url)
        .interact(Sense::click())
        .clicked()
    {
        if let Err(e) = open::that(website_url) {
            error!("Failed to open URL {} because: {}", website_url, e);
        };
    }
}

fn handle_lud16(ui: &mut egui::Ui, lud16: &str) {
    ui.add(app_images::filled_zap_image());

    let _ = ui
        .label(RichText::new(lud16).color(notedeck_ui::colors::PINK))
        .on_hover_text(lud16);
}

fn copy_key_widget(pfp_rect: &egui::Rect) -> impl egui::Widget + '_ {
    |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        #[allow(deprecated)]
        let copy_key_rect = painter.round_rect_to_pixels(egui::Rect::from_center_size(
            pfp_rect.center_bottom(),
            egui::vec2(48.0, 28.0),
        ));
        let resp = ui
            .interact(
                copy_key_rect,
                ui.id().with("custom_painter"),
                Sense::click(),
            )
            .on_hover_text("Copy npub to clipboard");

        let copy_key_rounding = CornerRadius::same(100);
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
            egui::StrokeKind::Outside,
        );

        app_images::key_image().paint_at(
            ui,
            #[allow(deprecated)]
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
        #[allow(deprecated)]
        let rect = painter.round_rect_to_pixels(rect);

        painter.rect_filled(
            rect,
            CornerRadius::same(8),
            if resp.hovered() {
                ui.visuals().widgets.active.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            },
        );
        painter.rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(8),
            if resp.hovered() {
                ui.visuals().widgets.active.bg_stroke
            } else {
                ui.visuals().widgets.inactive.bg_stroke
            },
            egui::StrokeKind::Outside,
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
            #[allow(deprecated)]
            painter.round_rect_to_pixels(Rect::from_center_size(
                painter.round_pos_to_pixel_center(center),
                edit_icon_size,
            ))
        };

        painter.galley(galley_rect.left_top(), galley, Color32::WHITE);

        app_images::edit_dark_image()
            .tint(ui.visuals().text_color())
            .paint_at(ui, edit_icon_rect);

        resp
    }
}
