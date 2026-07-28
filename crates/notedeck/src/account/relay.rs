use std::collections::BTreeSet;

use crate::{AccountData, RelaySpec, RemoteApi};
use enostr::{Keypair, NormRelayUrl, RelayId};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, Note, NoteBuilder, NoteKey, Subscription, Transaction};
use tracing::{debug, error, info};

#[derive(Clone)]
pub(crate) struct AccountRelayData {
    pub filter: Filter,
    /// Filter for the account's kind-10013 NIP-37 private relay list.
    pub private_filter: Filter,
    pub local: BTreeSet<RelaySpec>, // used locally but not advertised
    pub advertised: BTreeSet<RelaySpec>, // advertised via NIP-65
    /// Private-sync relays from the decrypted kind-10013 list (NIP-37). Used by
    /// dave/headway/notebook to sync private state across the user's own
    /// devices. Empty for read-only accounts (can't decrypt the list).
    pub private: BTreeSet<NormRelayUrl>,
}

impl AccountRelayData {
    pub fn new(pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-65 relay list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10002])
            .limit(1)
            .build();

        // ... and one for the kind-10013 private relay list (NIP-37).
        let private_filter = Filter::new()
            .authors([pubkey])
            .kinds([PRIVATE_RELAY_LIST_KIND as u64])
            .limit(1)
            .build();

        AccountRelayData {
            filter,
            private_filter,
            local: BTreeSet::new(),
            advertised: BTreeSet::new(),
            private: BTreeSet::new(),
        }
    }

    pub fn query(&mut self, ndb: &Ndb, txn: &Transaction, keypair: &Keypair) {
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

        self.advertised = relays.into_iter().collect();
        self.private = self.query_private_relays(ndb, txn, keypair);
    }

    /// Query the ndb for the account's current kind-10013 private relay list and
    /// return the decrypted relay set.
    fn query_private_relays(
        &self,
        ndb: &Ndb,
        txn: &Transaction,
        keypair: &Keypair,
    ) -> BTreeSet<NormRelayUrl> {
        let nks = ndb
            .query(txn, std::slice::from_ref(&self.private_filter), 1)
            .expect("query private relays results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        Self::harvest_private_relays(ndb, txn, &nks, keypair)
            .into_iter()
            .collect()
    }

    pub(crate) fn harvest_private_relays(
        ndb: &Ndb,
        txn: &Transaction,
        nks: &[NoteKey],
        keypair: &Keypair,
    ) -> Vec<NormRelayUrl> {
        let mut relays = Vec::new();
        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                parse_private_relay_list_note(&note, keypair, &mut relays);
            }
        }
        relays
    }

    pub fn new_private_relay_list_note(&'_ self, keypair: &Keypair) -> Option<Note<'_>> {
        construct_private_relay_list_note(self.private.iter(), keypair)
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

        let advertised: BTreeSet<RelaySpec> =
            AccountRelayData::harvest_nip65_relays(ndb, txn, &nks)
                .into_iter()
                .collect();

        // A 10002 note landed, but it may carry the same relay set we already
        // have. Only report a change (and trigger a read-relay retarget) when
        // the advertised set actually differs, so callers don't re-resolve
        // relays for a no-op update.
        if advertised == self.advertised {
            return false;
        }

        debug!("updated relays {:?}", advertised);
        self.advertised = advertised;

        true
    }

    /// Poll the kind-10013 private relay subscription, re-decrypting the list
    /// when a new note lands. Needs the account `keypair` to decrypt.
    #[profiling::function]
    pub fn poll_private_for_updates(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        sub: Subscription,
        keypair: &Keypair,
    ) {
        let nks = ndb.poll_for_notes(sub, 1);
        if nks.is_empty() {
            return;
        }

        let private: BTreeSet<NormRelayUrl> =
            AccountRelayData::harvest_private_relays(ndb, txn, &nks, keypair)
                .into_iter()
                .collect();

        if private != self.private {
            debug!("updated private relays {:?}", private);
            self.private = private;
        }
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

                    let Ok(norm_url) = NormRelayUrl::new(url) else {
                        continue;
                    };
                    relays.push(RelaySpec::new(norm_url, has_read_marker, has_write_marker));
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
        if relay_spec.has_read_marker {
            builder = builder.tag_str("read");
        } else if relay_spec.has_write_marker {
            builder = builder.tag_str("write");
        }
    }
    builder
}

/// NIP-37 "Relay List for Private Content" (kind `10013`).
///
/// The user's private-sync relays are *not* published as a public NIP-65
/// marker — they live in a dedicated kind-10013 event whose `.content` is the
/// NIP-44 self-encrypted (encrypted to the author's own pubkey) JSON array of
/// `["relay", url]` private tags. This keeps the private set off the public
/// relay list, is only decryptable by the author, and is the same event
/// Amethyst uses, so the private relay set round-trips across clients.
pub const PRIVATE_RELAY_LIST_KIND: u32 = 10013;

