use egui::{vec2, Button, Label, Layout, RichText};

use crate::{
    app_style::{get_font_size, NotedeckTextStyle},
    colors::PINK,
    fonts::NamedFontFamily,
    support::Support,
};

use super::{button_hyperlink::ButtonHyperlink, padding};

pub struct SupportView<'a> {
    support: &'a mut Support,
}

impl<'a> SupportView<'a> {
    pub fn new(support: &'a mut Support) -> Self {
        Self { support }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        padding(8.0, ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 8.0);
            let font = egui::FontId::new(
                get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
                egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
            );
            ui.add(Label::new(RichText::new("Running into a bug?").font(font)));
            ui.label(RichText::new("Step 1").text_style(NotedeckTextStyle::Heading3.text_style()));
            padding(8.0, ui, |ui| {
                ui.label("Open your default email client to get help from the Damus team");
                let size = vec2(120.0, 40.0);
                ui.allocate_ui_with_layout(size, Layout::top_down(egui::Align::Center), |ui| {
                    ui.add(ButtonHyperlink::new(
                        Button::new(
                            RichText::new("Open Email")
                                .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Body)),
                        )
                        .fill(PINK)
                        .min_size(size),
                        self.support.get_mailto_url(),
                    ));
                })
            });

            ui.add_space(8.0);

            if let Some(logs) = self.support.get_most_recent_log() {
                ui.label(
                    RichText::new("Step 2").text_style(NotedeckTextStyle::Heading3.text_style()),
                );
                let size = vec2(80.0, 40.0);
                let copy_button = Button::new(
                    RichText::new("Copy").size(get_font_size(ui.ctx(), &NotedeckTextStyle::Body)),
                )
                .fill(PINK)
                .min_size(size);
                padding(8.0, ui, |ui| {
                    ui.add(Label::new("Press the button below to copy your most recent logs to your system's clipboard. Then paste it into your email.").wrap());
                    ui.allocate_ui_with_layout(size, Layout::top_down(egui::Align::Center), |ui| {
                        if ui.add(copy_button).clicked() {
                            ui.output_mut(|w| {
                                w.copied_text = logs.to_string();
                            });
                        }
                    });
                });
            } else {
                ui.label(
                    egui::RichText::new("ERROR: Could not find logs on system")
                        .color(egui::Color32::RED),
                );
            }
        });
    }
}
