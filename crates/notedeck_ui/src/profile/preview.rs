use crate::ProfilePic;
use egui::{Frame, Label, RichText};
use egui_extras::Size;
use nostrdb::ProfileRecord;

use notedeck::{name::get_display_name, profile::get_profile_url, Images, NotedeckTextStyle};

use super::{about_section_widget, banner, display_name_widget};

pub struct ProfilePreview<'a, 'cache> {
    profile: &'a ProfileRecord<'a>,
    cache: &'cache mut Images,
    banner_height: Size,
}

impl<'a, 'cache> ProfilePreview<'a, 'cache> {
    pub fn new(profile: &'a ProfileRecord<'a>, cache: &'cache mut Images) -> Self {
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
        crate::padding(padding, ui, |ui| {
            let mut pfp_rect = ui.available_rect_before_wrap();
            let size = 80.0;
            pfp_rect.set_width(size);
            pfp_rect.set_height(size);
            let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

            ui.put(
                pfp_rect,
                &mut ProfilePic::new(self.cache, get_profile_url(Some(self.profile)))
                    .size(size)
                    .border(ProfilePic::border_stroke(ui)),
            );
            ui.add(display_name_widget(
                &get_display_name(Some(self.profile)),
                false,
            ));
            ui.add(about_section_widget(Some(self.profile)));
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
    cache: &'cache mut Images,
    is_nsec: bool,
}

impl<'a, 'cache> SimpleProfilePreview<'a, 'cache> {
    pub fn new(
        profile: Option<&'a ProfileRecord<'a>>,
        cache: &'cache mut Images,
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
        Frame::new()
            .show(ui, |ui| {
                ui.add(&mut ProfilePic::new(self.cache, get_profile_url(self.profile)).size(48.0));
                ui.vertical(|ui| {
                    ui.add(display_name_widget(&get_display_name(self.profile), true));
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
