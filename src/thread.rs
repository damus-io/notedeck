use crate::{
    multi_subscriber::MultiSubscriber,
    note::NoteRef,
    notes_holder::NotesHolder,
    timeline::{TimelineTab, ViewFilter},
};
use nostrdb::{Filter, FilterBuilder};

#[derive(Default)]
pub struct Thread {
    view: TimelineTab,
    pub multi_subscriber: Option<MultiSubscriber>,
}

impl Thread {
    pub fn new(notes: Vec<NoteRef>) -> Self {
        let mut cap = ((notes.len() as f32) * 1.5) as usize;
        if cap == 0 {
            cap = 25;
        }
        let mut view = TimelineTab::new_with_capacity(ViewFilter::NotesAndReplies, cap);
        view.notes = notes;

        Thread {
            view,
            multi_subscriber: None,
        }
    }

    pub fn view(&self) -> &TimelineTab {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut TimelineTab {
        &mut self.view
    }

    fn filters_raw(root: &[u8; 32]) -> Vec<FilterBuilder> {
        vec![
            nostrdb::Filter::new().kinds([1]).event(root),
            nostrdb::Filter::new().ids([root]).limit(1),
        ]
    }

    pub fn filters_since(root: &[u8; 32], since: u64) -> Vec<Filter> {
        Self::filters_raw(root)
            .into_iter()
            .map(|fb| fb.since(since).build())
            .collect()
    }

    pub fn filters(root: &[u8; 32]) -> Vec<Filter> {
        Self::filters_raw(root)
            .into_iter()
            .map(|mut fb| fb.build())
            .collect()
    }
}

impl NotesHolder for Thread {
    fn get_multi_subscriber(&mut self) -> Option<&mut MultiSubscriber> {
        self.multi_subscriber.as_mut()
    }

    fn filters(for_id: &[u8; 32]) -> Vec<Filter> {
        Thread::filters(for_id)
    }

    fn new_notes_holder(_: &[u8; 32], _: Vec<Filter>, notes: Vec<NoteRef>) -> Self {
        Thread::new(notes)
    }

    fn get_view(&mut self) -> &mut TimelineTab {
        &mut self.view
    }

    fn filters_since(for_id: &[u8; 32], since: u64) -> Vec<Filter> {
        Thread::filters_since(for_id, since)
    }

    fn set_multi_subscriber(&mut self, subscriber: MultiSubscriber) {
        self.multi_subscriber = Some(subscriber);
    }
}
