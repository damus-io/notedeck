use egui::{vec2, Margin, RichText, Sense};
use notedeck::{fonts::get_font_size, NotedeckTextStyle, ReportType};
use notedeck_ui::galley_centered_pos;

pub struct ReportView<'a> {
    selected: &'a mut Option<ReportType>,
}

impl<'a> ReportView<'a> {
    pub fn new(selected: &'a mut Option<ReportType>) -> Self {
        Self { selected }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<ReportType> {
        let mut action = None;

        egui::Frame::new()
            .inner_margin(Margin::symmetric(48, 24))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 8.0);

                ui.add(egui::Label::new(
                    RichText::new("Report")
                        .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading3)),
                ));

                ui.add_space(4.0);

                for report_type in ReportType::ALL {
                    let is_selected = *self.selected == Some(*report_type);
                    if ui
                        .radio(is_selected, report_type.label())
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        *self.selected = Some(*report_type);
                    }
                }

                ui.add_space(8.0);

                let can_submit = self.selected.is_some();

                let resp = ui.allocate_response(vec2(ui.available_width(), 40.0), Sense::click());

                let fill = if !can_submit {
                    ui.visuals().widgets.inactive.bg_fill
                } else if resp.hovered() {
                    notedeck_ui::colors::PINK.gamma_multiply(0.8)
                } else {
                    notedeck_ui::colors::PINK
                };

                let painter = ui.painter_at(resp.rect);
                painter.rect_filled(resp.rect, egui::CornerRadius::same(20), fill);

                let galley = painter.layout_no_wrap(
                    "Submit Report".to_owned(),
                    NotedeckTextStyle::Body.get_font_id(ui.ctx()),
                    egui::Color32::WHITE,
                );

                painter.galley(
                    galley_centered_pos(&galley, resp.rect.center()),
                    galley,
                    egui::Color32::WHITE,
                );

                if can_submit && resp.clicked() {
                    action = *self.selected;
                }
            });

        action
    }
}
