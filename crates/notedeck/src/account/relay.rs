use std::collections::BTreeSet;

use crate::{AccountData, EguiWakeup, RelaySpec};
use enostr::{Keypair, NormRelayUrl, OutboxSessionHandler, RelayId};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, Note, NoteBuilder, NoteKey, Subscription, Transaction};
use tracing::{debug, error, info};

#[derive(Clone)]
pub(crate) struct AccountRelayData {
    pub filter: Filter,
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
            filter,
            local: BTreeSet::new(),
            advertised: BTreeSet::new(),
        }
    }

    pub fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        // Query the ndb immediately to see if the user list is already there
        let lim = self
            .filter
            .limit()
            .unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(txn, std::slice::from_ref(&self.filter), lim)
            .expect("query user relays results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let relays = Self::harvest_nip65_relays(ndb, txn, &nks);
        debug!("initial relays {:?}", relays);

        self.advertised = relays.into_iter().collect()
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

                                let Ok(norm_url) = NormRelayUrl::new(url) else {
                                    continue;
                                };
                                relays.push(RelaySpec::new(
                                    norm_url,
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

    pub fn new_nip65_relays_note(&'_ self, seckey: &[u8; 32]) -> Note<'_> {
        let mut builder = NoteBuilder::new().kind(10002).content("");
        for rs in &self.advertised {
            builder = builder
                .start_tag()
                .tag_str("r")
                .tag_str(&rs.url.to_string());
            if rs.has_read_marker {
                builder = builder.tag_str("read");
            } else if rs.has_write_marker {
                builder = builder.tag_str("write");
            }
        }
        builder.sign(seckey).build().expect("note build")
    }

    #[profiling::function]
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
            .filter_map(|u| Some(RelaySpec::new(NormRelayUrl::new(&u).ok()?, false, false)))
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
        .filter_map(|u| Some(RelaySpec::new(NormRelayUrl::new(&u).ok()?, false, false)))
        .collect();

        Self {
            forced_relays,
            bootstrap_relays,
        }
    }
}

pub fn calculate_relays(
    relay_defaults: &RelayDefaults,
    data: &AccountRelayData,
    readable: bool, // are we calculating the readable relays? or the writable?
) -> HashSet<NormRelayUrl> {
    // If forced relays are set use them only
    let mut desired_relays = relay_defaults.forced_relays.clone();

    // Compose the desired relay lists from the selected account
    if desired_relays.is_empty() {
        desired_relays.extend(
            data.local
                .iter()
                .filter(|l| {
                    if readable {
                        l.is_readable()
                    } else {
                        l.is_writable()
                    }
                })
                .cloned(),
        );
        desired_relays.extend(
            data.advertised
                .iter()
                .filter(|l| {
                    if readable {
                        l.is_readable()
                    } else {
                        l.is_writable()
                    }
                })
                .cloned(),
        );
    }

    // If no relays are specified at this point use the bootstrap list
    if desired_relays.is_empty() {
        desired_relays = relay_defaults.bootstrap_relays.clone();
    }

    debug!("desired relays: {:?}", desired_relays);

    desired_relays.into_iter().map(|r| r.url).collect()
}

// TODO(kernelkind): these should have `NormRelayUrl` instead of `String`...
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
    pool: &mut OutboxSessionHandler<'_, EguiWakeup>,
    relay_defaults: &RelayDefaults,
    account_data: &mut AccountData,
) {
    let Ok(relay_url) = NormRelayUrl::new(action.get_url()) else {
        return;
    };

    let relay_url_str = relay_url.to_string();
    match action {
        RelayAction::Add(_) => info!("add advertised relay \"{relay_url_str}\""),
        RelayAction::Remove(_) => info!("remove advertised relay \"{relay_url_str}\""),
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
        let note = account_data
            .relay
            .new_nip65_relays_note(&secretkey.to_secret_bytes());

        pool.broadcast_note(&note, write_relays(relay_defaults, &account_data.relay));
    }
}

pub fn write_relays(relay_defaults: &RelayDefaults, data: &AccountRelayData) -> Vec<RelayId> {
    let mut relays: Vec<RelayId> = calculate_relays(relay_defaults, data, false)
        .into_iter()
        .map(RelayId::Websocket)
        .collect();

    relays.push(RelayId::Multicast);

    relays
}
