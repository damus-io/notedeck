use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{relayspec::relays_from_nip65_note, AccountData, RelaySpec, RemoteApi};
use enostr::{Keypair, NormRelayUrl, RelayId};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, Note, NoteBuilder, NoteKey, Subscription, Transaction};
use tracing::{debug, error, info};

const RELAY_LIST_POLL_LIMIT: u32 = 64;

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
    ndb_advertised_created_at: Option<u64>,
    pending_advertised: Option<RelayListProjection>,
}

#[derive(Clone)]
pub(crate) struct RelayListProjection {
    pub created_at: u64,
    pub advertised: BTreeSet<RelaySpec>,
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
            ndb_advertised_created_at: None,
            pending_advertised: None,
        }
    }

    pub fn query(&mut self, ndb: &Ndb, txn: &Transaction, keypair: &Keypair) {
        let projection = self.newest_nip65_projection(ndb, txn);
        self.apply_nip65_projection(projection);
        debug!("initial relays {:?}", self.advertised);
        self.private = self.query_private_relays(ndb, txn, keypair);
    }

    fn newest_nip65_projection(&self, ndb: &Ndb, txn: &Transaction) -> Option<RelayListProjection> {
        let lim = self
            .filter
            .limit()
            .unwrap_or(crate::filter::default_limit()) as i32;
        let results = ndb
            .query(txn, std::slice::from_ref(&self.filter), lim)
            .expect("query user relays results");

        for result in results {
            let Ok(note) = ndb.get_note_by_key(txn, result.note_key) else {
                continue;
            };
            return Some(Self::projection_from_nip65_note(&note));
        }

        None
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

    pub(crate) fn newest_effective_nip65_relays(
        &self,
        ndb: &Ndb,
        txn: &Transaction,
    ) -> Option<(u64, BTreeSet<RelaySpec>)> {
        let ndb_projection = self.newest_nip65_projection(ndb, txn);
        Self::effective_projection(self.pending_advertised.as_ref(), ndb_projection.as_ref())
            .map(|projection| (projection.created_at, projection.advertised))
    }

    pub(crate) fn accept_local_relay_list(&mut self, projection: RelayListProjection) {
        self.advertised = projection.advertised.clone();
        self.pending_advertised = Some(projection);
    }

    fn effective_projection(
        pending: Option<&RelayListProjection>,
        ndb: Option<&RelayListProjection>,
    ) -> Option<RelayListProjection> {
        match (pending, ndb) {
            (Some(pending), Some(ndb)) if pending.created_at >= ndb.created_at => {
                Some(pending.clone())
            }
            (Some(_), Some(ndb)) => Some(ndb.clone()),
            (Some(pending), None) => Some(pending.clone()),
            (None, Some(ndb)) => Some(ndb.clone()),
            (None, None) => None,
        }
    }

    fn apply_nip65_projection(&mut self, ndb_projection: Option<RelayListProjection>) -> bool {
        let old = self.advertised.clone();

        match ndb_projection {
            Some(projection) => {
                if self
                    .ndb_advertised_created_at
                    .is_some_and(|current| projection.created_at < current)
                {
                    return false;
                }

                if self.pending_advertised.as_ref().is_some_and(|pending| {
                    projection.created_at > pending.created_at
                        || (projection.created_at == pending.created_at
                            && projection.advertised == pending.advertised)
                }) {
                    self.pending_advertised = None;
                }

                self.ndb_advertised_created_at = Some(projection.created_at);
                if self
                    .pending_advertised
                    .as_ref()
                    .is_some_and(|pending| pending.created_at >= projection.created_at)
                {
                    self.advertised = self
                        .pending_advertised
                        .as_ref()
                        .expect("pending projection")
                        .advertised
                        .clone();
                } else {
                    self.advertised = projection.advertised;
                }
            }
            None => {
                self.ndb_advertised_created_at = None;
                if let Some(pending) = &self.pending_advertised {
                    self.advertised = pending.advertised.clone();
                } else {
                    self.advertised.clear();
                }
            }
        }

        self.advertised != old
    }

    fn projection_from_nip65_note(note: &Note<'_>) -> RelayListProjection {
        RelayListProjection {
            created_at: note.created_at(),
            advertised: relays_from_nip65_note(note).into_iter().collect(),
        }
    }

    /// Drain relay-list poll hits and rebuild advertised relays from local NDB.
    ///
    /// Returns the previous relay data only when the effective advertised relay
    /// projection changed. Idle frames do not clone relay state.
    #[profiling::function]
    pub fn poll_for_updates(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        sub: Subscription,
    ) -> Option<Self> {
        let nks = ndb.poll_for_notes(sub, RELAY_LIST_POLL_LIMIT);

        if nks.is_empty() {
            return None;
        }

        let mut newest = None;
        for nk in nks {
            let Ok(note) = ndb.get_note_by_key(txn, nk) else {
                continue;
            };
            let projection = Self::projection_from_nip65_note(&note);
            if newest.as_ref().is_none_or(|current: &RelayListProjection| {
                projection.created_at >= current.created_at
            }) {
                newest = Some(projection);
            }
        }

        let newest = newest?;

        let previous = self.clone();
        let changed = self.apply_nip65_projection(Some(newest));
        debug!("updated relays {:?}", self.advertised);
        changed.then_some(previous)
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
/// marker -- they live in a dedicated kind-10013 event whose `.content` is the
/// NIP-44 self-encrypted (encrypted to the author's own pubkey) JSON array of
/// `["relay", url]` private tags. This keeps the private set off the public
/// relay list, is only decryptable by the author, and is the same event
/// Amethyst uses, so the private relay set round-trips across clients.
pub const PRIVATE_RELAY_LIST_KIND: u32 = 10013;

/// NIP-44 self-encrypt `plaintext` to the keypair's own pubkey. Returns `None`
/// for a read-only (pubkey-only) account -- it has no secret key to encrypt with.
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

/// Current Unix timestamp used for locally signed relay-list replacements.
fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
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

    fn is_private(&self) -> bool {
        matches!(
            self,
            RelayAction::AddPrivate(_) | RelayAction::RemovePrivate(_)
        )
    }
}

fn apply_relay_action(
    advertised: &mut BTreeSet<RelaySpec>,
    action: &RelayAction,
    relay_url: NormRelayUrl,
) {
    match action {
        RelayAction::Add(_) => {
            advertised.insert(RelaySpec::new(relay_url, false, false));
        }
        RelayAction::Remove(_) => {
            advertised.remove(&RelaySpec::new(relay_url, false, false));
        }
        RelayAction::AddPrivate(_) | RelayAction::RemovePrivate(_) => unreachable!(),
    }
}

fn normalize_relay_action(action: &RelayAction) -> Option<NormRelayUrl> {
    let Ok(relay_url) = NormRelayUrl::new(action.get_url()) else {
        return None;
    };

    let relay_url_str = relay_url.to_string();
    match action {
        RelayAction::Add(_) => info!("add advertised relay \"{relay_url_str}\""),
        RelayAction::Remove(_) => info!("remove advertised relay \"{relay_url_str}\""),
        RelayAction::AddPrivate(_) => info!("add private relay \"{relay_url_str}\""),
        RelayAction::RemovePrivate(_) => info!("remove private relay \"{relay_url_str}\""),
    }

    Some(relay_url)
}

/// Apply a local advertised-relay edit without creating a signed relay-list note.
pub(super) fn apply_local_advertised_relay_action(
    action: RelayAction,
    relay_defaults: &RelayDefaults,
    relay_data: &mut AccountRelayData,
) -> bool {
    if action.is_private() {
        return false;
    }

    let old = relay_data.advertised.clone();
    let Some(relay_url) = normalize_relay_action(&action) else {
        return false;
    };

    if relay_data.advertised.is_empty() {
        relay_data
            .advertised
            .extend(relay_defaults.bootstrap_relays.iter().cloned());
    }

    apply_relay_action(&mut relay_data.advertised, &action, relay_url);
    relay_data.pending_advertised = None;

    relay_data.advertised != old
}

/// Locally-authored relay-list edit ready for NDB ingest and broadcast.
pub(super) struct AdvertisedRelayEdit {
    pub note: Note<'static>,
    pub projection: RelayListProjection,
    pub write_relays: Vec<RelayId>,
}

/// Computes write relay targets for a specific advertised relay-list projection.
fn write_relays_for_advertised(
    relay_defaults: &RelayDefaults,
    relay_data: &AccountRelayData,
    advertised: BTreeSet<RelaySpec>,
) -> Vec<RelayId> {
    let mut projected_data = relay_data.clone();
    projected_data.advertised = advertised;
    write_relays(relay_defaults, &projected_data)
}

/// Merges old and new write targets into a stable relay publish order.
fn merge_write_relays(
    old_write_relays: impl IntoIterator<Item = RelayId>,
    new_write_relays: impl IntoIterator<Item = RelayId>,
) -> Vec<RelayId> {
    let mut websocket_relays = BTreeSet::new();
    let mut has_multicast = false;

    for relay in old_write_relays.into_iter().chain(new_write_relays) {
        match relay {
            RelayId::Websocket(url) => {
                websocket_relays.insert(url);
            }
            RelayId::Multicast => {
                has_multicast = true;
            }
        }
    }

    let mut relays = websocket_relays
        .into_iter()
        .map(RelayId::Websocket)
        .collect::<Vec<_>>();
    if has_multicast {
        relays.push(RelayId::Multicast);
    }

    relays
}

/// Computes a signed NIP-65 relay-list edit without mutating account state.
pub(super) fn modify_advertised_relays(
    kp: &Keypair,
    action: RelayAction,
    relay_defaults: &RelayDefaults,
    relay_data: &AccountRelayData,
    base_advertised: &BTreeSet<RelaySpec>,
    bootstrap_if_empty: bool,
    created_after: Option<u64>,
) -> Option<AdvertisedRelayEdit> {
    if action.is_private() {
        return None;
    }

    let secretkey = kp.secret_key.as_ref()?;
    let relay_url = normalize_relay_action(&action)?;

    let old_write_relays =
        write_relays_for_advertised(relay_defaults, relay_data, base_advertised.clone());
    let mut advertised = base_advertised.clone();
    if advertised.is_empty() && bootstrap_if_empty {
        // If there is no local relay-list event and the selected account
        // has no advertised relays, initialize with the bootstrapping set.
        advertised.extend(relay_defaults.bootstrap_relays.iter().cloned());
    }
    apply_relay_action(&mut advertised, &action, relay_url);

    let secret_bytes = secretkey.to_secret_bytes();
    let created_at = created_after
        .map(|created_at| created_at.saturating_add(1))
        .unwrap_or(0)
        .max(current_unix_timestamp());
    let note_builder = construct_nip65_relays_note(&advertised).created_at(created_at);
    let note = note_builder
        .sign(&secret_bytes)
        .build()
        .expect("note build");
    let advertised: BTreeSet<RelaySpec> = relays_from_nip65_note(&note).into_iter().collect();
    let new_write_relays =
        write_relays_for_advertised(relay_defaults, relay_data, advertised.clone());
    let write_relays = merge_write_relays(old_write_relays, new_write_relays);

    let projection = RelayListProjection {
        created_at: note.created_at(),
        advertised,
    };

    Some(AdvertisedRelayEdit {
        note,
        projection,
        write_relays,
    })
}

/// Apply an `AddPrivate`/`RemovePrivate` action: mutate the in-memory private
/// relay set and republish the kind-10013 NIP-37 list to the account's NIP-65
/// write relays and the private relays themselves.
pub(super) fn modify_private_relays(
    kp: &Keypair,
    action: RelayAction,
    remote: &mut RemoteApi<'_>,
    relay_defaults: &RelayDefaults,
    account_data: &mut AccountData,
) {
    let Some(relay_url) = normalize_relay_action(&action) else {
        return;
    };

    let private = &mut account_data.relay.private;
    match action {
        RelayAction::AddPrivate(_) => {
            private.insert(relay_url);
        }
        RelayAction::RemovePrivate(_) => {
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
    use std::collections::BTreeSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        construct_nip65_relays_note, construct_private_relay_list_note, modify_advertised_relays,
        parse_private_relay_list_note, AccountRelayData, RelayAction, RelayDefaults,
        RelayListProjection, PRIVATE_RELAY_LIST_KIND,
    };
    use crate::RelaySpec;
    use enostr::{FullKeypair, Keypair, NormRelayUrl, RelayId};

    fn relay_spec(url: &str) -> RelaySpec {
        RelaySpec::new(NormRelayUrl::new(url).expect("relay url"), false, false)
    }

    fn relay_projection(
        created_at: u64,
        relays: impl IntoIterator<Item = RelaySpec>,
    ) -> RelayListProjection {
        RelayListProjection {
            created_at,
            advertised: relays.into_iter().collect(),
        }
    }

    fn relay_defaults(bootstrap_relays: Vec<RelaySpec>) -> RelayDefaults {
        RelayDefaults {
            forced_relays: BTreeSet::new(),
            bootstrap_relays: bootstrap_relays.into_iter().collect(),
        }
    }

    fn websocket_targets(relays: &[RelayId]) -> BTreeSet<NormRelayUrl> {
        relays
            .iter()
            .filter_map(|relay| match relay {
                RelayId::Websocket(url) => Some(url.clone()),
                RelayId::Multicast => None,
            })
            .collect()
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_secs()
    }

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
    /// marker -- no trailing entries.
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

        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-read.example.com/")
                && tag.get_str(2) == Some("read")
                && tag.get(3).is_none()
        }));
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("r")
                && tag.get_str(1) == Some("wss://relay-both.example.com/")
                && tag.get(2).is_none()
        }));
    }

    #[test]
    fn apply_nip65_projection_keeps_same_second_mismatched_pending_advertised() {
        let account = FullKeypair::generate();
        let created_at = 42;
        let relay_a = relay_spec("wss://relay-a.example.com");
        let relay_b = relay_spec("wss://relay-b.example.com");
        let projection = relay_projection(created_at, [relay_a.clone(), relay_b]);
        let mut relay_data = AccountRelayData::new(account.pubkey.bytes());

        relay_data.accept_local_relay_list(projection);
        let changed =
            relay_data.apply_nip65_projection(Some(relay_projection(created_at, [relay_a])));

        assert!(
            !changed,
            "same-second mismatched NDB projection must not replace pending advertised relays"
        );
        let pending = relay_data
            .pending_advertised
            .as_ref()
            .expect("same-second mismatch remains pending");
        assert_eq!(pending.created_at, created_at);
        assert_eq!(relay_data.advertised, pending.advertised);
        assert_eq!(relay_data.ndb_advertised_created_at, Some(created_at));
    }

    #[test]
    fn apply_nip65_projection_clears_same_second_matching_pending_advertised() {
        let account = FullKeypair::generate();
        let created_at = 42;
        let relay_a = relay_spec("wss://relay-a.example.com");
        let relay_b = relay_spec("wss://relay-b.example.com");
        let advertised: BTreeSet<_> = [relay_a, relay_b].into_iter().collect();
        let projection = RelayListProjection {
            created_at,
            advertised,
        };
        let ndb_projection = projection.clone();
        let mut relay_data = AccountRelayData::new(account.pubkey.bytes());

        relay_data.accept_local_relay_list(projection);
        let changed = relay_data.apply_nip65_projection(Some(ndb_projection.clone()));

        assert!(!changed);
        assert_eq!(relay_data.advertised, ndb_projection.advertised);
        assert!(
            relay_data.pending_advertised.is_none(),
            "same-second matching NDB projection confirms pending advertised relays"
        );
        assert_eq!(relay_data.ndb_advertised_created_at, Some(created_at));
    }

    #[test]
    fn apply_nip65_projection_ignores_older_confirmed_projection() {
        let account = FullKeypair::generate();
        let relay_new = relay_spec("wss://relay-newer.example.com");
        let relay_old = relay_spec("wss://relay-older.example.com");
        let mut relay_data = AccountRelayData::new(account.pubkey.bytes());

        assert!(relay_data.apply_nip65_projection(Some(relay_projection(20, [relay_new.clone()]))));
        let changed = relay_data.apply_nip65_projection(Some(relay_projection(10, [relay_old])));

        assert!(!changed);
        assert_eq!(relay_data.advertised, [relay_new].into_iter().collect());
        assert_eq!(relay_data.ndb_advertised_created_at, Some(20));
    }

    #[test]
    fn modify_advertised_relays_remove_targets_removed_write_relay() {
        let account = FullKeypair::generate();
        let keypair = account.clone().to_keypair();
        let relay_defaults = relay_defaults(Vec::new());
        let relay_a = relay_spec("wss://relay-remove-a.example.com");
        let relay_b = relay_spec("wss://relay-remove-b.example.com");
        let base_advertised = [relay_a.clone(), relay_b.clone()].into_iter().collect();
        let mut relay_data = AccountRelayData::new(account.pubkey.bytes());
        relay_data.advertised = base_advertised;

        let edit = modify_advertised_relays(
            &keypair,
            RelayAction::Remove(relay_b.url.to_string()),
            &relay_defaults,
            &relay_data,
            &relay_data.advertised,
            false,
            None,
        )
        .expect("relay edit");

        assert_eq!(
            edit.write_relays,
            vec![
                RelayId::Websocket(relay_a.url.clone()),
                RelayId::Websocket(relay_b.url.clone()),
                RelayId::Multicast,
            ]
        );
        let write_targets = websocket_targets(&edit.write_relays);
        assert!(write_targets.contains(&relay_a.url));
        assert!(
            write_targets.contains(&relay_b.url),
            "removed writable relay must receive the replacement kind-10002"
        );
    }

    #[test]
    fn modify_advertised_relays_add_targets_pre_edit_bootstrap_and_new_write_relay() {
        let account = FullKeypair::generate();
        let keypair = account.clone().to_keypair();
        let bootstrap_a = relay_spec("wss://relay-bootstrap-a.example.com");
        let bootstrap_b = relay_spec("wss://relay-bootstrap-b.example.com");
        let relay_defaults = relay_defaults(vec![bootstrap_a.clone(), bootstrap_b.clone()]);
        let relay_c = relay_spec("wss://relay-c-add.example.com");
        let relay_data = AccountRelayData::new(account.pubkey.bytes());

        let edit = modify_advertised_relays(
            &keypair,
            RelayAction::Add(relay_c.url.to_string()),
            &relay_defaults,
            &relay_data,
            &relay_data.advertised,
            false,
            None,
        )
        .expect("relay edit");

        assert_eq!(
            edit.write_relays,
            vec![
                RelayId::Websocket(bootstrap_a.url.clone()),
                RelayId::Websocket(bootstrap_b.url.clone()),
                RelayId::Websocket(relay_c.url.clone()),
                RelayId::Multicast,
            ]
        );
        let write_targets = websocket_targets(&edit.write_relays);
        assert!(
            write_targets.contains(&bootstrap_a.url) && write_targets.contains(&bootstrap_b.url),
            "old empty effective write set should route through bootstrap relays"
        );
        assert!(
            write_targets.contains(&relay_c.url),
            "new writable relay must receive the replacement kind-10002"
        );
        assert_eq!(
            edit.projection.advertised,
            std::iter::once(relay_c).collect(),
            "bootstrap relays are publish targets only, not signed relay-list entries"
        );
    }

    #[test]
    fn modify_advertised_relays_uses_current_time_for_stale_replacement() {
        let account = FullKeypair::generate();
        let keypair = account.clone().to_keypair();
        let relay_defaults = relay_defaults(Vec::new());
        let relay_a = relay_spec("wss://relay-stale-replacement-a.example.com");
        let relay_b = relay_spec("wss://relay-stale-replacement-b.example.com");
        let base_advertised = std::iter::once(relay_a).collect::<BTreeSet<_>>();
        let mut relay_data = AccountRelayData::new(account.pubkey.bytes());
        relay_data.advertised = base_advertised;
        let stale_created_at = 1;
        let before_edit = now_secs();

        let edit = modify_advertised_relays(
            &keypair,
            RelayAction::Add(relay_b.url.to_string()),
            &relay_defaults,
            &relay_data,
            &relay_data.advertised,
            false,
            Some(stale_created_at),
        )
        .expect("relay edit");

        assert!(
            edit.note.created_at() >= before_edit,
            "new user intent must not be backdated to stale_created_at + 1"
        );
        assert!(
            edit.note.created_at() > stale_created_at.saturating_add(1),
            "stale local projection should not force a near-past replacement timestamp"
        );
    }
}
