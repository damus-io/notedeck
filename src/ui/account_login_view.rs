use crate::app_style::NotedeckTextStyle;
use crate::key_parsing::LoginError;
use crate::login_manager::LoginManager;
use crate::Damus;
use egui::TextEdit;
use egui::{Align, Align2, Button, Color32, Frame, Margin, RichText, Ui, Vec2, Window};

pub struct AccountLoginView {}

const MIN_WIDTH: f32 = 442.0;

impl AccountLoginView {
    pub fn ui(app: &mut Damus, ui: &mut egui::Ui) -> egui::Response {
        if app.is_mobile() {
            AccountLoginView::mobile_ui(app, ui)
        } else {
            AccountLoginView::desktop_ui(app, ui)
        }
    }

    fn desktop_ui(app: &mut Damus, ui: &mut egui::Ui) -> egui::Response {
        ui.vertical_centered(|ui| {
            ui.add_space(16f32);

            ui.label(login_window_info_text(ui));

            ui.add_space(24.0);

            Frame::none()
                .outer_margin(Margin::symmetric(48.0, 0.0))
                .show(ui, |ui| {
                    Self::login_form(app, ui);
                });

            ui.add_space(32.0);

            let y_margin: f32 = 24.0;
            let generate_frame = egui::Frame::default()
                .fill(ui.style().noninteractive().bg_fill) // TODO: gradient
                .rounding(ui.style().visuals.window_rounding)
                .stroke(ui.style().noninteractive().bg_stroke)
                .inner_margin(Margin::symmetric(48.0, y_margin));

            generate_frame.show(ui, |ui| {
                Self::generate_group(app, ui);
            });
        })
        .response
    }

    fn mobile_ui(app: &mut Damus, ui: &mut egui::Ui) -> egui::Response {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(login_textedit_info_text());
            });

            ui.vertical_centered_justified(|ui| {
                ui.add(login_textedit(&mut app.login_manager));

                Self::loading_and_error(&mut app.login_manager, ui);

                if ui.add(login_button()).clicked() {
                    app.login_manager.apply_login();
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

    pub fn show_mobile(app: &mut Damus, ui: &mut egui::Ui) -> egui::Response {
        egui::CentralPanel::default()
            .show(ui.ctx(), |_| {
                Window::new("Login")
                    .movable(true)
                    .constrain(true)
                    .collapsible(false)
                    .drag_to_scroll(false)
                    .title_bar(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                    .frame(Frame::central_panel(&ui.ctx().style()))
                    .max_width(ui.ctx().screen_rect().width() - 32.0) // margin
                    .show(ui.ctx(), |ui| Self::mobile_ui(app, ui));
            })
            .response
    }

    fn login_form(app: &mut Damus, ui: &mut egui::Ui) {
        ui.vertical_centered_justified(|ui| {
            ui.horizontal(|ui| {
                ui.label(login_textedit_info_text());
            });

            ui.add_space(8f32);

            ui.add(login_textedit(&mut app.login_manager).min_size(Vec2::new(MIN_WIDTH, 40.0)));

            Self::loading_and_error(&mut app.login_manager, ui);

            let login_button = login_button().min_size(Vec2::new(MIN_WIDTH, 40.0));

            if ui.add(login_button).clicked() {
                app.login_manager.apply_login()
            }

            if let Some(acc) = app.login_manager.check_for_successful_login() {
                if app.account_manager.add_account(acc) {
                    app.account_manager
                        .select_account(app.account_manager.num_accounts() - 1)
                }
                app.global_nav.pop();
                app.login_manager.clear();
            }
        });
    }

    fn loading_and_error(manager: &mut LoginManager, ui: &mut egui::Ui) {
        ui.add_space(8.0);

        ui.vertical_centered(|ui| {
            if manager.is_awaiting_network() {
                ui.add(egui::Spinner::new());
            }
        });

        if let Some(err) = manager.check_for_error() {
            show_error(ui, err);
        }

        ui.add_space(8.0);
    }

    fn generate_group(_app: &mut Damus, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("New in nostr?").text_style(NotedeckTextStyle::Heading3.text_style()),
            );

            ui.label(
                RichText::new(" — we got you!")
                    .text_style(NotedeckTextStyle::Heading3.text_style())
                    .color(ui.visuals().noninteractive().fg_stroke.color),
            );
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label(generate_info_text().color(ui.visuals().noninteractive().fg_stroke.color));
        });

        ui.add_space(16.0);

        let generate_button = generate_keys_button().min_size(Vec2::new(MIN_WIDTH, 40.0));
        if ui.add(generate_button).clicked() {
            // TODO: keygen
        }
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
    .min_size(Vec2::new(MIN_WIDTH, 40.0))
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
    use crate::{
        test_data,
        ui::{Preview, PreviewConfig, View},
        Damus,
    };

    use super::AccountLoginView;

    pub struct AccountLoginPreview {
        #[allow(dead_code)]
        is_mobile: bool,
        app: Damus,
    }

    impl AccountLoginPreview {
        fn new(is_mobile: bool) -> Self {
            let app = test_data::test_app(is_mobile);

            AccountLoginPreview { is_mobile, app }
        }
    }

    impl View for AccountLoginPreview {
        fn ui(&mut self, ui: &mut egui::Ui) {
            AccountLoginView::ui(&mut self.app, ui);
        }
    }

    impl Preview for AccountLoginView {
        type Prev = AccountLoginPreview;

        fn preview(cfg: PreviewConfig) -> Self::Prev {
            AccountLoginPreview::new(cfg.is_mobile)
        }
    }
}
