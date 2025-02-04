use core::f32;

use egui::{vec2, Button, Layout, Margin, RichText, Rounding, ScrollArea, TextEdit};
use notedeck::{ImageCache, NotedeckTextStyle};

use crate::{colors, profile_state::ProfileState};

use super::{banner, unwrap_profile_url, ProfilePic};

pub struct EditProfileView<'a> {
    state: &'a mut ProfileState,
    img_cache: &'a mut ImageCache,
}

impl<'a> EditProfileView<'a> {
    pub fn new(state: &'a mut ProfileState, img_cache: &'a mut ImageCache) -> Self {
        Self { state, img_cache }
    }

    // return true to save
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        ScrollArea::vertical()
            .show(ui, |ui| {
                banner(ui, Some(&self.state.banner), 188.0);

                let padding = 24.0;
                crate::ui::padding(padding, ui, |ui| {
                    self.inner(ui, padding);
                });

                ui.separator();

                let mut save = false;
                crate::ui::padding(padding, ui, |ui| {
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(button("Save changes", 119.0).fill(colors::PINK))
                            .clicked()
                        {
                            save = true;
                        }
                    });
                });

                save
            })
            .inner
    }

    fn inner(&mut self, ui: &mut egui::Ui, padding: f32) {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 16.0);
        let mut pfp_rect = ui.available_rect_before_wrap();
        let size = 80.0;
        pfp_rect.set_width(size);
        pfp_rect.set_height(size);
        let pfp_rect = pfp_rect.translate(egui::vec2(0.0, -(padding + 2.0 + (size / 2.0))));

        let pfp_url = unwrap_profile_url(if self.state.picture.is_empty() {
            None
        } else {
            Some(&self.state.picture)
        });
        ui.put(
            pfp_rect,
            ProfilePic::new(self.img_cache, pfp_url)
                .size(size)
                .border(ProfilePic::border_stroke(ui)),
        );

        in_frame(ui, |ui| {
            ui.add(label("Display name"));
            ui.add(singleline_textedit(&mut self.state.display_name));
        });

        in_frame(ui, |ui| {
            ui.add(label("Username"));
            ui.add(singleline_textedit(&mut self.state.name));
        });

        in_frame(ui, |ui| {
            ui.add(label("Profile picture"));
            ui.add(multiline_textedit(&mut self.state.picture));
        });

        in_frame(ui, |ui| {
            ui.add(label("Banner"));
            ui.add(multiline_textedit(&mut self.state.banner));
        });

        in_frame(ui, |ui| {
            ui.add(label("About"));
            ui.add(multiline_textedit(&mut self.state.about));
        });

        in_frame(ui, |ui| {
            ui.add(label("Website"));
            ui.add(singleline_textedit(&mut self.state.website));
        });

        in_frame(ui, |ui| {
            ui.add(label("Lightning network address (lud16)"));
            ui.add(multiline_textedit(&mut self.state.lud16));
        });

        in_frame(ui, |ui| {
            ui.add(label("Nostr address (NIP-05 identity)"));
            ui.add(singleline_textedit(&mut self.state.nip05));
            let split = &mut self.state.nip05.split('@');
            let prefix = split.next();
            let suffix = split.next();
            if let Some(prefix) = prefix {
                if let Some(suffix) = suffix {
                    let use_domain = if let Some(f) = prefix.chars().next() {
                        f == '_'
                    } else {
                        false
                    };
                    ui.colored_label(
                        ui.visuals().noninteractive().fg_stroke.color,
                        RichText::new(if use_domain {
                            format!("\"{}\" will be used for identification", suffix)
                        } else {
                            format!(
                                "\"{}\" at \"{}\" will be used for identification",
                                prefix, suffix
                            )
                        }),
                    );
                }
            }
        });
    }
}

fn label(text: &str) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        ui.label(RichText::new(text).font(NotedeckTextStyle::Body.get_bolded_font(ui.ctx())))
    }
}

fn singleline_textedit(data: &mut String) -> impl egui::Widget + '_ {
    TextEdit::singleline(data)
        .min_size(vec2(0.0, 40.0))
        .vertical_align(egui::Align::Center)
        .margin(Margin::symmetric(12.0, 10.0))
        .desired_width(f32::INFINITY)
}

fn multiline_textedit(data: &mut String) -> impl egui::Widget + '_ {
    TextEdit::multiline(data)
        // .min_size(vec2(0.0, 40.0))
        .vertical_align(egui::Align::TOP)
        .margin(Margin::symmetric(12.0, 10.0))
        .desired_width(f32::INFINITY)
        .desired_rows(1)
}

fn in_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none().show(ui, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 8.0);
        contents(ui);
    });
}

fn button(text: &str, width: f32) -> egui::Button<'static> {
    Button::new(text)
        .rounding(Rounding::same(8.0))
        .min_size(vec2(width, 40.0))
}

mod preview {
    use notedeck::App;

    use crate::{
        profile_state::ProfileState,
        test_data,
        ui::{Preview, PreviewConfig},
    };

    use super::EditProfileView;

    pub struct EditProfilePreivew {
        state: ProfileState,
    }

    impl Default for EditProfilePreivew {
        fn default() -> Self {
            Self {
                state: ProfileState::from_profile(&test_data::test_profile_record()),
            }
        }
    }

    impl App for EditProfilePreivew {
        fn update(&mut self, ctx: &mut notedeck::AppContext<'_>, ui: &mut egui::Ui) {
            EditProfileView::new(&mut self.state, ctx.img_cache).ui(ui);
        }
    }

    impl Preview for EditProfileView<'_> {
        type Prev = EditProfilePreivew;

        fn preview(_cfg: PreviewConfig) -> Self::Prev {
            EditProfilePreivew::default()
        }
    }
}
