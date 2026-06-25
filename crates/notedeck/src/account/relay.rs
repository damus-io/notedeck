use std::collections::BTreeSet;

use crate::{AccountData, RelaySpec, RemoteApi};
use enostr::{Keypair, NormRelayUrl, RelayId};
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
                parse_nip65_relays_note(&note, &mut relays);
            }
        }
        relays
    }

    pub fn new_nip65_relays_note(&'_ self, seckey: &[u8; 32]) -> Note<'_> {
        construct_nip65_relays_note(&self.advertised)
            .sign(seckey)
            .build()
            .expect("note build")
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

/// Parses the `r` tags of a single kind-10002 NIP-65 note into [`RelaySpec`]s,
/// appending them to `relays`.
pub(crate) fn parse_nip65_relays_note(note: &Note, relays: &mut Vec<RelaySpec>) {
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
                    // private marker lives at index 3 so it never collides
                    // with the read/write marker at index 2
                    let is_private = tag
                        .get(3)
                        .is_some_and(|m| m.variant().str() == Some("private"));

                    let Ok(norm_url) = NormRelayUrl::new(url) else {
                        continue;
                    };
                    relays.push(
                        RelaySpec::new(norm_url, has_read_marker, has_write_marker)
                            .with_private(is_private),
                    );
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

/// Builds a kind-10002 NIP-65 relay-list note for the provided advertised relays.
pub fn construct_nip65_relays_note<'a>(
    relay_specs: impl IntoIterator<Item = &'a RelaySpec>,
) -> NoteBuilder<'a> {
    let mut builder = NoteBuilder::new().kind(10002).content("");
    for relay_spec in relay_specs {
        builder = builder
            .start_tag()
            .tag_str("r")
            .tag_str(&relay_spec.url.to_string());
        // Emit the read/write marker (index 2) then the private marker (index 3)
        // so `private` always lands at the 4th tag entry, never colliding with
        // read/write. A no-marker private relay gets an empty placeholder at
        // index 2 to keep `private` at index 3.
        if relay_spec.has_read_marker {
            builder = builder.tag_str("read");
        } else if relay_spec.has_write_marker {
            builder = builder.tag_str("write");
        } else if relay_spec.is_private {
            builder = builder.tag_str("");
        }
        if relay_spec.is_private {
            builder = builder.tag_str("private");
        }
    }
    builder
}

pub(crate) struct RelayDefaults {
    pub forced_relays: BTreeSet<RelaySpec>,
    pub bootstrap_relays: BTreeSet<RelaySpec>,
}

/// Fallback relays an account with no relays of its own connects to. Callers
/// pass this into [`RelayDefaults::new`] in normal operation; passing an empty
/// set instead (e.g. in tests) keeps a fresh account from connecting anywhere,
/// so the suite never reaches out to production relays.
pub const DEFAULT_BOOTSTRAP_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    // "wss://pyramid.fiatjaf.com",  // Uncomment if needed
    "wss://nos.lol",
    "wss://nostr.wine",
    "wss://purplepag.es",
];

/// The default fallback relays as an owned list, for passing to
/// [`RelayDefaults::new`]/[`crate::Accounts::new`] in normal operation.
pub fn default_bootstrap_relays() -> Vec<String> {
    DEFAULT_BOOTSTRAP_RELAYS
        .iter()
        .map(|&url| url.to_string())
        .collect()
}

impl RelayDefaults {
    pub(crate) fn new(forced_relays: Vec<String>, bootstrap_relays: Vec<String>) -> Self {
        let forced_relays: BTreeSet<RelaySpec> = forced_relays
            .into_iter()
            .filter_map(|u| Some(RelaySpec::new(NormRelayUrl::new(&u).ok()?, false, false)))
            .collect();
        let bootstrap_relays = bootstrap_relays
            .into_iter()
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
    /// Mark/unmark an advertised relay as a private sync relay.
    SetPrivate(String, bool),
}

impl RelayAction {
    pub(super) fn get_url(&self) -> &str {
        match self {
            RelayAction::Add(url) => url,
            RelayAction::Remove(url) => url,
            RelayAction::SetPrivate(url, _) => url,
        }
    }
}

pub(super) fn modify_advertised_relays(
    kp: &Keypair,
    action: RelayAction,
    remote: &mut RemoteApi<'_>,
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
        RelayAction::SetPrivate(_, yes) => {
            info!("set advertised relay \"{relay_url_str}\" private={yes}")
        }
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
        RelayAction::SetPrivate(_, yes) => {
            // Preserve the existing read/write markers; only flip the private
            // flag. RelaySpec equality is url-only, so `replace` swaps the
            // stored spec in place.
            let existing = advertised
                .get(&RelaySpec::new(relay_url.clone(), false, false))
                .cloned()
                .unwrap_or_else(|| RelaySpec::new(relay_url, false, false));
            advertised.replace(existing.with_private(yes));
        }
    }

