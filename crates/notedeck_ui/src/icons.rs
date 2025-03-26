use egui::{vec2, Color32, Stroke};

/// Creates a magnifying glass icon widget
pub fn search_icon(size: f32, height: f32) -> impl egui::Widget {
    move |ui: &mut egui::Ui| {
        // Use the provided height parameter
        let desired_size = vec2(size, height);
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());

        // Calculate center position - this ensures the icon is centered in its allocated space
        let center_pos = rect.center();
        let stroke = Stroke::new(1.5, Color32::from_rgb(150, 150, 150));

        // Draw circle
        let circle_radius = size * 0.35;
        ui.painter()
            .circle(center_pos, circle_radius, Color32::TRANSPARENT, stroke);

        // Draw handle
        let handle_start = center_pos + vec2(circle_radius * 0.7, circle_radius * 0.7);
        let handle_end = handle_start + vec2(size * 0.25, size * 0.25);
        ui.painter()
            .line_segment([handle_start, handle_end], stroke);

        response
    }
}
