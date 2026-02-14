/// Pill-style UI components for displaying labeled values.
///
/// Pills are compact, rounded UI elements used to display key-value pairs
/// in a visually distinct way, commonly used in query displays.
use egui::Ui;

/// Render a pill label with a text value
pub fn pill_label(name: &str, value: &str, ui: &mut Ui) {
    pill_label_ui(
        name,
        move |ui| {
            ui.label(value);
        },
        ui,
    );
}

/// Render a pill label with a custom UI closure for the value
pub fn pill_label_ui(name: &str, mut value: impl FnMut(&mut Ui), ui: &mut Ui) {
    egui::Frame::new()
        .fill(ui.visuals().noninteractive().bg_fill)
        .inner_margin(egui::Margin::same(4))
        .corner_radius(egui::CornerRadius::same(10))
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().noninteractive().bg_stroke.color,
        ))
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(ui.visuals().noninteractive().weak_bg_fill)
                .inner_margin(egui::Margin::same(4))
                .corner_radius(egui::CornerRadius::same(10))
                .stroke(egui::Stroke::new(
                    1.0,
                    ui.visuals().noninteractive().bg_stroke.color,
                ))
                .show(ui, |ui| {
                    ui.label(name);
                });

            value(ui);
        });
}
