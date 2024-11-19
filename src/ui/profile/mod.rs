pub mod picture;
pub mod preview;

use crate::ui::note::NoteOptions;
use egui::{ScrollArea, Widget};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
pub use picture::ProfilePic;
pub use preview::ProfilePreview;

use crate::{
    actionbar::NoteAction, imgcache::ImageCache, notecache::NoteCache,
    notes_holder::NotesHolderStorage, profile::Profile,
};

use super::timeline::{tabs_ui, TimelineTabView};

pub struct ProfileView<'a> {
    pubkey: &'a Pubkey,
    col_id: usize,
    profiles: &'a mut NotesHolderStorage<Profile>,
    note_options: NoteOptions,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
}

impl<'a> ProfileView<'a> {
    pub fn new(
        pubkey: &'a Pubkey,
        col_id: usize,
        profiles: &'a mut NotesHolderStorage<Profile>,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        note_options: NoteOptions,
    ) -> Self {
        ProfileView {
            pubkey,
            col_id,
            profiles,
            ndb,
            note_cache,
            img_cache,
            note_options,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let scroll_id = egui::Id::new(("profile_scroll", self.col_id, self.pubkey));

        ScrollArea::vertical()
            .id_source(scroll_id)
            .show(ui, |ui| {
                let txn = Transaction::new(self.ndb).expect("txn");
                if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, self.pubkey.bytes()) {
                    ProfilePreview::new(&profile, self.img_cache).ui(ui);
                }
                let profile = self
                    .profiles
                    .notes_holder_mutated(self.ndb, self.note_cache, &txn, self.pubkey.bytes())
                    .get_ptr();

                profile.timeline.selected_view = tabs_ui(ui);

                let reversed = false;

                TimelineTabView::new(
                    profile.timeline.current_view(),
                    reversed,
                    self.note_options,
                    &txn,
                    self.ndb,
                    self.note_cache,
                    self.img_cache,
                )
                .show(ui)
            })
            .inner
    }
}