/// NIP-44 self-encrypt `plaintext` to the keypair's own pubkey. Returns `None`
/// for a read-only (pubkey-only) account — it has no secret key to encrypt with.
fn nip44_self_encrypt(keypair: &Keypair, plaintext: &str) -> Option<String> {
    let secret_key = keypair.secret_key.as_ref()?;
    let public_key = nostr::PublicKey::from_slice(keypair.pubkey.bytes()).ok()?;
    nostr::nips::nip44::encrypt(
        secret_key,
        &public_key,
        plaintext,
        nostr::nips::nip44::Version::default(),
    )
    .ok()
}

/// NIP-44 self-decrypt `payload` that was encrypted to the keypair's own pubkey.
/// Returns `None` for a read-only account or on any decode/decrypt failure.
fn nip44_self_decrypt(keypair: &Keypair, payload: &str) -> Option<String> {
    let secret_key = keypair.secret_key.as_ref()?;
    let public_key = nostr::PublicKey::from_slice(keypair.pubkey.bytes()).ok()?;
    nostr::nips::nip44::decrypt(secret_key, &public_key, payload).ok()
}

/// Parse a single kind-10013 note's encrypted `.content` into private relay
/// URLs, appending to `relays`. Needs the account `keypair` to decrypt; a
/// read-only account or a note we can't decrypt yields nothing.
pub(crate) fn parse_private_relay_list_note(
    note: &Note,
    keypair: &Keypair,
    relays: &mut Vec<NormRelayUrl>,
) {
    let Some(plaintext) = nip44_self_decrypt(keypair, note.content()) else {
        return;
    };
    // Private tags are a JSON array of tags, e.g. [["relay","wss://..."], ...].
    let Ok(tags) = serde_json::from_str::<Vec<Vec<String>>>(&plaintext) else {
        error!("private relay list: malformed decrypted content");
        return;
    };
    for tag in tags {
        if tag.first().map(String::as_str) == Some("relay") {
            if let Some(url) = tag.get(1).and_then(|u| NormRelayUrl::new(u).ok()) {
                relays.push(url);
            }
        }
    }
}

/// Build a kind-10013 NIP-37 private-relay-list note for `relays`, NIP-44
/// self-encrypting the relay set into `.content`. Returns `None` for a
/// read-only account (no secret key to encrypt/sign with).
pub fn construct_private_relay_list_note<'a>(
    relays: impl IntoIterator<Item = &'a NormRelayUrl>,
    keypair: &Keypair,
) -> Option<Note<'a>> {
    let secret_key = keypair.secret_key.as_ref()?;
    let tags: Vec<Vec<String>> = relays
        .into_iter()
        .map(|url| vec!["relay".to_string(), url.to_string()])
        .collect();
    let plaintext = serde_json::to_string(&tags).ok()?;
    let content = nip44_self_encrypt(keypair, &plaintext)?;
    NoteBuilder::new()
        .kind(PRIVATE_RELAY_LIST_KIND)
        .content(&content)
        .sign(&secret_key.to_secret_bytes())
        .build()
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
    /// Add a relay to the kind-10013 NIP-37 private sync relay list.
    AddPrivate(String),
    /// Remove a relay from the kind-10013 NIP-37 private sync relay list.
    RemovePrivate(String),
}

impl RelayAction {
    pub(super) fn get_url(&self) -> &str {
        match self {
            RelayAction::Add(url) => url,
            RelayAction::Remove(url) => url,
            RelayAction::AddPrivate(url) => url,
            RelayAction::RemovePrivate(url) => url,
        }
    }

