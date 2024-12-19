pub mod picture;
pub mod preview;

use crate::notes_holder::NotesHolder;
use crate::ui::note::NoteOptions;
use egui::{ScrollArea, Widget};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
pub use picture::ProfilePic;
pub use preview::ProfilePreview;
use tracing::error;

use crate::{actionbar::NoteAction, notes_holder::NotesHolderStorage, profile::Profile};

use super::timeline::{tabs_ui, TimelineTabView};
use notedeck::{ImageCache, MuteFun, NoteCache};

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

    pub fn ui(&mut self, ui: &mut egui::Ui, is_muted: &MuteFun) -> Option<NoteAction> {
        let scroll_id = egui::Id::new(("profile_scroll", self.col_id, self.pubkey));

        ScrollArea::vertical()
            .id_salt(scroll_id)
            .show(ui, |ui| {
                let txn = Transaction::new(self.ndb).expect("txn");
                if let Ok(profile) = self.ndb.get_profile_by_pubkey(&txn, self.pubkey.bytes()) {
                    ProfilePreview::new(&profile, self.img_cache).ui(ui);
                }
                let profile = self
                    .profiles
                    .notes_holder_mutated(
                        self.ndb,
                        self.note_cache,
                        &txn,
                        self.pubkey.bytes(),
                        is_muted,
                    )
                    .get_ptr();

                profile.timeline.selected_view =
                    tabs_ui(ui, profile.timeline.selected_view, &profile.timeline.views);

                // poll for new notes and insert them into our existing notes
                if let Err(e) =
                    profile.poll_notes_into_view(&txn, self.ndb, self.note_cache, is_muted)
                {
                    error!("Profile::poll_notes_into_view: {e}");
                }

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
                .show(ui, is_muted)
            })
            .inner
    }
}
