use futures::stream::StreamExt;
use tracing::debug;

use nostrdb::{Filter, Ndb, Transaction};

use crate::dispatcher;
use crate::note::NoteRef;
use crate::{with_mut_damus, DamusRef};

pub async fn setup_user_relays(damusref: DamusRef) {
    debug!("do_setup_user_relays starting");

    let filter = with_mut_damus(&damusref, |damus| {
        debug!("setup_user_relays: acquired damus for filter");

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
    });

    let mut sub = with_mut_damus(&damusref, |mut damus| {
        debug!("setup_user_relays: acquired damus for query + subscribe");
        let txn = Transaction::new(&damus.ndb).expect("transaction");
        let results = query_note_json(&damus.ndb, &txn, &filter);
        debug!("setup_user_relays: query #1 results: {:#?}", results);

        // Add a relay subscription to the pool
        dispatcher::subscribe(&mut damus, &[filter.clone()], 10).expect("subscribe")
    });
    debug!("setup_user_relays: sub {}", sub.id);

    loop {
        match sub.receiver.next().await {
            Some(ev) => {
                debug!("setup_user_relays: saw {:?}", ev);
                with_mut_damus(&damusref, |damus| {
                    let txn = Transaction::new(&damus.ndb).expect("transaction");
                    let results = query_note_json(&damus.ndb, &txn, &filter);
                    debug!("setup_user_relays: query #2 results: {:#?}", results);
                })
            }
            None => {
                debug!("setup_user_relays: saw None");
                break;
            }
        }
    }

    debug!("do_setup_user_relays finished");
}

fn query_note_json<'a>(ndb: &Ndb, txn: &'a Transaction, filter: &Filter) -> Vec<String> {
    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
    let results = ndb
        .query(&txn, &[filter.clone()], lim)
        .expect("query results");
    results
        .iter()
        .map(|qr| NoteRef::new(qr.note_key, qr.note.created_at()))
        .filter_map(|nr| ndb.get_note_by_key(&txn, nr.key).ok())
        .map(|n| n.json().unwrap())
        .collect()
}
