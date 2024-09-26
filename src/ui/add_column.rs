use egui::{RichText, Ui};
use nostrdb::FilterBuilder;

use crate::{app_style::NotedeckTextStyle, timeline::Timeline, user_account::UserAccount};

pub enum AddColumnResponse {
    Timeline(Timeline),
}

pub struct AddColumnView<'a> {
    cur_account: Option<&'a UserAccount>,
}

impl<'a> AddColumnView<'a> {
    pub fn new(cur_account: Option<&'a UserAccount>) -> Self {
        Self { cur_account }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<AddColumnResponse> {
        ui.label(RichText::new("Add column").text_style(NotedeckTextStyle::Heading.text_style()));

        if ui.button("create global timeline").clicked() {
            Some(AddColumnResponse::Timeline(create_global_timeline()))
        } else {
            None
        }
    }
}

fn create_global_timeline() -> Timeline {
    let filter = FilterBuilder::new().kinds([1]).build();
    Timeline::new(
        crate::timeline::TimelineKind::Generic,
        crate::filter::FilterState::Ready(vec![filter]),
    )
}

// struct ColumnOption {
//     title: &'static str,
//     description: &'static str,
//     icon: Box::<dyn Widget>,
//     route: Route,
// }
