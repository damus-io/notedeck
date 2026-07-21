use std::sync::Arc;

use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use tracing::debug;

use crate::{bookmarks::bookmarks_from_note, Bookmarks};

#[derive(Clone)]
pub(crate) struct AccountBookmarksData {
    pub filter: Filter,
    pub bookmarks: Arc<Bookmarks>,
}

impl AccountBookmarksData {
    pub fn new(pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-51 bookmarks list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10003])
            .limit(1)
            .build();

        let bookmarks = Arc::new(Bookmarks::default());

        AccountBookmarksData { filter, bookmarks }
    }

    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        // Query the ndb immediately to see if the user's bookmarks list is already there
        let lim = self
            .filter
            .limit()
            .unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(txn, std::slice::from_ref(&self.filter), lim)
            .expect("query user bookmarks results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let bookmarks = Self::harvest_nip51_bookmarks(ndb, txn, &nks);
        debug!("initial bookmarks {:?}", bookmarks);

        self.bookmarks = Arc::new(bookmarks);
    }

    pub(crate) fn harvest_nip51_bookmarks(
        ndb: &Ndb,
        txn: &Transaction,
        nks: &[NoteKey],
    ) -> Bookmarks {
        // NIP-51 kind-10003 is a single replaceable event, so at most one of
        // these note keys is actually current; take its tags as-is.
        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                return bookmarks_from_note(&note);
            }
        }

        Bookmarks::default()
    }

    #[profiling::function]
    pub(super) fn poll_for_updates(&mut self, ndb: &Ndb, txn: &Transaction, sub: Subscription) {
        let nks = ndb.poll_for_notes(sub, 1);

        if nks.is_empty() {
            return;
        }

        let bookmarks = Self::harvest_nip51_bookmarks(ndb, txn, &nks);
        debug!("updated bookmarks {:?}", bookmarks);
        self.bookmarks = Arc::new(bookmarks);
    }
}
