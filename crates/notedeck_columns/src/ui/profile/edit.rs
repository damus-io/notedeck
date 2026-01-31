use core::f32;

use egui::{vec2, Button, CornerRadius, Layout, Margin, RichText, ScrollArea, TextEdit};
use egui_winit::clipboard::Clipboard;
use enostr::ProfileState;
use notedeck::DragResponse;
use notedeck::{
    profile::unwrap_profile_url, tr, Images, Localization, MediaJobSender, NotedeckTextStyle,
};
use notedeck_ui::context_menu::{input_context, PasteBehavior};
use notedeck_ui::{profile::banner, ProfilePic};

pub struct EditProfileView<'a> {
    state: &'a mut ProfileState,
    clipboard: &'a mut Clipboard,
    img_cache: &'a mut Images,
    i18n: &'a mut Localization,
    jobs: &'a MediaJobSender,
}

impl<'a> EditProfileView<'a> {
    pub fn new(
        i18n: &'a mut Localization,
        state: &'a mut ProfileState,
        img_cache: &'a mut Images,
        clipboard: &'a mut Clipboard,
        jobs: &'a MediaJobSender,
    ) -> Self {
        Self {
            i18n,
            state,
            img_cache,
            clipboard,
            jobs,
        }
    }

    pub fn scroll_id() -> egui::Id {
        egui::Id::new("edit_profile")
    }

    // return true to save
    pub fn ui(&mut self, ui: &mut egui::Ui) -> DragResponse<bool> {
        let scroll_out = ScrollArea::vertical()
            .id_salt(EditProfileView::scroll_id())
            .stick_to_bottom(true)
            .show(ui, |ui| {
                banner(ui, self.img_cache, self.jobs, self.state.banner(), 188.0);

                let padding = 24.0;
                notedeck_ui::padding(padding, ui, |ui| {
                    self.inner(ui, padding);
                });

                ui.separator();

                let mut save = false;
                notedeck_ui::padding(padding, ui, |ui| {
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                button(
                                    tr!(
                                        self.i18n,
                                        "Save changes",
                                        "Button label to save profile changes"
                                    )
                                    .as_str(),
                                    119.0,
                                )
                                .fill(notedeck_ui::colors::PINK),
                            )
                            .clicked()
                        {
                            save = true;
                        }
                    });
                });

                Some(save)
            });
        DragResponse::scroll(scroll_out)
    }

    fn inner(&mut self, ui: &mut egui::Ui, padding: f32) {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 16.0);
        let mut pfp_rect = ui.available_rect_before_wrap();
        let size = 80.0;
        pfp_rect.set_width(size);
        pfp_rect.set_height(size);
        let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

        let pfp_url = unwrap_profile_url(self.state.picture());
        ui.put(
            pfp_rect,
            &mut ProfilePic::new(self.img_cache, self.jobs, pfp_url)
                .size(size)
                .border(ProfilePic::border_stroke(ui)),
        );

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(
                    self.i18n,
                    "Display name",
                    "Profile display name field label"
                )
                .as_str(),
            ));
            singleline_textedit(ui, self.state.str_mut("display_name"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(self.i18n, "Username", "Profile username field label").as_str(),
            ));
            singleline_textedit(ui, self.state.str_mut("name"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(
                    self.i18n,
                    "Profile picture",
                    "Profile picture URL field label"
                )
                .as_str(),
            ));
            multiline_textedit(ui, self.state.str_mut("picture"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(self.i18n, "Banner", "Profile banner URL field label").as_str(),
            ));
            multiline_textedit(ui, self.state.str_mut("banner"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(self.i18n, "About", "Profile about/bio field label").as_str(),
            ));
            multiline_textedit(ui, self.state.str_mut("about"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(self.i18n, "Website", "Profile website field label").as_str(),
            ));
            singleline_textedit(ui, self.state.str_mut("website"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(
                    self.i18n,
                    "Lightning network address (lud16)",
                    "Bitcoin Lightning network address field label"
                )
                .as_str(),
            ));
            multiline_textedit(ui, self.state.str_mut("lud16"), self.clipboard);
        });

        in_frame(ui, |ui| {
            ui.add(label(
                tr!(
                    self.i18n,
                    "Nostr address (NIP-05 identity)",
                    "NIP-05 identity field label"
                )
                .as_str(),
            ));

            singleline_textedit(ui, self.state.str_mut("nip05"), self.clipboard);

            let Some(nip05) = self.state.nip05() else {
                return;
            };

            let mut split = nip05.split('@');

            let Some(prefix) = split.next() else {
                return;
            };
            let Some(suffix) = split.next() else {
                return;
            };

            let use_domain = if let Some(f) = prefix.chars().next() {
                f == '_'
            } else {
                false
            };
            ui.colored_label(
                ui.visuals().noninteractive().fg_stroke.color,
                RichText::new(if use_domain {
                    tr!(
                        self.i18n,
                        "\"{domain}\" will be used for identification",
                        "Domain identification message",
                        domain = suffix
                    )
                } else {
                    tr!(
                        self.i18n,
                        "\"{username}\" at \"{domain}\" will be used for identification",
                        "Username and domain identification message",
                        username = prefix,
                        domain = suffix
                    )
                }),
            );
        });
    }
}

fn label(text: &str) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        ui.label(RichText::new(text).font(NotedeckTextStyle::Body.get_bolded_font(ui.ctx())))
    }
}

fn singleline_textedit(ui: &mut egui::Ui, data: &mut String, clipboard: &mut Clipboard) {
    let r = ui.add(
        TextEdit::singleline(data)
            .min_size(vec2(0.0, 40.0))
            .vertical_align(egui::Align::Center)
            .margin(Margin::symmetric(12, 10))
            .desired_width(f32::INFINITY),
    );

    input_context(ui, &r, clipboard, data, PasteBehavior::Clear);
}

fn multiline_textedit(ui: &mut egui::Ui, data: &mut String, clipboard: &mut Clipboard) {
    let r = ui.add(
        TextEdit::multiline(data)
            // .min_size(vec2(0.0, 40.0))
            .vertical_align(egui::Align::TOP)
            .margin(Margin::symmetric(12, 10))
            .desired_width(f32::INFINITY)
            .desired_rows(1),
    );

    input_context(ui, &r, clipboard, data, PasteBehavior::Clear);
}

fn in_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().show(ui, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 8.0);
        contents(ui);
    });
}

fn button(text: &str, width: f32) -> egui::Button<'static> {
    Button::new(text)
        .corner_radius(CornerRadius::same(8))
        .min_size(vec2(width, 40.0))
}
