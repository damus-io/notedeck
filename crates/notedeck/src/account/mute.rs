use std::sync::Arc;

use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use tracing::{debug, error};

use crate::{filter::NamedFilter, Muted};

pub(crate) struct AccountMutedData {
    pub filter: NamedFilter,
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

        AccountMutedData {
            filter: NamedFilter::new("account-mutelist", vec![filter]),
            muted: Arc::new(Muted::default()),
        }
    }

    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        // Query the ndb immediately to see if the user's muted list is already there
        let lim = self.filter.filter[0]
            .limit()
            .unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(txn, &self.filter.filter, lim)
            .expect("query user muted results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let muted = Self::harvest_nip51_muted(ndb, txn, &nks);
        debug!("initial muted {:?}", muted);

        self.muted = Arc::new(muted);
    }

    pub(crate) fn harvest_nip51_muted(ndb: &Ndb, txn: &Transaction, nks: &[NoteKey]) -> Muted {
        let mut muted = Muted::default();
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

    pub(super) fn poll_for_updates(&mut self, ndb: &Ndb, txn: &Transaction, sub: Subscription) {
        let nks = ndb.poll_for_notes(sub, 1);

        if nks.is_empty() {
            return;
        }

        let muted = AccountMutedData::harvest_nip51_muted(ndb, txn, &nks);
        debug!("updated muted {:?}", muted);
        self.muted = Arc::new(muted);
    }
}
