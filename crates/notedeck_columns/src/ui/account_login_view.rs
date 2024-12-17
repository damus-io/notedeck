use crate::login_manager::AcquireKeyState;
use crate::ui::{Preview, PreviewConfig, View};
use egui::TextEdit;
use egui::{Align, Button, Color32, Frame, InnerResponse, Margin, RichText, Vec2};
use enostr::Keypair;
use notedeck::NotedeckTextStyle;

pub struct AccountLoginView<'a> {
    manager: &'a mut AcquireKeyState,
}

pub enum AccountLoginResponse {
    CreateNew,
    LoginWith(Keypair),
}

impl<'a> AccountLoginView<'a> {
    pub fn new(state: &'a mut AcquireKeyState) -> Self {
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

                self.manager.loading_and_error_ui(ui);

                if ui.add(login_button()).clicked() {
                    self.manager.apply_acquire();
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

        if let Some(keypair) = self.manager.get_login_keypair() {
            return Some(AccountLoginResponse::LoginWith(keypair.clone()));
        }
        None
    }
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

fn login_textedit(manager: &mut AcquireKeyState) -> TextEdit {
    manager.get_acquire_textedit(|text| {
        egui::TextEdit::singleline(text)
            .hint_text(
                RichText::new("Enter your public key (npub), nostr address (e.g. vrod@damus.io), or private key (nsec) here...")
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
        manager: AcquireKeyState,
    }

    impl View for AccountLoginPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountLoginView::new(&mut self.manager).ui(ui);
        }
    }

    impl Preview for AccountLoginView<'_> {
        type Prev = AccountLoginPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            let _ = cfg;
            let manager = AcquireKeyState::new();
            AccountLoginPreview { manager }
        }
    }
}
