use crate::app_style::NotedeckTextStyle;
use crate::key_parsing::LoginError;
use crate::login_manager::LoginManager;
use crate::ui::{Preview, PreviewConfig, View};
use egui::{Align, Button, Color32, Frame, Margin, Response, RichText, Ui, Vec2};
use egui::{Image, TextEdit};

pub struct AccountLoginView<'a> {
    manager: &'a mut LoginManager,
}

impl<'a> AccountLoginView<'a> {
    pub fn new(manager: &'a mut LoginManager) -> Self {
        AccountLoginView { manager }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Response {
        Frame::none()
            .outer_margin(12.0)
            .show(ui, |ui| {
                self.show(ui);
            })
            .response
    }

    fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        ui.vertical(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(32.0);
                ui.label(login_title_text());
            });

            ui.horizontal(|ui| {
                ui.label(login_textedit_info_text());
            });

            ui.vertical_centered_justified(|ui| {
                ui.add(login_textedit(self.manager));

                self.loading_and_error(ui);

                if ui.add(login_button()).clicked() {
                    self.manager.apply_login();
                }
            });

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("New to Nostr?")
                        .color(ui.style().visuals.noninteractive().fg_stroke.color)
                        .text_style(NotedeckTextStyle::Body.text_style()),
                );

                if ui
                    .add(Button::new(RichText::new("Create Account")).frame(false))
                    .clicked()
                {
                    // TODO: navigate to 'create account' screen
                }
            });
        })
        .response
    }

    fn loading_and_error(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);

        ui.vertical_centered(|ui| {
            if self.manager.is_awaiting_network() {
                ui.add(egui::Spinner::new());
            }
        });

        if let Some(err) = self.manager.check_for_error() {
            show_error(ui, err);
        }

        ui.add_space(8.0);
    }
}

fn show_error(ui: &mut egui::Ui, err: &LoginError) {
    ui.horizontal(|ui| {
        let error_label = match err {
            LoginError::InvalidKey => {
                egui::Label::new(RichText::new("Invalid key.").color(ui.visuals().error_fg_color))
            }
            LoginError::Nip05Failed(e) => {
                egui::Label::new(RichText::new(e).color(ui.visuals().error_fg_color))
            }
        };
        ui.add(error_label.truncate());
    });
}

fn login_title_text() -> RichText {
    RichText::new("Login")
        .text_style(NotedeckTextStyle::Heading2.text_style())
        .strong()
}

fn login_info_text() -> RichText {
    RichText::new("The best alternative to tweetDeck built in nostr protocol")
        .text_style(NotedeckTextStyle::Heading3.text_style())
}

fn login_window_info_text(ui: &Ui) -> RichText {
    RichText::new("Enter your private key to start using Notedeck")
        .text_style(NotedeckTextStyle::Body.text_style())
        .color(ui.visuals().noninteractive().fg_stroke.color)
}

fn login_textedit_info_text() -> RichText {
    RichText::new("Enter your key")
        .strong()
        .text_style(NotedeckTextStyle::Body.text_style())
}

fn logo_unformatted() -> Image<'static> {
    let logo_gradient_data = egui::include_image!("../../assets/Logo-Gradient-2x.png");
    return egui::Image::new(logo_gradient_data);
}

fn generate_info_text() -> RichText {
    RichText::new("Quickly generate your keys. Make sure you save them safely.")
        .text_style(NotedeckTextStyle::Body.text_style())
}

fn generate_keys_button() -> Button<'static> {
    Button::new(RichText::new("Generate keys").text_style(NotedeckTextStyle::Body.text_style()))
}

fn login_button() -> Button<'static> {
    Button::new(
        RichText::new("Login now — let's do this!")
            .text_style(NotedeckTextStyle::Body.text_style())
            .strong(),
    )
    .fill(Color32::from_rgb(0xF8, 0x69, 0xB6)) // TODO: gradient
    .min_size(Vec2::new(0.0, 40.0))
}

fn login_textedit(manager: &mut LoginManager) -> TextEdit {
    manager.get_login_textedit(|text| {
        egui::TextEdit::singleline(text)
            .hint_text(
                RichText::new("Your key here...").text_style(NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(Align::Center)
            .min_size(Vec2::new(0.0, 40.0))
            .margin(Margin::same(12.0))
    })
}

mod preview {
    use super::*;

    pub struct AccountLoginPreview {
        manager: LoginManager,
    }

    impl View for AccountLoginPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountLoginView::new(&mut self.manager).ui(ui);
        }
    }

    impl<'a> Preview for AccountLoginView<'a> {
        type Prev = AccountLoginPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            let manager = LoginManager::new();
            AccountLoginPreview { manager }
        }
    }
}
