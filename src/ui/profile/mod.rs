pub mod picture;
pub mod preview;

use egui::{Label, RichText};
use nostrdb::Ndb;
pub use picture::ProfilePic;
pub use preview::ProfilePreview;
use tracing::info;

use crate::{
    actionbar::TimelineResponse, column::Columns, imgcache::ImageCache, notecache::NoteCache,
    timeline::TimelineId,
};

use super::TimelineView;

pub struct ProfileView<'a> {
    timeline_id: TimelineId,
    columns: &'a mut Columns,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
}

impl<'a> ProfileView<'a> {
    pub fn new(
        timeline_id: TimelineId,
        columns: &'a mut Columns,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
    ) -> Self {
        ProfileView {
            timeline_id,
            columns,
            ndb,
            note_cache,
            img_cache,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> TimelineResponse {
        ui.add(Label::new(
            RichText::new("PROFILE VIEW").text_style(egui::TextStyle::Heading),
        ));

        TimelineView::new(
            self.timeline_id,
            self.columns,
            self.ndb,
            self.note_cache,
            self.img_cache,
            false,
        )
        .ui(ui)
    }
}
