use std::sync::Arc;

use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use tracing::{debug, error};

use crate::Muted;

#[derive(Clone)]
pub(crate) struct AccountMutedData {
    pub filter: Filter,
    pub muted: Arc<Muted>,
}

impl AccountMutedData {
    pub fn new(pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-51 muted list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10000])
            .limit(1)
            .build();

        let muted = Arc::new(Muted::default());

        AccountMutedData { filter, muted }
    }

    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        // Query the ndb immediately to see if the user's muted list is already there
        let lim = self
            .filter
            .limit()
            .unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(txn, std::slice::from_ref(&self.filter), lim)
            .expect("query user muted results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let max_hashtags = self.muted.max_hashtags_per_note;
        let muted = Self::harvest_nip51_muted(ndb, txn, &nks, max_hashtags);
        debug!("initial muted {:?}", muted);

        self.muted = Arc::new(muted);
    }

    pub(crate) fn harvest_nip51_muted(
        ndb: &Ndb,
        txn: &Transaction,
        nks: &[NoteKey],
        max_hashtags_per_note: usize,
    ) -> Muted {
        let mut muted = Muted {
            max_hashtags_per_note,
            ..Default::default()
        };

        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                for tag in note.tags() {
                    match tag.get(0).and_then(|t| t.variant().str()) {
                        Some("p") => {
                            if let Some(id) = tag.get(1).and_then(|f| f.variant().id()) {
                                muted.pubkeys.insert(*id);
                            }
                        }
                        Some("t") => {
                            if let Some(str) = tag.get(1).and_then(|f| f.variant().str()) {
                                muted.hashtags.insert(str.to_string());
                            }
                        }
                        Some("word") => {
                            if let Some(str) = tag.get(1).and_then(|f| f.variant().str()) {
                                muted.words.insert(str.to_string());
                            }
                        }
                        Some("e") => {
                            if let Some(id) = tag.get(1).and_then(|f| f.variant().id()) {
                                muted.threads.insert(*id);
                            }
                        }
                        Some("alt") => {
                            // maybe we can ignore these?
                        }
                        Some(x) => error!("query_nip51_muted: unexpected tag: {}", x),
                        None => error!(
                            "query_nip51_muted: bad tag value: {:?}",
                            tag.get_unchecked(0).variant()
                        ),
                    }
                }
            }
        }
        muted
    }

    #[profiling::function]
    pub(super) fn poll_for_updates(&mut self, ndb: &Ndb, txn: &Transaction, sub: Subscription) {
        let nks = ndb.poll_for_notes(sub, 1);

        if nks.is_empty() {
            return;
        }

        let max_hashtags = self.muted.max_hashtags_per_note;
        let muted = AccountMutedData::harvest_nip51_muted(ndb, txn, &nks, max_hashtags);
        debug!("updated muted {:?}", muted);
        self.muted = Arc::new(muted);
    }

    /// Update the max hashtags per note setting
    pub fn update_max_hashtags(&mut self, max_hashtags_per_note: usize) {
        let mut muted = (*self.muted).clone();
        muted.max_hashtags_per_note = max_hashtags_per_note;
        self.muted = Arc::new(muted);
    }
}
