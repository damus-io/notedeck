use crate::{route::Route, Damus};
use enostr::NoteId;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BarAction {
    Reply,
    OpenThread,
}

impl BarAction {
    pub fn execute(self, app: &mut Damus, timeline: usize, replying_to: &[u8; 32]) {
        match self {
            BarAction::Reply => {
                let timeline = &mut app.timelines[timeline];
                timeline
                    .routes
                    .push(Route::Reply(NoteId::new(replying_to.to_owned())));
                timeline.navigating = true;
            }

            BarAction::OpenThread => {
                let timeline = &mut app.timelines[timeline];
                timeline
                    .routes
                    .push(Route::Thread(NoteId::new(replying_to.to_owned())));
                timeline.navigating = true;
            }
        }
    }
}
