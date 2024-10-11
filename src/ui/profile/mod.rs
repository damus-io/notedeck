pub mod picture;
pub mod preview;

use egui::{ScrollArea, Widget};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
pub use picture::ProfilePic;
pub use preview::ProfilePreview;

use crate::{
    actionbar::TimelineResponse, column::Columns, imgcache::ImageCache, notecache::NoteCache,
    timeline::TimelineId,
};

use super::TimelineView;

pub struct ProfileView<'a> {
    pubkey: Pubkey,
    timeline_id: TimelineId,
    columns: &'a mut Columns,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
}

impl<'a> ProfileView<'a> {
    pub fn new(
        pubkey: Pubkey,
        timeline_id: TimelineId,
        columns: &'a mut Columns,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
    ) -> Self {
        ProfileView {
            pubkey,
            timeline_id,
            columns,
            ndb,
            note_cache,
            img_cache,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> TimelineResponse {
        let scroll_id = egui::Id::new(("profile_scroll", self.timeline_id, self.pubkey));

        ScrollArea::vertical()
            .id_source(scroll_id)
            .show(ui, |ui| {
                {
                    let txn = Transaction::new(self.ndb).expect("txn");
                    if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, self.pubkey.bytes()) {
                        ProfilePreview::new(&profile, self.img_cache).ui(ui);
                    }
                }

                TimelineView::new(
                    self.timeline_id,
                    self.columns,
                    self.ndb,
                    self.note_cache,
                    self.img_cache,
                    false,
                )
                .ui_no_scroll(ui)
            })
            .inner
    }
}
