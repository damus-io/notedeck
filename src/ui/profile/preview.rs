use crate::app_style::NotedeckTextStyle;
use crate::{colors, images, DisplayName};
use egui::load::TexturePoll;
use egui::{RichText, Sense};
use egui_extras::Size;
use nostrdb::ProfileRecord;

pub struct ProfilePreview<'a> {
    profile: &'a ProfileRecord<'a>,
    banner_height: Size,
}

impl<'a> ProfilePreview<'a> {
    pub fn new(profile: &'a ProfileRecord<'a>) -> Self {
        let banner_height = Size::exact(80.0);
        ProfilePreview {
            profile,
            banner_height,
        }
    }

    pub fn banner_height(&mut self, size: Size) {
        self.banner_height = size;
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
        if let Some(texture) = Self::banner_texture(ui, profile) {
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

    fn body(ui: &mut egui::Ui, profile: &ProfileRecord<'_>) {
        let name = if let Some(name) = crate::profile::get_profile_name(profile) {
            name
        } else {
            DisplayName::One("??")
        };

        crate::ui::padding(12.0, ui, |ui| {
            match name {
                DisplayName::One(n) => {
                    ui.label(RichText::new(n).text_style(NotedeckTextStyle::Heading3.text_style()));
                }

                DisplayName::Both {
                    display_name,
                    username,
                } => {
                    ui.label(
                        RichText::new(display_name)
                            .text_style(NotedeckTextStyle::Heading3.text_style()),
                    );

                    ui.label(
                        RichText::new(format!("@{}", username))
                            .size(12.0)
                            .color(colors::MID_GRAY),
                    );
                }
            }

            if let Some(about) = profile.record().profile().and_then(|p| p.about()) {
                ui.label(about);
            }
        });
    }
}

impl<'a> egui::Widget for ProfilePreview<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        ui.vertical(|ui| {
            ui.add_sized([ui.available_size().x, 80.0], |ui: &mut egui::Ui| {
                ProfilePreview::banner(ui, self.profile)
            });

            ProfilePreview::body(ui, self.profile);
        })
        .response
    }
}

mod previews {
    use super::*;
    use crate::test_data::test_profile_record;
    use crate::ui::{Preview, View};
    use egui::Widget;

    pub struct ProfilePreviewPreview<'a> {
        profile: ProfileRecord<'a>,
    }

    impl<'a> ProfilePreviewPreview<'a> {
        pub fn new() -> Self {
            let profile = test_profile_record();
            ProfilePreviewPreview { profile }
        }
    }

    impl<'a> Default for ProfilePreviewPreview<'a> {
        fn default() -> Self {
            ProfilePreviewPreview::new()
        }
    }

    impl<'a> View for ProfilePreviewPreview<'a> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ProfilePreview::new(&self.profile).ui(ui);
        }
    }

    impl<'a> Preview for ProfilePreview<'a> {
        /// A preview of the profile preview :D
        type Prev = ProfilePreviewPreview<'a>;

        fn preview() -> Self::Prev {
            ProfilePreviewPreview::new()
        }
    }
}
