use egui::{vec2, Button, Label, Layout, RichText, ScrollArea};
use notedeck::{tr, Localization, NotedeckTextStyle};
use notedeck_ui::padding;

const EULA_TEXT: &str = include_str!("../../../../docs/EULA.md");

pub enum TosAcceptanceResponse {
    Accept,
}

pub struct TosAcceptanceView<'a> {
    i18n: &'a mut Localization,
    age_confirmed: &'a mut bool,
    tos_confirmed: &'a mut bool,
}

impl<'a> TosAcceptanceView<'a> {
    pub fn new(
        i18n: &'a mut Localization,
        age_confirmed: &'a mut bool,
        tos_confirmed: &'a mut bool,
    ) -> Self {
        Self {
            i18n,
            age_confirmed,
            tos_confirmed,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<TosAcceptanceResponse> {
        let mut response = None;

        padding(16.0, ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 12.0);

            ui.add(Label::new(
                RichText::new(tr!(
                    self.i18n,
                    "Terms of Service",
                    "TOS acceptance screen title"
                ))
                .text_style(NotedeckTextStyle::Heading2.text_style()),
            ));

            ui.add(Label::new(
                RichText::new(tr!(
                    self.i18n,
                    "Please read and accept the following terms to continue.",
                    "TOS acceptance instruction text"
                ))
                .text_style(NotedeckTextStyle::Body.text_style()),
            ));

            let available = ui.available_height() - 120.0;
            let scroll_height = available.max(200.0);

            egui::Frame::group(ui.style())
                .fill(ui.style().visuals.extreme_bg_color)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .max_height(scroll_height)
                        .show(ui, |ui| {
                            ui.add(
                                Label::new(
                                    RichText::new(EULA_TEXT)
                                        .text_style(NotedeckTextStyle::Body.text_style()),
                                )
                                .wrap(),
                            );
                        });
                });

            ui.checkbox(
                self.age_confirmed,
                tr!(
                    self.i18n,
                    "I confirm that I am at least 17 years old",
                    "Age verification checkbox label"
                ),
            );

            ui.checkbox(
                self.tos_confirmed,
                tr!(
                    self.i18n,
                    "I have read and agree to the Terms of Service",
                    "TOS agreement checkbox label"
                ),
            );

            let can_accept = *self.age_confirmed && *self.tos_confirmed;
            let button_size = vec2(200.0, 40.0);

            ui.allocate_ui_with_layout(button_size, Layout::top_down(egui::Align::Center), |ui| {
                let font_size = notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
                let button = Button::new(
                    RichText::new(tr!(
                        self.i18n,
                        "Accept and Continue",
                        "Button to accept TOS and continue using the app"
                    ))
                    .size(font_size),
                )
                .min_size(button_size);

                let button = if can_accept {
                    button.fill(notedeck_ui::colors::PINK)
                } else {
                    button
                };

                if ui.add_enabled(can_accept, button).clicked() {
                    response = Some(TosAcceptanceResponse::Accept);
                }
            });
        });

        response
    }
}
