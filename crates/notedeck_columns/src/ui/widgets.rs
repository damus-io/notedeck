use egui::{Button, Widget};
use notedeck::NotedeckTextStyle;

/// Sized and styled to match the figma design
pub fn styled_button(text: &str, fill_color: egui::Color32) -> impl Widget + '_ {
    styled_button_toggleable(text, fill_color, true)
}

pub fn styled_button_toggleable(
    text: &str,
    fill_color: egui::Color32,
    enabled: bool,
) -> impl Widget + '_ {
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

        let size = galley.rect.expand2(egui::vec2(16.0, 8.0)).size();
        let mut button = Button::new(galley).corner_radius(8.0);

        if !enabled {
            button = button
                .sense(egui::Sense::focusable_noninteractive())
                .fill(ui.visuals().noninteractive().bg_fill)
                .stroke(ui.visuals().noninteractive().bg_stroke);
        } else {
            button = button.fill(fill_color);
        }

        let mut resp = ui.add_sized(size, button);

        if !enabled {
            resp = resp.on_hover_cursor(egui::CursorIcon::NotAllowed);
        }

        resp
    }
}
