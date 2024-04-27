use crate::{colors, ui, Damus};
use nostrdb::Transaction;

pub struct Mention<'a> {
    app: &'a mut Damus,
    txn: &'a Transaction,
    pk: &'a [u8; 32],
    size: f32,
}

impl<'a> Mention<'a> {
    pub fn new(app: &'a mut Damus, txn: &'a Transaction, pk: &'a [u8; 32]) -> Self {
        let size = 16.0;
        Mention { app, txn, pk, size }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl<'a> egui::Widget for Mention<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        mention_ui(self.app, self.txn, self.pk, ui, self.size)
    }
}

fn mention_ui(
    app: &mut Damus,
    txn: &Transaction,
    pk: &[u8; 32],
    ui: &mut egui::Ui,
    size: f32,
) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        let profile = app.ndb.get_profile_by_pubkey(txn, pk).ok();

        let name: String =
            if let Some(name) = profile.as_ref().and_then(crate::profile::get_profile_name) {
                format!("@{}", name.username())
            } else {
                "??".to_string()
            };

        let resp = ui.add(egui::Label::new(
            egui::RichText::new(name).color(colors::PURPLE).size(size),
        ));

        if let Some(rec) = profile.as_ref() {
            resp.on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(300.0);
                ui.add(ui::ProfilePreview::new(rec, &mut app.img_cache));
            });
        }
    })
    .response
}
