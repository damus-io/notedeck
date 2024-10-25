use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::Poll;
use tokio::task;

use tracing::{debug, error};

use enostr::RelayPool;
use nostrdb::{Filter, Ndb, Transaction};
use uuid::Uuid;

use crate::note::NoteRef;
use crate::Damus;

pub async fn track_user_relays(damus: &mut Damus) {
    debug!("track_user_relays starting");

    let filter = user_relay_filter(damus);

    // Do we have a user relay list stored in nostrdb? Start with that ...
    let txn = Transaction::new(&damus.ndb).expect("transaction");
    let relays = query_nip65_relays(&damus.ndb, &txn, &filter);
    debug!("track_user_relays: initial from nostrdb: {:#?}", relays);
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
            Ok(vec) => {
                debug!("saw {:?}", vec);
                let txn = Transaction::new(&damus.ndb).expect("transaction");
                let relays = query_nip65_relays(&damus.ndb, &txn, &filter);
                debug!(
                    "track_user_relays: subscription from nostrdb: {:#?}",
                    relays
                );
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

fn query_nip65_relays(ndb: &Ndb, txn: &Transaction, filter: &Filter) -> Vec<String> {
    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
    let results = ndb
        .query(txn, &[filter.clone()], lim)
        .expect("query results");
    results
        .iter()
        .map(|qr| NoteRef::new(qr.note_key, qr.note.created_at()))
        .filter_map(|nr| ndb.get_note_by_key(txn, nr.key).ok())
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
    pool.advertised_relays = relays.into_iter().collect();
    if let Err(e) = pool.configure_relays(wakeup) {
        error!("{:?}", e)
    }
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
