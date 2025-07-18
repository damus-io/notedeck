use std::collections::BTreeSet;

use enostr::{Keypair, Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, NoteBuilder, NoteKey, Subscription, Transaction};
use tracing::{debug, error, info};
use url::Url;

use crate::{filter::NamedFilter, AccountData, RelaySpec};

pub(crate) struct AccountRelayData {
    pub filter: NamedFilter,
    pub local: BTreeSet<RelaySpec>, // used locally but not advertised
    pub advertised: BTreeSet<RelaySpec>, // advertised via NIP-65
}

impl AccountRelayData {
    pub fn new(pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-65 relay list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10002])
            .limit(1)
            .build();

        AccountRelayData {
            filter: NamedFilter::new("user-relay-list", vec![filter]),
            local: BTreeSet::new(),
            advertised: BTreeSet::new(),
        }
    }

    pub fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        // Query the ndb immediately to see if the user list is already there
        let lim = self.filter.filter[0]
            .limit()
            .unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(txn, &self.filter.filter, lim)
            .expect("query user relays results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let relays = Self::harvest_nip65_relays(ndb, txn, &nks);
        debug!("initial relays {:?}", relays);

        self.advertised = relays.into_iter().collect()
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

    pub fn poll_for_updates(&mut self, ndb: &Ndb, txn: &Transaction, sub: Subscription) -> bool {
        let nks = ndb.poll_for_notes(sub, 1);

        if nks.is_empty() {
            return false;
        }

        let relays = AccountRelayData::harvest_nip65_relays(ndb, txn, &nks);
        debug!("updated relays {:?}", relays);
        self.advertised = relays.into_iter().collect();

        true
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
            "multicast",
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

pub(super) fn update_relay_configuration(
    pool: &mut RelayPool,
    relay_defaults: &RelayDefaults,
    pk: &Pubkey,
    data: &AccountRelayData,
    wakeup: impl Fn() + Send + Sync + Clone + 'static,
) {
    debug!(
        "updating relay configuration for currently selected {:?}",
        pk.hex()
    );

    // If forced relays are set use them only
    let mut desired_relays = relay_defaults.forced_relays.clone();

    // Compose the desired relay lists from the selected account
    if desired_relays.is_empty() {
        desired_relays.extend(data.local.iter().cloned());
        desired_relays.extend(data.advertised.iter().cloned());
    }

    // If no relays are specified at this point use the bootstrap list
    if desired_relays.is_empty() {
        desired_relays = relay_defaults.bootstrap_relays.clone();
    }

    debug!("current relays: {:?}", pool.urls());
    debug!("desired relays: {:?}", desired_relays);

    let pool_specs = pool
        .urls()
        .iter()
        .map(|url| RelaySpec::new(url.clone(), false, false))
        .collect();
    let add: BTreeSet<RelaySpec> = desired_relays.difference(&pool_specs).cloned().collect();
    let mut sub: BTreeSet<RelaySpec> = pool_specs.difference(&desired_relays).cloned().collect();
    if !add.is_empty() {
        debug!("configuring added relays: {:?}", add);
        let _ = pool.add_urls(add.iter().map(|r| r.url.clone()).collect(), wakeup);
    }
    if !sub.is_empty() {
        // certain relays are persistent like the multicast relay,
        // although we should probably have a way to explicitly
        // disable it
        sub.remove(&RelaySpec::new("multicast", false, false));

        debug!("removing unwanted relays: {:?}", sub);
        pool.remove_urls(&sub.iter().map(|r| r.url.clone()).collect());
    }

    debug!("current relays: {:?}", pool.urls());
}

pub enum RelayAction {
    Add(String),
    Remove(String),
}

impl RelayAction {
    pub(super) fn get_url(&self) -> &str {
        match self {
            RelayAction::Add(url) => url,
            RelayAction::Remove(url) => url,
        }
    }
}

pub(super) fn modify_advertised_relays(
    kp: &Keypair,
    action: RelayAction,
    pool: &mut RelayPool,
    relay_defaults: &RelayDefaults,
    account_data: &mut AccountData,
) {
    let relay_url = AccountRelayData::canonicalize_url(action.get_url());
    match action {
        RelayAction::Add(_) => info!("add advertised relay \"{}\"", relay_url),
        RelayAction::Remove(_) => info!("remove advertised relay \"{}\"", relay_url),
    }

    // let selected = self.cache.selected_mut();

    let advertised = &mut account_data.relay.advertised;
    if advertised.is_empty() {
        // If the selected account has no advertised relays,
        // initialize with the bootstrapping set.
        advertised.extend(relay_defaults.bootstrap_relays.iter().cloned());
    }
    match action {
        RelayAction::Add(_) => {
            advertised.insert(RelaySpec::new(relay_url, false, false));
        }
        RelayAction::Remove(_) => {
            advertised.remove(&RelaySpec::new(relay_url, false, false));
        }
    }

    // If we have the secret key publish the NIP-65 relay list
    if let Some(secretkey) = &kp.secret_key {
        account_data
            .relay
            .publish_nip65_relays(&secretkey.to_secret_bytes(), pool);
    }
}
