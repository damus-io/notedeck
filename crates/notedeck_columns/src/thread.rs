use crate::{multi_subscriber::MultiSubscriber, timeline::Timeline};

use nostrdb::FilterBuilder;
use notedeck::{RootNoteId, RootNoteIdBuf};

pub struct Thread {
    pub timeline: Timeline,
    pub subscription: Option<MultiSubscriber>,
}

impl Thread {
    pub fn new(root_id: RootNoteIdBuf) -> Self {
        let timeline = Timeline::thread(root_id);

        Thread {
            timeline,
            subscription: None,
        }
    }

    pub fn filters_raw(root_id: RootNoteId<'_>) -> Vec<FilterBuilder> {
        vec![
            nostrdb::Filter::new().kinds([1]).event(root_id.bytes()),
            nostrdb::Filter::new().ids([root_id.bytes()]).limit(1),
        ]
    }
}
