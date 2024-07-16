use crate::note::NoteRef;
use crate::timeline::{TimelineView, ViewFilter};
use nostrdb::{Ndb, Transaction};
use std::collections::HashMap;
use tracing::debug;

#[derive(Default)]
pub struct Thread {
    pub view: TimelineView,
}

impl Thread {
    pub fn new(notes: Vec<NoteRef>) -> Self {
        let mut cap = ((notes.len() as f32) * 1.5) as usize;
        if cap == 0 {
            cap = 25;
        }
        let mut view = TimelineView::new_with_capacity(ViewFilter::NotesAndReplies, cap);
        view.notes = notes;

        Thread { view }
    }
}

#[derive(Default)]
pub struct Threads {
    threads: HashMap<[u8; 32], Thread>,
}

impl Threads {
    pub fn thread_mut(&mut self, ndb: &Ndb, txn: &Transaction, root_id: &[u8; 32]) -> &mut Thread {
        // we can't use the naive hashmap entry API here because lookups
        // require a copy, wait until we have a raw entry api. We could
        // also use hashbrown?

        if self.threads.contains_key(root_id) {
            return self.threads.get_mut(root_id).unwrap();
        }

        // looks like we don't have this thread yet, populate it
        // TODO: should we do this in the caller?
        let root = if let Ok(root) = ndb.get_note_by_id(txn, root_id) {
            root
        } else {
            debug!("couldnt find root note for id {}", hex::encode(root_id));
            self.threads.insert(root_id.to_owned(), Thread::new(vec![]));
            return self.threads.get_mut(root_id).unwrap();
        };

        // we don't have the thread, query for it!
        let filter = vec![
            nostrdb::Filter::new()
                .kinds(vec![1])
                .event(root.id())
                .build(),
            nostrdb::Filter::new()
                .kinds(vec![1])
                .ids(vec![*root.id()])
                .build(),
        ];

        // TODO: what should be the max results ?
        let notes = if let Ok(mut results) = ndb.query(txn, filter, 10000) {
            results.reverse();
            results
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect()
        } else {
            debug!(
                "got no results from thread lookup for {}",
                hex::encode(root.id())
            );
            vec![]
        };

        debug!("found thread with {} notes", notes.len());
        self.threads.insert(root_id.to_owned(), Thread::new(notes));
        self.threads.get_mut(root_id).unwrap()
    }

    //fn thread_by_id(&self, ndb: &Ndb, id: &[u8; 32]) -> &mut Thread {
    //}
}
