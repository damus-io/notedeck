use futures::stream::StreamExt;
use tracing::{debug, error};

use enostr::RelayPool;
use nostrdb::{Filter, Ndb, Transaction};

use crate::dispatcher;
use crate::note::NoteRef;
use crate::{with_mut_damus, DamusRef};

pub async fn track_user_relays(damusref: DamusRef) {
    debug!("track_user_relays starting");

    let filter = user_relay_filter(&damusref);

    // Do we have a user relay list stored in nostrdb? Start with that ...
    with_mut_damus(&damusref, |damus| {
        let txn = Transaction::new(&damus.ndb).expect("transaction");
        let relays = query_nip65_relays(&damus.ndb, &txn, &filter);
        debug!("track_user_relays: initial from nostrdb: {:#?}", relays);
        set_relays(&mut damus.pool, relays);
    });

    // Subscribe to user relay list updates
    let mut src = with_mut_damus(&damusref, |damus| {
        dispatcher::subscribe(damus, &[filter.clone()], 10).expect("subscribe")
    });
    debug!(
        "track_user_relays: ndbid: {}, poolid: {}",
        src.ndbid, src.poolid
    );

    // Track user relay list updates
    loop {
        match src.receiver.next().await {
            Some(_ev) => with_mut_damus(&damusref, |damus| {
                let txn = Transaction::new(&damus.ndb).expect("transaction");
                let relays = query_nip65_relays(&damus.ndb, &txn, &filter);
                debug!("track_user_relays update: {:#?}", relays);
                set_relays(&mut damus.pool, relays);
            }),
            None => {
                debug!("track_user_relays: saw None");
                break;
            }
        }
    }

    // Should only get here if the channel is closed
    debug!("track_user_relays finished");
}

fn user_relay_filter(damusref: &DamusRef) -> Filter {
    with_mut_damus(&damusref, |damus| {
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
    })
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

fn set_relays(pool: &mut RelayPool, relays: Vec<String>) {
    let wakeup = move || {
        // FIXME - how do we repaint?
    };
    if let Err(e) = pool.set_relays(&relays, wakeup) {
        error!("{:?}", e)
    }
}
