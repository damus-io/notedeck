use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::Poll;
use tokio::task;

use tracing::{debug, error};

use enostr::{Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use uuid::Uuid;

use crate::muted::Muted;
use crate::note::NoteRef;
use crate::Damus;

pub fn spawn_track_user_relays(damus: &mut Damus) {
    // This is only safe because we are absolutely single threaded ...
    let damus_ptr = &mut *damus as *mut Damus;
    spawn_sendable(async move {
        let damus = unsafe { &mut *damus_ptr };
        track_user_relays(damus).await;
    });
}

pub async fn track_user_relays(damus: &mut Damus) {
    debug!("track_user_relays starting");

    let filter = user_relay_filter(damus);

    // Do we have a user relay list stored in nostrdb? Start with that ...
    let txn = Transaction::new(&damus.ndb).expect("transaction");
    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
    let nks = damus
        .ndb
        .query(&txn, &[filter.clone()], lim)
        .expect("query user relays results")
        .iter()
        .map(|qr| qr.note_key)
        .collect();
    let relays = handle_nip65_relays(&damus.ndb, &txn, &nks);
    debug!("track_user_relays: initial: {:#?}", relays);
    set_advertised_relays(&mut damus.pool, relays);
    drop(txn);

    // Subscribe to user relay list updates
    let ndbsub = damus
        .ndb
        .subscribe(&[filter.clone()])
        .expect("ndb subscription");
    let poolid = Uuid::new_v4().to_string();
    damus.pool.subscribe(poolid.clone(), vec![filter.clone()]);

    // Wait for updates to the subscription
    loop {
        match damus.ndb.wait_for_notes(ndbsub, 10).await {
            Ok(nks) => {
                let txn = Transaction::new(&damus.ndb).expect("transaction");
                let relays = handle_nip65_relays(&damus.ndb, &txn, &nks);
                debug!("track_user_relays: update: {:#?}", relays);
                set_advertised_relays(&mut damus.pool, relays);
            }
            Err(err) => error!("err: {:?}", err),
        }
    }
}

fn user_relay_filter(damus: &mut Damus) -> Filter {
    let account = damus
        .accounts
        .get_selected_account()
        .as_ref()
        .map(|a| a.pubkey.bytes())
        .expect("selected account");

    // NIP-65
    Filter::new()
        .authors([account])
        .kinds([10002])
        .limit(1)
        .build()
}

// useful for debugging
fn _query_note_json(ndb: &Ndb, txn: &Transaction, filter: &Filter) -> Vec<String> {
    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
    let results = ndb
        .query(txn, &[filter.clone()], lim)
        .expect("query results");
    results
        .iter()
        .map(|qr| NoteRef::new(qr.note_key, qr.note.created_at()))
        .filter_map(|nr| ndb.get_note_by_key(txn, nr.key).ok())
        .map(|n| n.json().unwrap())
        .collect()
}

fn handle_nip65_relays(ndb: &Ndb, txn: &Transaction, nks: &Vec<NoteKey>) -> Vec<String> {
    nks.iter()
        .filter_map(|nk| ndb.get_note_by_key(txn, *nk).ok())
        .flat_map(|n| {
            n.tags()
                .iter()
                .filter_map(|ti| ti.get_unchecked(1).variant().str())
                .map(|s| s.to_string())
        })
        .collect()
}

fn set_advertised_relays(pool: &mut RelayPool, relays: Vec<String>) {
    let wakeup = move || {
        // FIXME - how do we repaint?
    };
    pool.advertised_relays = relays
        .into_iter()
        .map(|s| RelayPool::canonicalize_url(&s))
        .collect();
    if let Err(e) = pool.configure_relays(wakeup) {
        error!("{:?}", e)
    }
}

pub fn spawn_track_user_muted(damus: &mut Damus) {
    // This is only safe because we are absolutely single threaded ...
    let damus_ptr = &mut *damus as *mut Damus;
    spawn_sendable(async move {
        let damus = unsafe { &mut *damus_ptr };
        track_user_muted(damus).await;
    });
}

pub async fn track_user_muted(damus: &mut Damus) {
    debug!("track_user_muted starting");

    let filter = user_muted_filter(damus);

    // Do we have a user muted list stored in nostrdb? Start with that ...
    let txn = Transaction::new(&damus.ndb).expect("transaction");
    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
    let nks = damus
        .ndb
        .query(&txn, &[filter.clone()], lim)
        .expect("query user muted results")
        .iter()
        .map(|qr| qr.note_key)
        .collect();
    let muted = handle_nip51_muted(&damus.ndb, &txn, &nks);
    debug!("track_user_muted: initial: {:#?}", muted);
    damus.muted = muted;
    drop(txn);

    // Subscribe to user relay list updates
    let ndbsub = damus
        .ndb
        .subscribe(&[filter.clone()])
        .expect("ndb subscription");
    let poolid = Uuid::new_v4().to_string();
    damus.pool.subscribe(poolid.clone(), vec![filter.clone()]);

    // Wait for updates to the subscription
    loop {
        match damus.ndb.wait_for_notes(ndbsub, 10).await {
            Ok(nks) => {
                let txn = Transaction::new(&damus.ndb).expect("transaction");
                let muted = handle_nip51_muted(&damus.ndb, &txn, &nks);
                debug!("track_user_muted: update: {:#?}", muted);
                damus.muted = muted;
            }
            Err(err) => error!("err: {:?}", err),
        }
    }
}

fn user_muted_filter(damus: &mut Damus) -> Filter {
    let account = damus
        .accounts
        .get_selected_account()
        .as_ref()
        .map(|a| a.pubkey.bytes())
        .expect("selected account");

    // NIP-65
    Filter::new()
        .authors([account])
        .kinds([10000])
        .limit(1)
        .build()
}

fn handle_nip51_muted(ndb: &Ndb, txn: &Transaction, nks: &Vec<NoteKey>) -> Muted {
    let mut muted = Muted::default();
    for nk in nks.iter() {
        if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
            for tag in note.tags() {
                match tag.get(0).and_then(|t| t.variant().str()) {
                    Some("p") => {
                        if let Some(id) = tag.get(1).and_then(|f| f.variant().id()) {
                            muted.pubkeys.insert(Pubkey::new(id.clone()));
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
                            muted.threads.insert(id.clone());
                        }
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

// Generic task spawning helpers

struct SendableFuture<F> {
    future: Pin<Box<F>>,
    _marker: PhantomData<*const ()>,
}

unsafe impl<F> Send for SendableFuture<F> {}

impl<F: Future> Future for SendableFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}

pub fn spawn_sendable<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    let future = Box::pin(future);
    let sendable_future = SendableFuture {
        future,
        _marker: PhantomData,
    };
    task::spawn(sendable_future);
}
