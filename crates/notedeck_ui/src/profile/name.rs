use egui::RichText;
use notedeck::{NostrName, NotedeckTextStyle};

pub fn one_line_display_name_widget<'a>(
    visuals: &egui::Visuals,
    display_name: NostrName<'a>,
    style: NotedeckTextStyle,
) -> impl egui::Widget + 'a {
    let text_style = style.text_style();
    let color = visuals.noninteractive().fg_stroke.color;

    move |ui: &mut egui::Ui| -> egui::Response {
        ui.label(
            RichText::new(display_name.name())
                .text_style(text_style)
                .color(color),
        )
    }
}
