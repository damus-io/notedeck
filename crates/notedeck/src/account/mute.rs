use std::sync::Arc;

use enostr::RelayPool;
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use tracing::{debug, error};
use uuid::Uuid;

use crate::Muted;

pub(crate) struct AccountMutedData {
    pub filter: Filter,
    pub subid: Option<String>,
    pub sub: Option<Subscription>,
    pub muted: Arc<Muted>,
}

impl AccountMutedData {
    pub fn new(ndb: &Ndb, txn: &Transaction, pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-51 muted list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10000])
            .limit(1)
            .build();

        // Query the ndb immediately to see if the user's muted list is already there
        let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(txn, &[filter.clone()], lim)
            .expect("query user muted results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let muted = Self::harvest_nip51_muted(ndb, txn, &nks);
        debug!("pubkey {}: initial muted {:?}", hex::encode(pubkey), muted);

        AccountMutedData {
            filter,
            subid: None,
            sub: None,
            muted: Arc::new(muted),
        }
    }

    // make this account the current selected account
    pub fn activate(&mut self, ndb: &Ndb, pool: &mut RelayPool) {
        debug!("activating muted sub {}", self.filter.json().unwrap());
        assert_eq!(self.subid, None, "subid already exists");
        assert_eq!(self.sub, None, "sub already exists");

        // local subscription
        let sub = ndb
            .subscribe(&[self.filter.clone()])
            .expect("ndb muted subscription");

        // remote subscription
        let subid = Uuid::new_v4().to_string();
        pool.subscribe(subid.clone(), vec![self.filter.clone()]);

        self.sub = Some(sub);
        self.subid = Some(subid);
    }

    // this account is no longer the selected account
    pub fn deactivate(&mut self, ndb: &mut Ndb, pool: &mut RelayPool) {
        debug!("deactivating muted sub {}", self.filter.json().unwrap());
        assert_ne!(self.subid, None, "subid doesn't exist");
        assert_ne!(self.sub, None, "sub doesn't exist");

        // remote subscription
        pool.unsubscribe(self.subid.as_ref().unwrap().clone());

        // local subscription
        ndb.unsubscribe(self.sub.unwrap())
            .expect("ndb muted unsubscribe");

        self.sub = None;
        self.subid = None;
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
}
