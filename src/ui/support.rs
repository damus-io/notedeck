use crate::support::Support;

use super::padding;

pub struct SupportView<'a> {
    support: &'a mut Support,
}

impl<'a> SupportView<'a> {
    pub fn new(support: &'a mut Support) -> Self {
        Self { support }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        padding(8.0, ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Run into a bug? ".to_owned());
                ui.hyperlink_to("click here", self.support.get_mailto_url());
                ui.label("  to open your default email client and send an email to Damus Support with your logs attached.".to_owned());
            });

            if let Some(dir) = self.support.get_log_dir() {
                ui.add_space(16.0);
                ui.label("alternatively, you can manually email support@damus.io with your logs attached which can be found in:".to_owned());
                ui.label(dir);
            }
        });
    }
}
