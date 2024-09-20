use crate::app_style::NotedeckTextStyle;
use crate::imgcache::ImageCache;
use crate::ui::ProfilePic;
use crate::{colors, images, DisplayName};
use egui::load::TexturePoll;
use egui::{Frame, RichText, Sense, Widget};
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
        crate::ui::padding(12.0, ui, |ui| {
            ui.add(ProfilePic::new(self.cache, get_profile_url(Some(self.profile))).size(80.0));
            ui.add(display_name_widget(
                get_display_name(Some(self.profile)),
                false,
            ));
            ui.add(about_section_widget(self.profile));
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

pub struct SimpleProfilePreview<'a, 'cache> {
    profile: Option<&'a ProfileRecord<'a>>,
    cache: &'cache mut ImageCache,
}

impl<'a, 'cache> SimpleProfilePreview<'a, 'cache> {
    pub fn new(profile: Option<&'a ProfileRecord<'a>>, cache: &'cache mut ImageCache) -> Self {
        SimpleProfilePreview { profile, cache }
    }
}

impl<'a, 'cache> egui::Widget for SimpleProfilePreview<'a, 'cache> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        Frame::none()
            .show(ui, |ui| {
                ui.add(ProfilePic::new(self.cache, get_profile_url(self.profile)).size(48.0));
                ui.vertical(|ui| {
                    ui.add(display_name_widget(get_display_name(self.profile), true));
                });
            })
            .response
    }
}

mod previews {
    use super::*;
    use crate::test_data::test_profile_record;
    use crate::ui::{Preview, PreviewConfig, View};

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

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            ProfilePreviewPreview::new()
        }
    }
}

pub fn get_display_name<'a>(profile: Option<&'a ProfileRecord<'a>>) -> DisplayName<'a> {
    if let Some(name) = profile.and_then(|p| crate::profile::get_profile_name(p)) {
        name
    } else {
        DisplayName::One("??")
    }
}

pub fn get_profile_url<'a>(profile: Option<&'a ProfileRecord<'a>>) -> &'a str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}

pub fn get_profile_url_owned(profile: Option<ProfileRecord<'_>>) -> &str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}

fn display_name_widget(
    display_name: DisplayName<'_>,
    add_placeholder_space: bool,
) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| match display_name {
        DisplayName::One(n) => {
            let name_response =
                ui.label(RichText::new(n).text_style(NotedeckTextStyle::Heading3.text_style()));
            if add_placeholder_space {
                ui.add_space(16.0);
            }
            name_response
        }

        DisplayName::Both {
            display_name,
            username,
        } => {
            ui.label(
                RichText::new(display_name).text_style(NotedeckTextStyle::Heading3.text_style()),
            );

            ui.label(
                RichText::new(format!("@{}", username))
                    .size(12.0)
                    .color(colors::MID_GRAY),
            )
        }
    }
}

pub fn one_line_display_name_widget(
    display_name: DisplayName<'_>,
    style: NotedeckTextStyle,
) -> impl egui::Widget + '_ {
    let text_style = style.text_style();
    move |ui: &mut egui::Ui| match display_name {
        DisplayName::One(n) => ui.label(
            RichText::new(n)
                .text_style(text_style)
                .color(colors::GRAY_SECONDARY),
        ),

        DisplayName::Both {
            display_name,
            username: _,
        } => ui.label(
            RichText::new(display_name)
                .text_style(text_style)
                .color(colors::GRAY_SECONDARY),
        ),
    }
}

fn about_section_widget<'a>(profile: &'a ProfileRecord<'a>) -> impl egui::Widget + 'a {
    |ui: &mut egui::Ui| {
        if let Some(about) = profile.record().profile().and_then(|p| p.about()) {
            ui.label(about)
        } else {
            // need any Response so we dont need an Option
            ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
        }
    }
}
