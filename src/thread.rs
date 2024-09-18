use crate::{
    multi_subscriber::MultiSubscriber,
    note::NoteRef,
    notecache::NoteCache,
    timeline::{TimelineTab, ViewFilter},
    Error, Result,
};
use enostr::RelayPool;
use nostrdb::{Filter, FilterBuilder, Ndb, Transaction};
use std::collections::HashMap;
use tracing::{debug, warn};

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

    #[must_use = "UnknownIds::update_from_note_refs should be used on this result"]
    pub fn poll_notes_into_view(&mut self, txn: &Transaction, ndb: &Ndb) -> Result<()> {
        if let Some(multi_subscriber) = &mut self.multi_subscriber {
            let reversed = true;
            let note_refs: Vec<NoteRef> = multi_subscriber.poll_for_notes(ndb, txn)?;
            self.view.insert(&note_refs, reversed);
        } else {
            return Err(Error::Generic(
                "Thread unexpectedly has no MultiSubscriber".to_owned(),
            ));
        }

        Ok(())
    }

    /// Look for new thread notes since our last fetch
    pub fn new_notes(
        notes: &[NoteRef],
        root_id: &[u8; 32],
        txn: &Transaction,
        ndb: &Ndb,
    ) -> Vec<NoteRef> {
        if notes.is_empty() {
            return vec![];
        }

        let last_note = notes[0];
        let filters = Thread::filters_since(root_id, last_note.created_at + 1);

        if let Ok(results) = ndb.query(txn, &filters, 1000) {
            debug!("got {} results from thread update", results.len());
            results
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect()
        } else {
            debug!("got no results from thread update",);
            vec![]
        }
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

#[derive(Default)]
pub struct Threads {
    /// root id to thread
    pub root_id_to_thread: HashMap<[u8; 32], Thread>,
}

pub enum ThreadResult<'a> {
    Fresh(&'a mut Thread),
    Stale(&'a mut Thread),
}

impl<'a> ThreadResult<'a> {
    pub fn get_ptr(self) -> &'a mut Thread {
        match self {
            Self::Fresh(ptr) => ptr,
            Self::Stale(ptr) => ptr,
        }
    }

    pub fn is_stale(&self) -> bool {
        match self {
            Self::Fresh(_ptr) => false,
            Self::Stale(_ptr) => true,
        }
    }
}

impl Threads {
    pub fn thread_expected_mut(&mut self, root_id: &[u8; 32]) -> &mut Thread {
        self.root_id_to_thread
            .get_mut(root_id)
            .expect("thread_expected_mut used but there was no thread")
    }

    pub fn thread_mut<'a>(
        &'a mut self,
        ndb: &Ndb,
        txn: &Transaction,
        root_id: &[u8; 32],
    ) -> ThreadResult<'a> {
        // we can't use the naive hashmap entry API here because lookups
        // require a copy, wait until we have a raw entry api. We could
        // also use hashbrown?

        if self.root_id_to_thread.contains_key(root_id) {
            return ThreadResult::Stale(self.thread_expected_mut(root_id));
        }

        // we don't have the thread, query for it!
        let filters = Thread::filters(root_id);

        let notes = if let Ok(results) = ndb.query(txn, &filters, 1000) {
            results
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect()
        } else {
            debug!(
                "got no results from thread lookup for {}",
                hex::encode(root_id)
            );
            vec![]
        };

        if notes.is_empty() {
            warn!("thread query returned 0 notes? ")
        } else {
            debug!("found thread with {} notes", notes.len());
        }

        self.root_id_to_thread
            .insert(root_id.to_owned(), Thread::new(notes));
        ThreadResult::Fresh(self.root_id_to_thread.get_mut(root_id).unwrap())
    }

    //fn thread_by_id(&self, ndb: &Ndb, id: &[u8; 32]) -> &mut Thread {
    //}
}

/// Local thread unsubscribe
pub fn thread_unsubscribe(
    ndb: &Ndb,
    threads: &mut Threads,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    id: &[u8; 32],
) {
    let txn = Transaction::new(ndb).expect("txn");
    let root_id = crate::note::root_note_id_from_selected_id(ndb, note_cache, &txn, id);

    let thread = threads.thread_mut(ndb, &txn, root_id).get_ptr();

    if let Some(multi_subscriber) = &mut thread.multi_subscriber {
        multi_subscriber.unsubscribe(ndb, pool);
    }
}
