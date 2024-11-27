use crate::app_style::{get_font_size, NotedeckTextStyle};
use crate::imgcache::ImageCache;
use crate::storage::{DataPath, DataPathType};
use crate::ui::ProfilePic;
use crate::user_account::UserAccount;
use crate::{colors, images, DisplayName};
use egui::load::TexturePoll;
use egui::{Frame, Label, RichText, Sense, Widget};
use egui_extras::Size;
use enostr::NoteId;
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

impl egui::Widget for ProfilePreview<'_, '_> {
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
                                RichText::new("View only mode")
                                    .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Tiny))
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
    use crate::ui::{Preview, PreviewConfig, View};

    pub struct ProfilePreviewPreview<'a> {
        profile: ProfileRecord<'a>,
        cache: ImageCache,
    }

    impl ProfilePreviewPreview<'_> {
        pub fn new() -> Self {
            let profile = test_profile_record();
            let path = DataPath::new("previews")
                .path(DataPathType::Cache)
                .join(ImageCache::rel_dir());
            let cache = ImageCache::new(path);
            ProfilePreviewPreview { profile, cache }
        }
    }

    impl Default for ProfilePreviewPreview<'_> {
        fn default() -> Self {
            ProfilePreviewPreview::new()
        }
    }

    impl View for ProfilePreviewPreview<'_> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ProfilePreview::new(&self.profile, &mut self.cache).ui(ui);
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

fn get_display_name_as_string(profile: Option<&'_ ProfileRecord<'_>>) -> String {
    let display_name = get_display_name(profile);
    match display_name {
        DisplayName::One(n) => n.to_string(),
        DisplayName::Both { display_name, .. } => display_name.to_string(),
    }
}

pub fn get_profile_displayname_string(ndb: &nostrdb::Ndb, pk: &enostr::Pubkey) -> String {
    let txn = nostrdb::Transaction::new(ndb).expect("Transaction should have worked");
    let profile = ndb.get_profile_by_pubkey(&txn, pk.bytes()).ok();
    get_display_name_as_string(profile.as_ref())
}

pub fn get_note_users_displayname_string(ndb: &nostrdb::Ndb, id: &NoteId) -> String {
    let txn = nostrdb::Transaction::new(ndb).expect("Transaction should have worked");
    let note = ndb.get_note_by_id(&txn, id.bytes());
    let profile = if let Ok(note) = note {
        ndb.get_profile_by_pubkey(&txn, note.pubkey()).ok()
    } else {
        None
    };
    get_display_name_as_string(profile.as_ref())
}