    // If we have the secret key publish the NIP-65 relay list
    if let Some(secretkey) = &kp.secret_key {
        let note = account_data
            .relay
            .new_nip65_relays_note(&secretkey.to_secret_bytes());

        let mut publisher = remote.publisher_explicit();
        publisher.publish_note(&note, write_relays(relay_defaults, &account_data.relay));
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

#[cfg(test)]
mod tests {
    use super::{construct_nip65_relays_note, parse_nip65_relays_note};
    use crate::RelaySpec;
    use enostr::{FullKeypair, NormRelayUrl};

    #[test]
    fn construct_nip65_relays_note_emits_expected_tags() {
        let owner = FullKeypair::generate();
        let relays = vec![
            RelaySpec::new(
                NormRelayUrl::new("wss://relay-read.example.com").expect("read relay"),
                true,
                false,
            ),
            RelaySpec::new(
                NormRelayUrl::new("wss://relay-write.example.com").expect("write relay"),
                false,
                true,
            ),
            RelaySpec::new(
                NormRelayUrl::new("wss://relay-both.example.com").expect("both relay"),
                false,
                false,
            ),
        ];

        let note = construct_nip65_relays_note(&relays)
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("relay list note");

        assert_eq!(note.kind(), 10002);
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-read.example.com/")
                && tag.get_str(2) == Some("read")
        }));
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-write.example.com/")
                && tag.get_str(2) == Some("write")
        }));
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-both.example.com/")
                && tag.get(2).is_none()
        }));
    }

    /// A private relay with no read/write marker emits an empty placeholder at
    /// index 2 so `"private"` always lands at index 3.
    #[test]
    fn construct_nip65_relays_note_emits_private_marker_at_index_3() {
        let owner = FullKeypair::generate();
        let relays = vec![
            RelaySpec::new(
                NormRelayUrl::new("wss://private-both.example.com").expect("relay"),
                false,
                false,
            )
            .with_private(true),
            RelaySpec::new(
                NormRelayUrl::new("wss://private-read.example.com").expect("relay"),
                true,
                false,
            )
            .with_private(true),
        ];

        let note = construct_nip65_relays_note(&relays)
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("relay list note");

        // no marker + private -> ["r", url, "", "private"]
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://private-both.example.com/")
                && tag.get_str(2) == Some("")
                && tag.get_str(3) == Some("private")
        }));
        // read marker + private -> ["r", url, "read", "private"]
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://private-read.example.com/")
                && tag.get_str(2) == Some("read")
                && tag.get_str(3) == Some("private")
        }));
    }

    /// Non-private relays serialize to exactly the same tags as before the
    /// private marker existed (no trailing empty/placeholder entries).
    #[test]
    fn construct_nip65_relays_note_non_private_unchanged() {
        let owner = FullKeypair::generate();
        let relays = vec![
            RelaySpec::new(
                NormRelayUrl::new("wss://relay-read.example.com").expect("relay"),
                true,
                false,
            ),
            RelaySpec::new(
                NormRelayUrl::new("wss://relay-both.example.com").expect("relay"),
                false,
                false,
            ),
        ];

        let note = construct_nip65_relays_note(&relays)
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("relay list note");

        // read relay: marker at index 2, nothing at index 3
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-read.example.com/")
                && tag.get_str(2) == Some("read")
                && tag.get(3).is_none()
        }));
        // both relay: nothing past the url
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-both.example.com/")
                && tag.get(2).is_none()
        }));
    }

    /// Serialize -> parse round-trips the private flag while preserving the
    /// read/write markers.
    #[test]
    fn private_marker_round_trips() {
        let owner = FullKeypair::generate();
        let original = vec![
            RelaySpec::new(
                NormRelayUrl::new("wss://private-both.example.com").expect("relay"),
                false,
                false,
            )
            .with_private(true),
            RelaySpec::new(
                NormRelayUrl::new("wss://private-write.example.com").expect("relay"),
                false,
                true,
            )
            .with_private(true),
            RelaySpec::new(
                NormRelayUrl::new("wss://public-read.example.com").expect("relay"),
                true,
                false,
            ),
        ];

        let note = construct_nip65_relays_note(&original)
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("relay list note");

        let mut parsed = Vec::new();
        parse_nip65_relays_note(&note, &mut parsed);

        let find = |url: &str| {
            parsed
                .iter()
                .find(|r| r.url.to_string() == url)
                .unwrap_or_else(|| panic!("missing {url}"))
        };

        let both = find("wss://private-both.example.com/");
        assert!(both.is_private);
        assert!(!both.has_read_marker);
        assert!(!both.has_write_marker);

        let write = find("wss://private-write.example.com/");
        assert!(write.is_private);
        assert!(write.has_write_marker);

        let read = find("wss://public-read.example.com/");
        assert!(!read.is_private);
        assert!(read.has_read_marker);
    }
}
