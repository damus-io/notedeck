use crate::{colors, imgcache::ImageCache, ui};
use nostrdb::{Ndb, Transaction};

pub struct Mention<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
    txn: &'a Transaction,
    pk: &'a [u8; 32],
    selectable: bool,
    size: f32,
}

impl<'a> Mention<'a> {
    pub fn new(
        ndb: &'a Ndb,
        img_cache: &'a mut ImageCache,
        txn: &'a Transaction,
        pk: &'a [u8; 32],
    ) -> Self {
        let size = 16.0;
        let selectable = true;
        Mention {
            ndb,
            img_cache,
            txn,
            pk,
            selectable,
            size,
        }
    }

    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl<'a> egui::Widget for Mention<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        mention_ui(
            self.ndb,
            self.img_cache,
            self.txn,
            self.pk,
            ui,
            self.size,
            self.selectable,
        )
    }
}

fn mention_ui(
    ndb: &Ndb,
    img_cache: &mut ImageCache,
    txn: &Transaction,
    pk: &[u8; 32],
    ui: &mut egui::Ui,
    size: f32,
    selectable: bool,
) -> egui::Response {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    ui.horizontal(|ui| {
        let profile = ndb.get_profile_by_pubkey(txn, pk).ok();

        let name: String =
            if let Some(name) = profile.as_ref().and_then(crate::profile::get_profile_name) {
                format!("@{}", name.username())
            } else {
                "??".to_string()
            };

        let resp = ui.add(
            egui::Label::new(egui::RichText::new(name).color(colors::PURPLE).size(size))
                .selectable(selectable),
        );

        if let Some(rec) = profile.as_ref() {
            resp.on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(300.0);
                ui.add(ui::ProfilePreview::new(rec, img_cache));
            });
        }
    })
    .response
}
