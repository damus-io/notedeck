use crate::app_style::NotedeckTextStyle;
use crate::imgcache::ImageCache;
use crate::ui::ProfilePic;
use crate::{colors, images, DisplayName};
use egui::load::TexturePoll;
use egui::{RichText, Sense};
use egui_extras::Size;
use nostrdb::ProfileRecord;

pub struct ProfilePreview<'a, 'cache> {
    profile: &'a ProfileRecord<'a>,
    cache: &'cache mut ImageCache,
    banner_height: Size,
}

impl<'a, 'cache> ProfilePreview<'a, 'cache> {
    pub fn new(profile: &'a ProfileRecord<'a>, cache: &'cache mut ImageCache) -> Self {
        let banner_height = Size::exact(80.0);
        ProfilePreview {
            profile,
            cache,
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

    fn body(self, ui: &mut egui::Ui) {
        let name = if let Some(name) = crate::profile::get_profile_name(self.profile) {
            name
        } else {
            DisplayName::One("??")
        };

        crate::ui::padding(12.0, ui, |ui| {
            let url = if let Some(url) = self.profile.record().profile().and_then(|p| p.picture()) {
                url
            } else {
                ProfilePic::no_pfp_url()
            };

            ui.add(ProfilePic::new(self.cache, url).size(80.0));

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

            if let Some(about) = self.profile.record().profile().and_then(|p| p.about()) {
                ui.label(about);
            }
        });
    }
}

impl<'a, 'cache> egui::Widget for ProfilePreview<'a, 'cache> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        ui.vertical(|ui| {
            ui.add_sized([ui.available_size().x, 80.0], |ui: &mut egui::Ui| {
                ProfilePreview::banner(ui, self.profile)
            });

            self.body(ui);
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
        cache: ImageCache,
    }

    impl<'a> ProfilePreviewPreview<'a> {
        pub fn new() -> Self {
            let profile = test_profile_record();
            let cache = ImageCache::new(ImageCache::rel_datadir().into());
            ProfilePreviewPreview { profile, cache }
        }
    }

    impl<'a> Default for ProfilePreviewPreview<'a> {
        fn default() -> Self {
            ProfilePreviewPreview::new()
        }
    }

    impl<'a> View for ProfilePreviewPreview<'a> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ProfilePreview::new(&self.profile, &mut self.cache).ui(ui);
        }
    }

    impl<'a, 'cache> Preview for ProfilePreview<'a, 'cache> {
        /// A preview of the profile preview :D
        type Prev = ProfilePreviewPreview<'a>;

        fn preview() -> Self::Prev {
            ProfilePreviewPreview::new()
        }
    }
}
