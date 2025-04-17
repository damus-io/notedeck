use egui::{Button, Widget};
use notedeck::NotedeckTextStyle;

/// Sized and styled to match the figma design
pub fn styled_button(text: &str, fill_color: egui::Color32) -> impl Widget + '_ {
    move |ui: &mut egui::Ui| -> egui::Response {
        let painter = ui.painter();
        let text_color = if ui.visuals().dark_mode {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };

        let galley = painter.layout(
            text.to_owned(),
            NotedeckTextStyle::Body.get_font_id(ui.ctx()),
            text_color,
            ui.available_width(),
        );

        ui.add_sized(
            galley.rect.expand2(egui::vec2(16.0, 8.0)).size(),
            Button::new(galley).corner_radius(8.0).fill(fill_color),
        )
    }
}
