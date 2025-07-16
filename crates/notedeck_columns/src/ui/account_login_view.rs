use crate::login_manager::AcquireKeyState;
use crate::ui::{Preview, PreviewConfig};
use egui::{
    Align, Button, Color32, Frame, InnerResponse, Layout, Margin, RichText, TextEdit, Vec2,
};
use egui_winit::clipboard::Clipboard;
use enostr::Keypair;
use notedeck::{AppAction, Localization, NotedeckTextStyle, fonts::get_font_size, tr};
use notedeck_ui::{
    app_images,
    context_menu::{PasteBehavior, input_context},
};

pub struct AccountLoginView<'a> {
    manager: &'a mut AcquireKeyState,
    clipboard: &'a mut Clipboard,
    i18n: &'a mut Localization,
}

pub enum AccountLoginResponse {
    CreateNew,
    LoginWith(Keypair),
}

impl<'a> AccountLoginView<'a> {
    pub fn new(
        manager: &'a mut AcquireKeyState,
        clipboard: &'a mut Clipboard,
        i18n: &'a mut Localization,
    ) -> Self {
        AccountLoginView {
            manager,
            clipboard,
            i18n,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> InnerResponse<Option<AccountLoginResponse>> {
        Frame::new().outer_margin(12.0).show(ui, |ui| self.show(ui))
    }

    fn show(&mut self, ui: &mut egui::Ui) -> Option<AccountLoginResponse> {
        ui.vertical(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(32.0);
                ui.label(login_title_text(self.i18n));
            });

            ui.horizontal(|ui| {
                ui.label(login_textedit_info_text(self.i18n));
            });

            ui.vertical_centered_justified(|ui| {
                ui.horizontal(|ui| {
                    let available_width = ui.available_width();
                    let button_width = 32.0;
                    let text_edit_width = available_width - button_width;

                    let textedit_resp = ui.add_sized([text_edit_width, 40.0], login_textedit(self.manager, self.i18n));
                    input_context(&textedit_resp, self.clipboard, self.manager.input_buffer(), PasteBehavior::Clear);

                    if eye_button(ui, self.manager.password_visible()).clicked() {
                        self.manager.toggle_password_visibility();
                    }
                });
                ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                let help_text_style = NotedeckTextStyle::Small;
                ui.add(egui::Label::new(
                    RichText::new(tr!(self.i18n, "Enter your public key (npub), nostr address (e.g. {address}), or private key (nsec). You must enter your private key to be able to post, reply, etc.", "Instructions for entering Nostr credentials", address="vrod@damus.io"))
                        .text_style(help_text_style.text_style())
                        .size(get_font_size(ui.ctx(), &help_text_style)).color(ui.visuals().weak_text_color()),
                    ).wrap())
                });

                self.manager.loading_and_error_ui(ui, self.i18n);

                if ui.add(login_button(self.i18n)).clicked() {
                    self.manager.apply_acquire();
                }
            });

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(tr!(self.i18n,"New to Nostr?", "Label asking if the user is new to Nostr. Underneath this label is a button to create an account."))
                        .color(ui.style().visuals.noninteractive().fg_stroke.color)
                        .text_style(NotedeckTextStyle::Body.text_style()),
                );

                if ui
                    .add(Button::new(RichText::new(tr!(self.i18n,"Create Account", "Button to create a new account"))).frame(false))
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

fn login_title_text(i18n: &mut Localization) -> RichText {
    RichText::new(tr!(i18n, "Login", "Login page title"))
        .text_style(NotedeckTextStyle::Heading2.text_style())
        .strong()
}

fn login_textedit_info_text(i18n: &mut Localization) -> RichText {
    RichText::new(tr!(i18n, "Enter your key", "Label for key input field. Key can be public key (npub), private key (nsec), or Nostr address (NIP-05)."))
        .strong()
        .text_style(NotedeckTextStyle::Body.text_style())
}

fn login_button(i18n: &mut Localization) -> Button<'static> {
    Button::new(
        RichText::new(tr!(i18n, "Login now — let's do this!", "Login button text"))
            .text_style(NotedeckTextStyle::Body.text_style())
            .strong(),
    )
    .fill(Color32::from_rgb(0xF8, 0x69, 0xB6)) // TODO: gradient
    .min_size(Vec2::new(0.0, 40.0))
}

fn login_textedit<'a>(
    manager: &'a mut AcquireKeyState,
    i18n: &'a mut Localization,
) -> TextEdit<'a> {
    let create_textedit = |text| {
        egui::TextEdit::singleline(text)
            .hint_text(
                RichText::new(tr!(
                    i18n,
                    "Your key here...",
                    "Placeholder text for key input field"
                ))
                .text_style(NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(Align::Center)
            .min_size(Vec2::new(0.0, 40.0))
            .margin(Margin::same(12))
    };

    let is_visible = manager.password_visible();
    let mut text_edit = manager.get_acquire_textedit(create_textedit);
    if !is_visible {
        text_edit = text_edit.password(true);
    }
    text_edit
}

fn eye_button(ui: &mut egui::Ui, is_visible: bool) -> egui::Response {
    let is_dark_mode = ui.visuals().dark_mode;
    let icon = if is_visible && is_dark_mode {
        app_images::eye_dark_image()
    } else if is_visible {
        app_images::eye_light_image()
    } else if is_dark_mode {
        app_images::eye_slash_dark_image()
    } else {
        app_images::eye_slash_light_image()
    };
    ui.add(Button::image(icon).frame(false))
}

mod preview {
    use super::*;
    use notedeck::{App, AppContext};

    pub struct AccountLoginPreview {
        manager: AcquireKeyState,
    }

    impl App for AccountLoginPreview {
        fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
            AccountLoginView::new(&mut self.manager, ctx.clipboard, ctx.i18n).ui(ui);

            None
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