    /// Whether this action mutates the kind-10013 private relay list rather than
    /// the public NIP-65 (kind-10002) advertised list.
    fn is_private(&self) -> bool {
        matches!(
            self,
            RelayAction::AddPrivate(_) | RelayAction::RemovePrivate(_)
        )
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

    if action.is_private() {
        modify_private_relays(kp, action, relay_url, remote, relay_defaults, account_data);
        return;
    }

    let relay_url_str = relay_url.to_string();
    match action {
        RelayAction::Add(_) => info!("add advertised relay \"{relay_url_str}\""),
        RelayAction::Remove(_) => info!("remove advertised relay \"{relay_url_str}\""),
        RelayAction::AddPrivate(_) | RelayAction::RemovePrivate(_) => unreachable!(),
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
        RelayAction::AddPrivate(_) | RelayAction::RemovePrivate(_) => unreachable!(),
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

/// Apply an `AddPrivate`/`RemovePrivate` action: mutate the in-memory private
/// relay set and republish the kind-10013 NIP-37 list to the account's NIP-65
/// write relays (and the private relays themselves, so the new set lands on the
/// very relays it names). A read-only account can't encrypt/sign the list, so
/// the change stays local.
fn modify_private_relays(
    kp: &Keypair,
    action: RelayAction,
    relay_url: NormRelayUrl,
    remote: &mut RemoteApi<'_>,
    relay_defaults: &RelayDefaults,
    account_data: &mut AccountData,
) {
    let relay_url_str = relay_url.to_string();
    let private = &mut account_data.relay.private;
    match action {
        RelayAction::AddPrivate(_) => {
            info!("add private relay \"{relay_url_str}\"");
            private.insert(relay_url);
        }
        RelayAction::RemovePrivate(_) => {
            info!("remove private relay \"{relay_url_str}\"");
            private.remove(&relay_url);
        }
        RelayAction::Add(_) | RelayAction::Remove(_) => unreachable!(),
    }

    // Encrypt + sign the kind-10013 list. None for a read-only account.
    let Some(note) = account_data.relay.new_private_relay_list_note(kp) else {
        return;
    };

    // NIP-37: publish to the author's NIP-65 write relays. Also target the
    // private relays directly so the list is recoverable from them too.
    let mut targets = write_relays(relay_defaults, &account_data.relay);
    for url in &account_data.relay.private {
        let id = RelayId::Websocket(url.clone());
        if !targets.contains(&id) {
            targets.push(id);
        }
    }

    let mut publisher = remote.publisher_explicit();
    publisher.publish_note(&note, targets);
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
    use super::{
        construct_nip65_relays_note, construct_private_relay_list_note,
        parse_private_relay_list_note, PRIVATE_RELAY_LIST_KIND,
    };
    use crate::RelaySpec;
    use enostr::{FullKeypair, Keypair, NormRelayUrl};

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

    /// A kind-10013 private relay list is a 10013 event whose `.content` is
    /// NIP-44 encrypted (not plaintext) and carries no public relay tags.
    #[test]
    fn construct_private_relay_list_note_is_encrypted_10013() {
        let owner = FullKeypair::generate().to_keypair();
        let relays = [
            NormRelayUrl::new("wss://private-a.example.com").expect("relay"),
            NormRelayUrl::new("wss://private-b.example.com").expect("relay"),
        ];

        let note = construct_private_relay_list_note(relays.iter(), &owner)
            .expect("private relay list note");

        assert_eq!(note.kind(), PRIVATE_RELAY_LIST_KIND);
        // The relay set must not leak into the public content or tags.
        assert!(!note.content().contains("private-a.example.com"));
        assert_eq!(note.tags().into_iter().count(), 0);
    }

    /// Encrypt -> decrypt round-trips the private relay set for the author.
    #[test]
    fn private_relay_list_round_trips() {
        let owner = FullKeypair::generate().to_keypair();
        let relays = [
            NormRelayUrl::new("wss://private-a.example.com").expect("relay"),
            NormRelayUrl::new("wss://private-b.example.com").expect("relay"),
        ];

        let note = construct_private_relay_list_note(relays.iter(), &owner)
            .expect("private relay list note");

        let mut parsed = Vec::new();
        parse_private_relay_list_note(&note, &owner, &mut parsed);
        parsed.sort_by_key(|u| u.to_string());

        assert_eq!(parsed.as_slice(), &relays[..]);
    }

    /// A different account can't decrypt the author's private relay list.
    #[test]
    fn private_relay_list_not_readable_by_others() {
        let owner = FullKeypair::generate().to_keypair();
        let other = FullKeypair::generate().to_keypair();
        let relays = [NormRelayUrl::new("wss://private-a.example.com").expect("relay")];

        let note = construct_private_relay_list_note(relays.iter(), &owner)
            .expect("private relay list note");

        let mut parsed = Vec::new();
        parse_private_relay_list_note(&note, &other, &mut parsed);
        assert!(parsed.is_empty());
    }

    /// A read-only (pubkey-only) account has no secret key, so it can neither
    /// construct nor decrypt a private relay list.
    #[test]
    fn private_relay_list_read_only_account_is_noop() {
        let owner = FullKeypair::generate().to_keypair();
        let relays = [NormRelayUrl::new("wss://private-a.example.com").expect("relay")];
        let note = construct_private_relay_list_note(relays.iter(), &owner)
            .expect("private relay list note");

        let read_only = Keypair::only_pubkey(owner.pubkey);
        assert!(construct_private_relay_list_note(relays.iter(), &read_only).is_none());

        let mut parsed = Vec::new();
        parse_private_relay_list_note(&note, &read_only, &mut parsed);
        assert!(parsed.is_empty());
    }

    /// NIP-65 relays serialize to just the `r` tag plus an optional read/write
    /// marker — no trailing entries.
    #[test]
    fn construct_nip65_relays_note_no_trailing_entries() {
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

        // read relay: marker at index 2, nothing after
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
}
