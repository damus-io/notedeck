use egui::{Ui, WidgetText};

/// Placeholder for platforms where ffmpeg-based playback is unavailable.
pub fn show_video_embeds(ui: &mut Ui, urls: &[String]) {
    for url in urls {
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.label("Video playback is not available on this platform.");
            ui.hyperlink_to(WidgetText::from(url), url);
        });
    }
}
