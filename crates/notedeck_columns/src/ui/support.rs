use crate::support::{Support, SUPPORT_EMAIL};
use egui::{vec2, Button, Label, Layout, RichText};
use notedeck::{tr, Localization, NamedFontFamily, NotedeckTextStyle};
use notedeck_ui::{colors::PINK, padding};
use robius_open::Uri;
use tracing::error;

pub struct SupportView<'a> {
    support: &'a mut Support,
    i18n: &'a mut Localization,
}

impl<'a> SupportView<'a> {
    pub fn new(support: &'a mut Support, i18n: &'a mut Localization) -> Self {
        Self { support, i18n }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        padding(8.0, ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 8.0);
            let font = egui::FontId::new(
                notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
                egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
            );
            ui.add(Label::new(
                RichText::new(tr!(
                    self.i18n,
                    "Running into a bug?",
                    "Heading for support section"
                ))
                .font(font),
            ));
            ui.label(
                RichText::new(tr!(
                    self.i18n,
                    "Step 1",
                    "Step 1 label in support instructions"
                ))
                .text_style(NotedeckTextStyle::Heading3.text_style()),
            );
            padding(8.0, ui, |ui| {
                ui.label(tr!(
                    self.i18n,
                    "Open your default email client to get help from the Damus team",
                    "Instruction to open email client"
                ));

                ui.horizontal_wrapped(|ui| {
                    ui.label(tr!(self.i18n, "Support email:", "Support email address",));
                    ui.label(RichText::new(SUPPORT_EMAIL).color(PINK))
                });

                let size = vec2(120.0, 40.0);
                ui.allocate_ui_with_layout(size, Layout::top_down(egui::Align::Center), |ui| {
                    let font_size =
                        notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body);
                    let button_resp = ui.add(open_email_button(self.i18n, font_size, size));
                    if button_resp.clicked() {
                        if let Err(e) = Uri::new(self.support.get_mailto_url()).open() {
                            error!(
                                "Failed to open URL {} because: {:?}",
                                self.support.get_mailto_url(),
                                e
                            );
                        };
                    };
                    button_resp.on_hover_text_at_pointer(self.support.get_mailto_url());
                })
            });

            ui.add_space(8.0);

            if let Some(logs) = self.support.get_most_recent_log() {
                ui.label(
                    RichText::new(tr!(
                        self.i18n,
                        "Step 2",
                        "Step 2 label in support instructions"
                    ))
                    .text_style(NotedeckTextStyle::Heading3.text_style()),
                );
                let size = vec2(80.0, 40.0);
                let copy_button = Button::new(
                    RichText::new(tr!(self.i18n, "Copy", "Button label to copy logs")).size(
                        notedeck::fonts::get_font_size(ui.ctx(), &NotedeckTextStyle::Body),
                    ),
                )
                .fill(PINK)
                .min_size(size);
                padding(8.0, ui, |ui| {
                    ui.add(Label::new(RichText::new(tr!(self.i18n,"Press the button below to copy your most recent logs to your system's clipboard. Then paste it into your email.", "Instruction for copying logs"))).wrap());
                    ui.allocate_ui_with_layout(size, Layout::top_down(egui::Align::Center), |ui| {
                        if ui.add(copy_button).clicked() {
                            ui.ctx().copy_text(logs.to_string());
                        }
                    });
                });
            } else {
                ui.label(
                    egui::RichText::new("ERROR: Could not find logs on system")
                        .color(egui::Color32::RED),
                );
            }
            ui.label(format!("Notedeck {}", env!("CARGO_PKG_VERSION")));
            ui.label(format!("Commit hash: {}", env!("GIT_COMMIT_HASH")));
        });
    }
}

fn open_email_button(
    i18n: &mut Localization,
    font_size: f32,
    size: egui::Vec2,
) -> impl egui::Widget {
    Button::new(
        RichText::new(tr!(i18n, "Open Email", "Button label to open email client")).size(font_size),
    )
    .fill(PINK)
    .min_size(size)
}
