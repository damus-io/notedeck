use crate::app_style::NotedeckTextStyle;
use crate::key_parsing::LoginError;
use crate::login_manager::LoginState;
use crate::ui::{Preview, PreviewConfig, View};
use egui::TextEdit;
use egui::{Align, Button, Color32, Frame, InnerResponse, Margin, RichText, Vec2};
use enostr::Keypair;

pub struct AccountLoginView<'a> {
    manager: &'a mut LoginState,
}

pub enum AccountLoginResponse {
    CreateNew,
    LoginWith(Keypair),
}

impl<'a> AccountLoginView<'a> {
    pub fn new(state: &'a mut LoginState) -> Self {
        AccountLoginView { manager: state }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> InnerResponse<Option<AccountLoginResponse>> {
        Frame::none()
            .outer_margin(12.0)
            .show(ui, |ui| self.show(ui))
    }

    fn show(&mut self, ui: &mut egui::Ui) -> Option<AccountLoginResponse> {
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
                    self.manager.should_create_new();
                }
            });
        });

        if self.manager.check_for_create_new() {
            return Some(AccountLoginResponse::CreateNew);
        }

        if let Some(keypair) = self.manager.check_for_successful_login() {
            return Some(AccountLoginResponse::LoginWith(keypair));
        }
        None
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

fn login_textedit_info_text() -> RichText {
    RichText::new("Enter your key")
        .strong()
        .text_style(NotedeckTextStyle::Body.text_style())
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

fn login_textedit(manager: &mut LoginState) -> TextEdit {
    manager.get_login_textedit(|text| {
        egui::TextEdit::singleline(text)
            .hint_text(
                RichText::new("Enter your public key (npub, nip05), or private key (nsec) here...")
                    .text_style(NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(Align::Center)
            .min_size(Vec2::new(0.0, 40.0))
            .margin(Margin::same(12.0))
    })
}

mod preview {
    use super::*;

    pub struct AccountLoginPreview {
        manager: LoginState,
    }

    impl View for AccountLoginPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountLoginView::new(&mut self.manager).ui(ui);
        }
    }

    impl<'a> Preview for AccountLoginView<'a> {
        type Prev = AccountLoginPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            let _ = cfg;
            let manager = LoginState::new();
            AccountLoginPreview { manager }
        }
    }
}
