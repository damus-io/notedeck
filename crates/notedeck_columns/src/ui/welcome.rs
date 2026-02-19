use egui::{vec2, Button, Label, Layout, RichText};
use notedeck::{tr, Localization, NotedeckTextStyle};
use notedeck_ui::padding;

pub enum WelcomeResponse {
    CreateAccount,
    Login,
    Browse,
}

pub struct WelcomeView<'a> {
    i18n: &'a mut Localization,
}

impl<'a> WelcomeView<'a> {
    pub fn new(i18n: &'a mut Localization) -> Self {
        Self { i18n }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<WelcomeResponse> {
        let mut response = None;

        padding(16.0, ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 16.0);

            ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                ui.add_space(48.0);

                ui.add(Label::new(
                    RichText::new(tr!(
                        self.i18n,
                        "Welcome to Notedeck",
                        "Welcome screen title"
                    ))
                    .text_style(NotedeckTextStyle::Heading2.text_style()),
                ));

                ui.add_space(8.0);

                let max_width: f32 = 400.0;
                ui.allocate_ui(vec2(max_width.min(ui.available_width()), 0.0), |ui| {
                    ui.add(
                        Label::new(
                            RichText::new(tr!(
                                self.i18n,
                                "Notedeck is a client for Nostr, an open protocol for decentralized social networking. Unlike traditional platforms, no single company controls your feed, your identity, or your data. Your account is a cryptographic key pair \u{2014} no emails, no passwords, no phone numbers.",
                                "Welcome screen body text explaining what Notedeck and Nostr are"
                            ))
                            .text_style(NotedeckTextStyle::Body.text_style()),
                        )
                        .wrap(),
                    );
                });

                ui.add_space(24.0);

                let button_size = vec2(200.0, 40.0);
                let font_size =
                    notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body);

                if ui
                    .add(
                        Button::new(
                            RichText::new(tr!(
                                self.i18n,
                                "Create Account",
                                "Button to create a new Nostr account"
                            ))
                            .size(font_size),
                        )
                        .fill(notedeck_ui::colors::PINK)
                        .min_size(button_size),
                    )
                    .clicked()
                {
                    response = Some(WelcomeResponse::CreateAccount);
                }

                ui.add_space(4.0);

                if ui
                    .add(
                        Button::new(
                            RichText::new(tr!(
                                self.i18n,
                                "I have a Nostr key",
                                "Button for existing Nostr users to log in with their key"
                            ))
                            .size(font_size),
                        )
                        .min_size(button_size),
                    )
                    .clicked()
                {
                    response = Some(WelcomeResponse::Login);
                }

                ui.add_space(4.0);

                if ui
                    .add(
                        Button::new(
                            RichText::new(tr!(
                                self.i18n,
                                "Just browsing",
                                "Button to dismiss welcome and browse the app without an account"
                            ))
                            .size(font_size),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    response = Some(WelcomeResponse::Browse);
                }
            });
        });

        response
    }
}
