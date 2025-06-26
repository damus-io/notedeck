pub mod edit;

pub use edit::EditProfileView;
use egui::{vec2, Layout, RichText, ScrollArea, Sense};
use enostr::Pubkey;
use nostrdb::{ProfileRecord, Transaction};
use tracing::error;

use crate::{
    timeline::{TimelineCache, TimelineKind},
    ui::timeline::{tabs_ui, TimelineTabView},
};
use notedeck::{
    name::get_display_name, profile::get_profile_url, Accounts, MuteFun, NoteAction, NoteContext,
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

                let kind = TimelineKind::Profile(*self.pubkey);
                let profile_timeline_opt = self.timeline_cache.timelines.get_mut(&kind);

                if let Some(profile_timeline) = profile_timeline_opt {
                    // poll timeline to add notes *before* getting the immutable reference for the view
                    if let Err(e) = profile_timeline.poll_notes_into_pending(
                        self.note_context.ndb,
                        &txn,
                        self.note_context.unknown_ids,
                        self.note_context.note_cache,
                    ) {
                        error!("Profile::poll_notes_into_pending: {e}");
                    }

                    // Now we can use the (implicitly reborrowed) timeline for the view
                    profile_timeline.selected_view =
                        tabs_ui(ui, profile_timeline.selected_view, &profile_timeline.views);

                    if let Some(note_action) = TimelineTabView::new(
                        profile_timeline.current_view(),
                        false, // reversed
                        self.note_options,
                        &txn,
                        self.is_muted,
                        self.note_context,
                        &self
                            .accounts
                            .get_selected_account()
                            .map(|a| (&a.key).into()),
                        self.jobs,
                    )
                    .show(ui)
                    {
                        action = Some(ProfileViewAction::Note(note_action));
                    }
                } else {
                    // Handle case where timeline doesn't exist yet (maybe show loading?)
                    ui.label("Loading profile timeline...");
                }
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

                ui.add_space(padding);
            });
        });
        action
    }
}

fn handle_link(ui: &mut egui::Ui, website_url: &str) {
    let icon_size = 18.0;

    ui.horizontal(|ui| {
        ui.add(app_images::link_image().fit_to_exact_size(vec2(icon_size, icon_size)));
        ui.add_space(4.0);
        ui.hyperlink(website_url);
    });
}

fn handle_lud16(ui: &mut egui::Ui, lud16: &str) {
    let icon_size = 18.0;
    ui.horizontal(|ui| {
        ui.add(app_images::zap_image().fit_to_exact_size(vec2(icon_size, icon_size)));
        ui.add_space(4.0);
        ui.label(lud16);
    });
}

fn copy_key_widget(_pfp_rect: &egui::Rect) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let icon_size = 18.0;
        let padding = 8.0;
        let circle_size = icon_size + 2.0 * padding;
        let desired_size = egui::vec2(circle_size, circle_size);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());

        if ui.is_rect_visible(rect) {
            let _visuals = ui.style().interact_selectable(&response, false);
            let local_response = ui.add(
                app_images::key_image()
                    .max_size(vec2(icon_size, icon_size))
                    .sense(Sense::click()),
            );
            if local_response.clicked() {
                // handle click
            }
        }
        response
    }
}

fn edit_profile_button() -> impl egui::Widget + 'static {
    move |ui: &mut egui::Ui| {
        let text = RichText::new("Edit Profile")
            .text_style(notedeck::NotedeckTextStyle::Button.text_style());
        let button = egui::Button::new(text)
            .min_size(egui::vec2(120.0, 32.0))
            .corner_radius(12.0);
        ui.add(button)
    }
}
