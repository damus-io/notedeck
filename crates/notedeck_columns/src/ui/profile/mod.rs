pub mod picture;
pub mod preview;

use crate::ui::note::NoteOptions;
use crate::{colors, images};
use crate::{notes_holder::NotesHolder, DisplayName};
use egui::load::TexturePoll;
use egui::{Label, RichText, ScrollArea, Sense, Widget};
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
                    ProfilePreview::new(&profile, self.img_cache).ui(ui);
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
}

fn display_name_widget(
    display_name: DisplayName<'_>,
    add_placeholder_space: bool,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| match display_name {
        DisplayName::One(n) => {
            let name_response = ui.add(
                Label::new(RichText::new(n).text_style(NotedeckTextStyle::Heading3.text_style()))
                    .selectable(false),
            );
            if add_placeholder_space {
                ui.add_space(16.0);
            }
            name_response
        }

        DisplayName::Both {
            display_name,
            username,
        } => {
            ui.add(
                Label::new(
                    RichText::new(display_name)
                        .text_style(NotedeckTextStyle::Heading3.text_style()),
                )
                .selectable(false),
            );

            ui.add(
                Label::new(
                    RichText::new(format!("@{}", username))
                        .size(12.0)
                        .color(colors::MID_GRAY),
                )
                .selectable(false),
            )
        }
    }
}

pub fn get_profile_url<'a>(profile: Option<&ProfileRecord<'a>>) -> &'a str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}

pub fn get_display_name<'a>(profile: Option<&ProfileRecord<'a>>) -> DisplayName<'a> {
    if let Some(name) = profile.and_then(|p| crate::profile::get_profile_name(p)) {
        name
    } else {
        DisplayName::One("??")
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
