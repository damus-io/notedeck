pub mod picture;
pub mod preview;

use crate::profile::get_display_name;
use crate::ui::note::NoteOptions;
use crate::{colors, images};
use crate::{notes_holder::NotesHolder, NostrName};
use egui::load::TexturePoll;
use egui::{Label, RichText, Rounding, ScrollArea, Sense, Stroke};
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
            let resp = ui.label(about);
            ui.add_space(8.0);
            resp
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
