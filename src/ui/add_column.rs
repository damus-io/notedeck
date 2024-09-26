use egui::{RichText, Ui};
use nostrdb::Ndb;

use crate::{
    app_style::NotedeckTextStyle,
    timeline::{Timeline, TimelineKind},
    user_account::UserAccount,
};

pub enum AddColumnResponse {
    Timeline(Timeline),
}

pub struct AddColumnView<'a> {
    ndb: &'a Ndb,
    cur_account: Option<&'a UserAccount>,
}

impl<'a> AddColumnView<'a> {
    pub fn new(ndb: &'a Ndb, cur_account: Option<&'a UserAccount>) -> Self {
        Self { ndb, cur_account }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        ui.label(RichText::new("Add column").text_style(NotedeckTextStyle::Heading.text_style()));

        if ui.button("create global timeline").clicked() {
            Some(AddColumnResponse::Timeline(
                TimelineKind::Universe
                    .into_timeline(self.ndb, None)
                    .expect("universe timeline"),
            ))
        } else {
            None
        }
    }
}

// struct ColumnOption {
//     title: &'static str,
//     description: &'static str,
//     icon: Box::<dyn Widget>,
//     route: Route,
// }
