pub mod picture;
pub mod preview;

use crate::profile::get_display_name;
use crate::ui::note::NoteOptions;
use crate::{colors, images};
use crate::{notes_holder::NotesHolder, NostrName};
use egui::load::TexturePoll;
use egui::{Label, RichText, ScrollArea, Sense};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
pub use picture::ProfilePic;
pub use preview::ProfilePreview;
use tracing::error;

use crate::{actionbar::NoteAction, notes_holder::NotesHolderStorage, profile::Profile};

use super::timeline::{tabs_ui, TimelineTabView};
use notedeck::{ImageCache, MuteFun, NoteCache, NotedeckTextStyle};

pub struct ProfileView<'a> {
    pubkey: &'a Pubkey,
    col_id: usize,
    profiles: &'a mut NotesHolderStorage<Profile>,
    note_options: NoteOptions,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
}

impl<'a> ProfileView<'a> {
    pub fn new(
        pubkey: &'a Pubkey,
        col_id: usize,
        profiles: &'a mut NotesHolderStorage<Profile>,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        note_options: NoteOptions,
    ) -> Self {
        ProfileView {
            pubkey,
            col_id,
            profiles,
            ndb,
            note_cache,
            img_cache,
            note_options,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, is_muted: &MuteFun) -> Option<NoteAction> {
        let scroll_id = egui::Id::new(("profile_scroll", self.col_id, self.pubkey));

        ScrollArea::vertical()
            .id_salt(scroll_id)
            .show(ui, |ui| {
                let txn = Transaction::new(self.ndb).expect("txn");
                if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, self.pubkey.bytes()) {
                    self.profile_body(ui, profile);
                }
                let profile = self
                    .profiles
                    .notes_holder_mutated(
                        self.ndb,
                        self.note_cache,
                        &txn,
                        self.pubkey.bytes(),
                        is_muted,
                    )
                    .get_ptr();

                profile.timeline.selected_view =
                    tabs_ui(ui, profile.timeline.selected_view, &profile.timeline.views);

                // poll for new notes and insert them into our existing notes
                if let Err(e) = profile.poll_notes_into_view(&txn, self.ndb, is_muted) {
                    error!("Profile::poll_notes_into_view: {e}");
                }

                let reversed = false;

                TimelineTabView::new(
                    profile.timeline.current_view(),
                    reversed,
                    self.note_options,
                    &txn,
                    self.ndb,
                    self.note_cache,
                    self.img_cache,
                )
                .show(ui)
            })
            .inner
    }

    fn profile_body(&mut self, ui: &mut egui::Ui, profile: ProfileRecord<'_>) {
        ui.vertical(|ui| {
            ui.add_sized([ui.available_size().x, 120.0], |ui: &mut egui::Ui| {
                banner(ui, &profile)
            });

            let padding = 12.0;
            crate::ui::padding(padding, ui, |ui| {
                let mut pfp_rect = ui.available_rect_before_wrap();
                let size = 80.0;
                pfp_rect.set_width(size);
                pfp_rect.set_height(size);
                let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

                ui.put(
                    pfp_rect,
                    ProfilePic::new(self.img_cache, get_profile_url(Some(&profile))).size(size),
                );
                ui.add(display_name_widget(get_display_name(Some(&profile)), false));
                ui.add(about_section_widget(&profile));

                if let Some(website_url) = profile.record().profile().and_then(|p| p.website()) {
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
            });
        });
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

        let resp = if let Some(disp_resp) = disp_resp {
            if let Some(username_resp) = username_resp {
                username_resp
            } else {
                disp_resp
            }
        } else {
            ui.add(Label::new(RichText::new(name.name())))
        };

        if add_placeholder_space {
            ui.add_space(16.0);
        }

        resp
    }
}

pub fn get_profile_url<'a>(profile: Option<&ProfileRecord<'a>>) -> &'a str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
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
            ui.label(about)
        } else {
            // need any Response so we dont need an Option
            ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
        }
    }
}

fn banner_texture(
    ui: &mut egui::Ui,
    profile: &ProfileRecord<'_>,
) -> Option<egui::load::SizedTexture> {
    // TODO: cache banner
    let banner = profile.record().profile().and_then(|p| p.banner());

    if let Some(banner) = banner {
        let texture_load_res =
            egui::Image::new(banner).load_for_size(ui.ctx(), ui.available_size());
        if let Ok(texture_poll) = texture_load_res {
            match texture_poll {
                TexturePoll::Pending { .. } => {}
                TexturePoll::Ready { texture, .. } => return Some(texture),
            }
        }
    }

    None
}

fn banner(ui: &mut egui::Ui, profile: &ProfileRecord<'_>) -> egui::Response {
    if let Some(texture) = banner_texture(ui, profile) {
        images::aspect_fill(
            ui,
            Sense::hover(),
            texture.id,
            texture.size.x / texture.size.y,
        )
    } else {
        // TODO: default banner texture
        ui.label("")
    }
}
