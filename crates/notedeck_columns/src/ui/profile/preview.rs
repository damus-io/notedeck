use crate::ui::ProfilePic;
use crate::NostrName;
use egui::{Frame, Label, RichText, Widget};
use egui_extras::Size;
use nostrdb::ProfileRecord;

use notedeck::{ImageCache, NotedeckTextStyle, UserAccount};

use super::{about_section_widget, banner, display_name_widget, get_display_name, get_profile_url};

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

    fn body(self, ui: &mut egui::Ui) {
        let padding = 12.0;
        crate::ui::padding(padding, ui, |ui| {
            let mut pfp_rect = ui.available_rect_before_wrap();
            let size = 80.0;
            pfp_rect.set_width(size);
            pfp_rect.set_height(size);
            let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

            ui.put(
                pfp_rect,
                ProfilePic::new(self.cache, get_profile_url(Some(self.profile))).size(size),
            );
            ui.add(display_name_widget(
                get_display_name(Some(self.profile)),
                false,
            ));
            ui.add(about_section_widget(self.profile));
        });
    }
}

impl egui::Widget for ProfilePreview<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        ui.vertical(|ui| {
            banner(
                ui,
                self.profile.record().profile().and_then(|p| p.banner()),
                80.0,
            );

            self.body(ui);
        })
        .response
    }
}

pub struct SimpleProfilePreview<'a, 'cache> {
    profile: Option<&'a ProfileRecord<'a>>,
    cache: &'cache mut ImageCache,
    is_nsec: bool,
}

impl<'a, 'cache> SimpleProfilePreview<'a, 'cache> {
    pub fn new(
        profile: Option<&'a ProfileRecord<'a>>,
        cache: &'cache mut ImageCache,
        is_nsec: bool,
    ) -> Self {
        SimpleProfilePreview {
            profile,
            cache,
            is_nsec,
        }
    }
}

impl egui::Widget for SimpleProfilePreview<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        Frame::none()
            .show(ui, |ui| {
                ui.add(ProfilePic::new(self.cache, get_profile_url(self.profile)).size(48.0));
                ui.vertical(|ui| {
                    ui.add(display_name_widget(get_display_name(self.profile), true));
                    if !self.is_nsec {
                        ui.add(
                            Label::new(
                                RichText::new("Read only")
                                    .size(notedeck::fonts::get_font_size(
                                        ui.ctx(),
                                        &NotedeckTextStyle::Tiny,
                                    ))
                                    .color(ui.visuals().warn_fg_color),
                            )
                            .selectable(false),
                        );
                    }
                });
            })
            .response
    }
}

mod previews {
    use super::*;
    use crate::test_data::test_profile_record;
    use crate::ui::{Preview, PreviewConfig};
    use notedeck::{App, AppContext};

    pub struct ProfilePreviewPreview<'a> {
        profile: ProfileRecord<'a>,
    }

    impl ProfilePreviewPreview<'_> {
        pub fn new() -> Self {
            let profile = test_profile_record();
            ProfilePreviewPreview { profile }
        }
    }

    impl Default for ProfilePreviewPreview<'_> {
        fn default() -> Self {
            ProfilePreviewPreview::new()
        }
    }

    impl App for ProfilePreviewPreview<'_> {
        fn update(&mut self, app: &mut AppContext<'_>, ui: &mut egui::Ui) {
            ProfilePreview::new(&self.profile, app.img_cache).ui(ui);
        }
    }

    impl<'a> Preview for ProfilePreview<'a, '_> {
        /// A preview of the profile preview :D
        type Prev = ProfilePreviewPreview<'a>;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            ProfilePreviewPreview::new()
        }
    }
}

pub fn get_profile_url_owned(profile: Option<ProfileRecord<'_>>) -> &str {
    if let Some(url) = profile.and_then(|pr| pr.record().profile().and_then(|p| p.picture())) {
        url
    } else {
        ProfilePic::no_pfp_url()
    }
}

pub fn get_account_url<'a>(
    txn: &'a nostrdb::Transaction,
    ndb: &nostrdb::Ndb,
    account: Option<&UserAccount>,
) -> &'a str {
    if let Some(selected_account) = account {
        if let Ok(profile) = ndb.get_profile_by_pubkey(txn, selected_account.pubkey.bytes()) {
            get_profile_url_owned(Some(profile))
        } else {
            get_profile_url_owned(None)
        }
    } else {
        get_profile_url(None)
    }
}

pub fn one_line_display_name_widget<'a>(
    visuals: &egui::Visuals,
    display_name: NostrName<'a>,
    style: NotedeckTextStyle,
) -> impl egui::Widget + 'a {
    let text_style = style.text_style();
    let color = visuals.noninteractive().fg_stroke.color;

    move |ui: &mut egui::Ui| -> egui::Response {
        ui.label(
            RichText::new(display_name.name())
                .text_style(text_style)
                .color(color),
        )
    }
}
