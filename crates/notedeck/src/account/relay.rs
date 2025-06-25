use std::collections::BTreeSet;

use enostr::RelayPool;
use nostrdb::{Filter, Ndb, NoteBuilder, NoteKey, Subscription, Transaction};
use tracing::{debug, error};
use url::Url;
use uuid::Uuid;

use crate::RelaySpec;

pub(crate) struct AccountRelayData {
    pub filter: Filter,
    pub subid: Option<String>,
    pub sub: Option<Subscription>,
    pub local: BTreeSet<RelaySpec>, // used locally but not advertised
    pub advertised: BTreeSet<RelaySpec>, // advertised via NIP-65
}

impl AccountRelayData {
    pub fn new(ndb: &Ndb, pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-65 relay list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10002])
            .limit(1)
            .build();

        // Query the ndb immediately to see if the user list is already there
        let txn = Transaction::new(ndb).expect("transaction");
        let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(&txn, &[filter.clone()], lim)
            .expect("query user relays results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let relays = Self::harvest_nip65_relays(ndb, &txn, &nks);
        debug!(
            "pubkey {}: initial relays {:?}",
            hex::encode(pubkey),
            relays
        );

        AccountRelayData {
            filter,
            subid: None,
            sub: None,
            local: BTreeSet::new(),
            advertised: relays.into_iter().collect(),
        }
    }

    // make this account the current selected account
    pub fn activate(&mut self, ndb: &Ndb, pool: &mut RelayPool) {
        debug!("activating relay sub {}", self.filter.json().unwrap());
        assert_eq!(self.subid, None, "subid already exists");
        assert_eq!(self.sub, None, "sub already exists");

        // local subscription
        let sub = ndb
            .subscribe(&[self.filter.clone()])
            .expect("ndb relay list subscription");

        // remote subscription
        let subid = Uuid::new_v4().to_string();
        pool.subscribe(subid.clone(), vec![self.filter.clone()]);

        self.sub = Some(sub);
        self.subid = Some(subid);
    }

    // this account is no longer the selected account
    pub fn deactivate(&mut self, ndb: &mut Ndb, pool: &mut RelayPool) {
        debug!("deactivating relay sub {}", self.filter.json().unwrap());
        assert_ne!(self.subid, None, "subid doesn't exist");
        assert_ne!(self.sub, None, "sub doesn't exist");

        // remote subscription
        pool.unsubscribe(self.subid.as_ref().unwrap().clone());

        // local subscription
        ndb.unsubscribe(self.sub.unwrap())
            .expect("ndb relay list unsubscribe");

        self.sub = None;
        self.subid = None;
    }

    // standardize the format (ie, trailing slashes) to avoid dups
    pub fn canonicalize_url(url: &str) -> String {
        match Url::parse(url) {
            Ok(parsed_url) => parsed_url.to_string(),
            Err(_) => url.to_owned(), // If parsing fails, return the original URL.
        }
    }

    pub(crate) fn harvest_nip65_relays(
        ndb: &Ndb,
        txn: &Transaction,
        nks: &[NoteKey],
    ) -> Vec<RelaySpec> {
        let mut relays = Vec::new();
        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                for tag in note.tags() {
                    match tag.get(0).and_then(|t| t.variant().str()) {
                        Some("r") => {
                            if let Some(url) = tag.get(1).and_then(|f| f.variant().str()) {
                                let has_read_marker = tag
                                    .get(2)
                                    .is_some_and(|m| m.variant().str() == Some("read"));
                                let has_write_marker = tag
                                    .get(2)
                                    .is_some_and(|m| m.variant().str() == Some("write"));
                                relays.push(RelaySpec::new(
                                    Self::canonicalize_url(url),
                                    has_read_marker,
                                    has_write_marker,
                                ));
                            }
                        }
                        Some("alt") => {
                            // ignore for now
                        }
                        Some(x) => {
                            error!("harvest_nip65_relays: unexpected tag type: {}", x);
                        }
                        None => {
                            error!("harvest_nip65_relays: invalid tag");
                        }
                    }
                }
            }
        }
        relays
    }

    pub fn publish_nip65_relays(&self, seckey: &[u8; 32], pool: &mut RelayPool) {
        let mut builder = NoteBuilder::new().kind(10002).content("");
        for rs in &self.advertised {
            builder = builder.start_tag().tag_str("r").tag_str(&rs.url);
            if rs.has_read_marker {
                builder = builder.tag_str("read");
            } else if rs.has_write_marker {
                builder = builder.tag_str("write");
            }
        }
        let note = builder.sign(seckey).build().expect("note build");
        pool.send(&enostr::ClientMessage::event(&note).expect("note client message"));
    }
}

pub(crate) struct RelayDefaults {
    pub forced_relays: BTreeSet<RelaySpec>,
    pub bootstrap_relays: BTreeSet<RelaySpec>,
}

impl RelayDefaults {
    pub(crate) fn new(forced_relays: Vec<String>) -> Self {
        let forced_relays: BTreeSet<RelaySpec> = forced_relays
            .into_iter()
            .map(|u| RelaySpec::new(AccountRelayData::canonicalize_url(&u), false, false))
            .collect();
        let bootstrap_relays = [
            "wss://relay.damus.io",
            // "wss://pyramid.fiatjaf.com",  // Uncomment if needed
            "wss://nos.lol",
            "wss://nostr.wine",
            "wss://purplepag.es",
        ]
        .iter()
        .map(|&url| url.to_string())
        .map(|u| RelaySpec::new(AccountRelayData::canonicalize_url(&u), false, false))
        .collect();

        Self {
            forced_relays,
            bootstrap_relays,
        }
    }
}
