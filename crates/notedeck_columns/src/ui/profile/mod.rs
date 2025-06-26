pub mod edit;

pub use edit::EditProfileView;
use egui::{vec2, Color32, CornerRadius, Layout, Rect, RichText, ScrollArea, Sense, Stroke};
use enostr::Pubkey;
use nostrdb::{ProfileRecord, Transaction};
use tracing::error;

use crate::{
    timeline::{TimelineCache, TimelineKind},
    ui::timeline::{tabs_ui, TimelineTabView},
};
use notedeck::{
    name::get_display_name, profile::get_profile_url, Accounts, MuteFun, NoteAction, NoteContext,
    NotedeckTextStyle,
};
use notedeck_ui::{
    app_images,
    jobs::JobsCache,
    profile::{about_section_widget, banner, display_name_widget},
    NoteOptions, ProfilePic,
};

pub struct ProfileView<'a, 'd> {
    pubkey: &'a Pubkey,
    accounts: &'a Accounts,
    col_id: usize,
    timeline_cache: &'a mut TimelineCache,
    note_options: NoteOptions,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    jobs: &'a mut JobsCache,
}

pub enum ProfileViewAction {
    EditProfile,
    Note(NoteAction),
}

impl<'a, 'd> ProfileView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pubkey: &'a Pubkey,
        accounts: &'a Accounts,
        col_id: usize,
        timeline_cache: &'a mut TimelineCache,
        note_options: NoteOptions,
        is_muted: &'a MuteFun,
        note_context: &'a mut NoteContext<'d>,
        jobs: &'a mut JobsCache,
    ) -> Self {
        ProfileView {
            pubkey,
            accounts,
            col_id,
            timeline_cache,
            note_options,
            is_muted,
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
            if let Ok(profile) = self
                .note_context
                .ndb
                .get_profile_by_pubkey(&txn, self.pubkey.bytes())
            {
                if self.profile_body(ui, profile) {
                    action = Some(ProfileViewAction::EditProfile);
                }
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
                self.is_muted,
                self.note_context,
                &(&self.accounts.get_selected_account().key).into(),
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

    fn profile_body(&mut self, ui: &mut egui::Ui, profile: ProfileRecord<'_>) -> bool {
        let mut action = false;
        ui.vertical(|ui| {
            banner(
                ui,
                profile.record().profile().and_then(|p| p.banner()),
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
                        &mut ProfilePic::new(
                            self.note_context.img_cache,
                            get_profile_url(Some(&profile)),
                        )
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

                    if self.accounts.contains_full_kp(self.pubkey) {
                        ui.with_layout(Layout::right_to_left(egui::Align::Max), |ui| {
                            if ui.add(edit_profile_button()).clicked() {
                                action = true;
                            }
                        });
                    }
                });

                ui.add_space(18.0);

                ui.add(display_name_widget(
                    &get_display_name(Some(&profile)),
                    false,
                ));

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
    let img = if ui.visuals().dark_mode {
        app_images::link_image()
    } else {
        app_images::link_image().tint(egui::Color32::BLACK)
    };

    ui.add(img);
    if ui
        .label(RichText::new(website_url).color(notedeck_ui::colors::PINK))
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
    let img = if ui.visuals().dark_mode {
        app_images::zap_image()
    } else {
        app_images::zap_image().tint(egui::Color32::BLACK)
    };
    ui.add(img);

    let _ = ui.label(RichText::new(lud16).color(notedeck_ui::colors::PINK));
}

fn copy_key_widget(pfp_rect: &egui::Rect) -> impl egui::Widget + '_ {
    |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        #[allow(deprecated)]
        let copy_key_rect = painter.round_rect_to_pixels(egui::Rect::from_center_size(
            pfp_rect.center_bottom(),
            egui::vec2(48.0, 28.0),
        ));
        let resp = ui.interact(
            copy_key_rect,
            ui.id().with("custom_painter"),
            Sense::click(),
        );

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
