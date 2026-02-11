use crate::{
    note::NoteRef,
    notecache::{CachedNote, NoteCache},
    Result,
};

use enostr::{Filter, NoteId, Pubkey};
use nostr::RelayUrl;
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::error;

#[must_use = "process_action should be used on this result"]
pub enum SingleUnkIdAction {
    NoAction,
    NeedsProcess(UnknownId),
}

#[must_use = "process_action should be used on this result"]
pub enum NoteRefsUnkIdAction {
    NoAction,
    NeedsProcess(Vec<NoteRef>),
}

impl NoteRefsUnkIdAction {
    pub fn new(refs: Vec<NoteRef>) -> Self {
        NoteRefsUnkIdAction::NeedsProcess(refs)
    }

    pub fn no_action() -> Self {
        Self::NoAction
    }

    pub fn process_action(
        &self,
        txn: &Transaction,
        ndb: &Ndb,
        unk_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) {
        match self {
            Self::NoAction => {}
            Self::NeedsProcess(refs) => {
                UnknownIds::update_from_note_refs(txn, ndb, unk_ids, note_cache, refs);
            }
        }
    }
}

impl SingleUnkIdAction {
    pub fn new(id: UnknownId) -> Self {
        SingleUnkIdAction::NeedsProcess(id)
    }

    pub fn no_action() -> Self {
        Self::NoAction
    }

    pub fn pubkey(pubkey: Pubkey) -> Self {
        SingleUnkIdAction::new(UnknownId::Pubkey(pubkey))
    }

    pub fn note_id(note_id: NoteId) -> Self {
        SingleUnkIdAction::new(UnknownId::Id(note_id))
    }

    /// Some functions may return unknown id actions that need to be processed.
    /// For example, when we add a new account we need to make sure we have the
    /// profile for that account. This function ensures we add this to the
    /// unknown id tracker without adding side effects to functions.
    pub fn process_action(&self, ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        match self {
            Self::NeedsProcess(id) => {
                ids.add_unknown_id_if_missing(ndb, txn, id);
            }
            Self::NoAction => {}
        }
    }
}

/// Timeout before retrying hint-routed IDs via broadcast.
const HINT_RETRY_TIMEOUT: Duration = Duration::from_secs(3);

/// Grace period to wait for connecting relays before sending hint-routed requests.
/// When some hint relays are connected but others are still connecting, we wait
/// this duration to give slower relays time to establish connection.
const GRACE_PERIOD_TIMEOUT: Duration = Duration::from_millis(300);

/// Unknown Id searcher
#[derive(Default, Debug)]
pub struct UnknownIds {
    ids: HashMap<UnknownId, HashSet<RelayUrl>>,
    first_updated: Option<Instant>,
    last_updated: Option<Instant>,
    /// IDs that were sent to hint relays, tracked for delayed broadcast retry.
    /// If hint relays don't respond within HINT_RETRY_TIMEOUT, these get
    /// re-queued for broadcast to all relays.
    pending_hint_ids: HashMap<UnknownId, Instant>,
    /// IDs waiting for grace period before being sent to hint relays.
    /// These have some hint relays connecting (not yet connected), so we wait
    /// briefly to give them time to connect before sending the request.
    grace_period_ids: HashMap<UnknownId, (HashSet<RelayUrl>, Instant)>,
}

impl UnknownIds {
    /// Simple debouncer
    pub fn ready_to_send(&self) -> bool {
        if self.ids.is_empty() {
            return false;
        }

        // we trigger on first set
        if self.first_updated == self.last_updated {
            return true;
        }

        let last_updated = if let Some(last) = self.last_updated {
            last
        } else {
            // if we've
            return true;
        };

        Instant::now() - last_updated >= Duration::from_secs(2)
    }

    pub fn ids_iter(&self) -> impl ExactSizeIterator<Item = &UnknownId> {
        self.ids.keys()
    }

    pub fn ids_mut(&mut self) -> &mut HashMap<UnknownId, HashSet<RelayUrl>> {
        &mut self.ids
    }

    pub fn clear(&mut self) {
        self.ids = HashMap::default();
        self.pending_hint_ids.clear();
        self.grace_period_ids.clear();
    }

    /// Track IDs that were sent to hint relays for delayed retry.
    pub fn track_pending_hint_ids(&mut self, ids: impl IntoIterator<Item = UnknownId>) {
        let now = Instant::now();
        for id in ids {
            self.pending_hint_ids.entry(id).or_insert(now);
        }
    }

    /// Track IDs waiting for grace period before sending to hint relays.
    ///
    /// These IDs have multiple relay hints where some are connected but others
    /// are still connecting. We wait briefly to give slower relays time to connect.
    pub fn track_grace_period_ids(
        &mut self,
        ids: impl IntoIterator<Item = (UnknownId, HashSet<RelayUrl>)>,
    ) {
        let now = Instant::now();
        for (id, hints) in ids {
            self.grace_period_ids.entry(id).or_insert((hints, now));
        }
    }

    /// Check if there are IDs waiting for grace period.
    pub fn has_grace_period_ids(&self) -> bool {
        !self.grace_period_ids.is_empty()
    }

    /// Check for grace period timeouts and return IDs ready to send.
    ///
    /// Returns IDs that have waited long enough and should now be sent to
    /// their hint relays (whether or not all hints have connected).
    pub fn check_grace_period_timeouts(&mut self) -> Vec<(UnknownId, HashSet<RelayUrl>)> {
        if self.grace_period_ids.is_empty() {
            return Vec::new();
        }

        let now = Instant::now();
        let ready: Vec<_> = self
            .grace_period_ids
            .iter()
            .filter(|(_, (_, added_at))| now.duration_since(*added_at) >= GRACE_PERIOD_TIMEOUT)
            .map(|(id, (hints, _))| (*id, hints.clone()))
            .collect();

        // Remove ready IDs from grace period tracking
        for (id, _) in &ready {
            self.grace_period_ids.remove(id);
        }

        if !ready.is_empty() {
            tracing::debug!(
                "check_grace_period_timeouts: {} ids ready after grace period",
                ready.len()
            );
        }

        ready
    }

    /// Check for hint-routed IDs that timed out and need broadcast retry.
    ///
    /// Verifies each ID is still unknown (not yet in ndb) before re-queuing
    /// to avoid redundant broadcasts for already-resolved IDs.
    ///
    /// Returns true if there are IDs ready for retry, which triggers
    /// re-queuing them for broadcast to all relays.
    pub fn check_hint_timeouts(&mut self, ndb: &Ndb, txn: &Transaction) -> bool {
        if self.pending_hint_ids.is_empty() {
            return false;
        }

        let now = Instant::now();
        let timed_out: Vec<UnknownId> = self
            .pending_hint_ids
            .iter()
            .filter(|(_, sent_at)| now.duration_since(**sent_at) >= HINT_RETRY_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();

        if timed_out.is_empty() {
            return false;
        }

        let mut requeued_count = 0;
        let mut resolved_count = 0;

        // Move timed-out IDs back to main queue (without hints, forcing broadcast)
        // but only if they're still unknown
        for id in &timed_out {
            self.pending_hint_ids.remove(id);

            // Check if the ID has been resolved (note/profile now exists in ndb)
            let is_resolved = match id {
                UnknownId::Id(note_id) => ndb.get_note_by_id(txn, note_id.bytes()).is_ok(),
                UnknownId::Pubkey(pk) => ndb.get_profile_by_pubkey(txn, pk.bytes()).is_ok(),
            };

            if is_resolved {
                resolved_count += 1;
                continue;
            }

            // Still unknown: re-add with empty hints to force broadcast
            self.ids.entry(*id).or_default();
            requeued_count += 1;
        }

        if requeued_count > 0 || resolved_count > 0 {
            tracing::debug!(
                "check_hint_timeouts: {} ids re-queued for broadcast, {} already resolved",
                requeued_count,
                resolved_count
            );
        }

        if requeued_count > 0 {
            self.mark_updated();
            true
        } else {
            false
        }
    }

    /// Check if there are pending hint IDs awaiting retry.
    pub fn has_pending_hints(&self) -> bool {
        !self.pending_hint_ids.is_empty()
    }

    pub fn filter(&self) -> Option<Vec<Filter>> {
        let ids: Vec<&UnknownId> = self.ids.keys().collect();
        get_unknown_ids_filter(&ids)
    }

    /// We've updated some unknown ids, update the last_updated time to now
    pub fn mark_updated(&mut self) {
        let now = Instant::now();
        if self.first_updated.is_none() {
            self.first_updated = Some(now);
        }
        self.last_updated = Some(now);
    }

    pub fn update_from_note_key(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        key: NoteKey,
    ) -> bool {
        let note = if let Ok(note) = ndb.get_note_by_key(txn, key) {
            note
        } else {
            return false;
        };

        UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note)
    }

    /// Should be called on freshly polled notes from subscriptions
    pub fn update_from_note_refs(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        note_refs: &[NoteRef],
    ) {
        for note_ref in note_refs {
            Self::update_from_note_key(txn, ndb, unknown_ids, note_cache, note_ref.key);
        }
    }

    pub fn update_from_note(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        note: &Note,
    ) -> bool {
        let before = unknown_ids.ids_iter().len();
        let key = note.key().expect("note key");
        //let cached_note = note_cache.cached_note_or_insert(key, note).clone();
        let cached_note = note_cache.cached_note_or_insert(key, note);
        if let Err(e) = get_unknown_note_ids(ndb, cached_note, txn, note, unknown_ids.ids_mut()) {
            error!("UnknownIds::update_from_note {e}");
        }
        let after = unknown_ids.ids_iter().len();

        if before != after {
            unknown_ids.mark_updated();
            true
        } else {
            false
        }
    }

    pub fn add_unknown_id_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, unk_id: &UnknownId) {
        match unk_id {
            UnknownId::Pubkey(pk) => self.add_pubkey_if_missing(ndb, txn, pk),
            UnknownId::Id(note_id) => self.add_note_id_if_missing(ndb, txn, note_id.bytes()),
        }
    }

    pub fn add_pubkey_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, pubkey: &[u8; 32]) {
        // we already have this profile, skip
        if ndb.get_profile_by_pubkey(txn, pubkey).is_ok() {
            return;
        }

        let unknown_id = UnknownId::Pubkey(Pubkey::new(*pubkey));
        if self.ids.contains_key(&unknown_id) {
            return;
        }
        self.ids.entry(unknown_id).or_default();
        self.mark_updated();
    }

    pub fn add_note_id_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, note_id: &[u8; 32]) {
        // we already have this note, skip
        if ndb.get_note_by_id(txn, note_id).is_ok() {
            return;
        }

        let unknown_id = UnknownId::Id(NoteId::new(*note_id));
        if self.ids.contains_key(&unknown_id) {
            return;
        }
        self.ids.entry(unknown_id).or_default();
        self.mark_updated();
    }
}

#[derive(Hash, Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnknownId {
    Pubkey(Pubkey),
    Id(NoteId),
}

impl UnknownId {
    pub fn is_pubkey(&self) -> Option<&Pubkey> {
        match self {
            UnknownId::Pubkey(pk) => Some(pk),
            _ => None,
        }
    }

    pub fn is_id(&self) -> Option<&NoteId> {
        match self {
            UnknownId::Id(id) => Some(id),
            _ => None,
        }
    }
}

/// Look for missing notes in various parts of notes that we see:
///
/// - pubkeys and notes mentioned inside the note
/// - notes being replied to
///
/// We return all of this in a HashSet so that we can fetch these from
/// remote relays.
///
#[profiling::function]
pub fn get_unknown_note_ids<'a>(
    ndb: &Ndb,
    cached_note: &CachedNote,
    txn: &'a Transaction,
    note: &Note<'a>,
    ids: &mut HashMap<UnknownId, HashSet<RelayUrl>>,
) -> Result<()> {
    // the author pubkey
    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
        ids.entry(UnknownId::Pubkey(Pubkey::new(*note.pubkey())))
            .or_default();
    }

    // Collect source relays where this note was seen - useful as hints for
    // referenced notes (reactions likely came from same relay as reacted note)
    let source_relays: Vec<RelayUrl> = note
        .relays(txn)
        .filter_map(|r| RelayUrl::parse(r).ok())
        .collect();

    // pull notes that notes are replying to (including reactions via e-tags)
    // Extract relay hints from e-tags to enable hint-based routing
    if cached_note.reply.root.is_some() {
        let note_reply = cached_note.reply.borrow(note.tags());
        if let Some(root) = note_reply.root() {
            if ndb.get_note_by_id(txn, root.id).is_err() {
                let entry = ids.entry(UnknownId::Id(NoteId::new(*root.id))).or_default();
                // Pass through relay hint from e-tag if available
                if let Some(relay_str) = root.relay {
                    if let Ok(relay_url) = RelayUrl::parse(relay_str) {
                        entry.insert(relay_url);
                    }
                }
                // Also use source relays as hints (reaction likely from same relay)
                entry.extend(source_relays.iter().cloned());
            }
        }

        if !note_reply.is_reply_to_root() {
            if let Some(reply) = note_reply.reply() {
                if ndb.get_note_by_id(txn, reply.id).is_err() {
                    let entry = ids
                        .entry(UnknownId::Id(NoteId::new(*reply.id)))
                        .or_default();
                    // Pass through relay hint from e-tag if available
                    if let Some(relay_str) = reply.relay {
                        if let Ok(relay_url) = RelayUrl::parse(relay_str) {
                            entry.insert(relay_url);
                        }
                    }
                    // Also use source relays as hints
                    entry.extend(source_relays.iter().cloned());
                }
            }
        }
    }

    let blocks = ndb.get_blocks_by_key(txn, note.key().expect("note key"))?;
    for block in blocks.iter(note) {
        if block.blocktype() != BlockType::MentionBech32 {
            continue;
        }

        match block.as_mention().unwrap() {
            Mention::Pubkey(npub) => {
                if ndb.get_profile_by_pubkey(txn, npub.pubkey()).is_err() {
                    ids.entry(UnknownId::Pubkey(Pubkey::new(*npub.pubkey())))
                        .or_default();
                }
            }
            Mention::Profile(nprofile) => {
                if ndb.get_profile_by_pubkey(txn, nprofile.pubkey()).is_err() {
                    let id = UnknownId::Pubkey(Pubkey::new(*nprofile.pubkey()));
                    let relays = nprofile
                        .relays_iter()
                        .filter_map(|s| RelayUrl::parse(s).ok())
                        .collect::<HashSet<RelayUrl>>();
                    ids.entry(id).or_default().extend(relays);
                }
            }
            Mention::Event(ev) => {
                let relays = ev
                    .relays_iter()
                    .filter_map(|s| RelayUrl::parse(s).ok())
                    .collect::<HashSet<RelayUrl>>();
                match ndb.get_note_by_id(txn, ev.id()) {
                    Err(_) => {
                        ids.entry(UnknownId::Id(NoteId::new(*ev.id())))
                            .or_default()
                            .extend(relays.clone());
                        if let Some(pk) = ev.pubkey() {
                            if ndb.get_profile_by_pubkey(txn, pk).is_err() {
                                ids.entry(UnknownId::Pubkey(Pubkey::new(*pk)))
                                    .or_default()
                                    .extend(relays);
                            }
                        }
                    }
                    Ok(note) => {
                        if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                            ids.entry(UnknownId::Pubkey(Pubkey::new(*note.pubkey())))
                                .or_default()
                                .extend(relays);
                        }
                    }
                }
            }
            Mention::Note(note) => match ndb.get_note_by_id(txn, note.id()) {
                Err(_) => {
                    ids.entry(UnknownId::Id(NoteId::new(*note.id())))
                        .or_default();
                }
                Ok(note) => {
                    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                        ids.entry(UnknownId::Pubkey(Pubkey::new(*note.pubkey())))
                            .or_default();
                    }
                }
            },
            _ => {}
        }
    }

    Ok(())
}

/// Build filters for a set of unknown IDs.
///
/// Creates separate filters for pubkeys (kind:0 profiles) and note IDs.
/// Limits to 500 IDs per batch to avoid oversized requests.
fn get_unknown_ids_filter(ids: &[&UnknownId]) -> Option<Vec<Filter>> {
    if ids.is_empty() {
        return None;
    }

    let ids = &ids[0..500.min(ids.len())];
    let mut filters: Vec<Filter> = vec![];

    let pks: Vec<&[u8; 32]> = ids
        .iter()
        .flat_map(|id| id.is_pubkey().map(|pk| pk.bytes()))
        .collect();
    if !pks.is_empty() {
        let pk_filter = Filter::new().authors(pks).kinds([0]).build();
        filters.push(pk_filter);
    }

    let note_ids: Vec<&[u8; 32]> = ids
        .iter()
        .flat_map(|id| id.is_id().map(|id| id.bytes()))
        .collect();
    if !note_ids.is_empty() {
        filters.push(Filter::new().ids(note_ids).build());
    }

    Some(filters)
}

/// Send subscription requests to fetch unknown IDs from relays.
///
/// Uses hint-based routing when relay hints are available (from NIP-19
/// nevent/nprofile bech32 strings). IDs with hints are fetched from their
/// hint relays first. IDs without hints are broadcast to all connected relays.
///
/// If hint relays aren't in the pool, falls back to broadcasting those IDs
/// to all relays to avoid dropping them.
///
/// IDs sent to hint relays are tracked for delayed retry - if not resolved
/// within HINT_RETRY_TIMEOUT, they'll be re-queued for broadcast.
#[profiling::function]
pub fn unknown_id_send(unknown_ids: &mut UnknownIds, pool: &mut enostr::RelayPool) {
    let total_count = unknown_ids.ids_iter().len();
    if total_count == 0 {
        return;
    }

    tracing::debug!("unknown_id_send: processing {} unknown ids", total_count);

    // Get the set of relay URLs currently in the pool for fallback checking
    let pool_urls = pool.urls();

    // Partition IDs into those with hints and those without
    let ids_map = std::mem::take(unknown_ids.ids_mut());

    // Collect IDs to broadcast (no hints, or hints not in pool)
    let mut ids_to_broadcast: Vec<UnknownId> = Vec::new();

    // Group IDs by relay hint for efficient batching (only for hints in pool)
    let mut relay_to_ids: HashMap<RelayUrl, Vec<UnknownId>> = HashMap::new();

    // Track all IDs sent to hint relays for delayed retry
    let mut hint_routed_ids: HashSet<UnknownId> = HashSet::new();

    // Track IDs that need grace period (some hints connected, others connecting)
    let mut grace_period_ids: Vec<(UnknownId, HashSet<RelayUrl>)> = Vec::new();

    for (id, hints) in ids_map {
        // No hints: broadcast
        if hints.is_empty() {
            ids_to_broadcast.push(id);
            continue;
        }

        // Check if any hint relay is in the pool (normalize for comparison)
        let hints_in_pool: Vec<_> = hints
            .iter()
            .filter(|url| {
                let normalized = enostr::RelayPool::canonicalize_url(url.to_string());
                pool_urls.contains(&normalized)
            })
            .collect();

        // None of the hint relays are in the pool: fall back to broadcast
        if hints_in_pool.is_empty() {
            tracing::debug!(
                "unknown_id_send: hint relays not in pool for {:?}, falling back to broadcast",
                id
            );
            ids_to_broadcast.push(id);
            continue;
        }

        // Check if any hint relays are still connecting (grace period logic)
        // If some are connected but others are connecting, wait for grace period
        let has_connecting = pool.has_connecting_relays(hints_in_pool.iter().map(|u| u.as_str()));
        let has_connected = hints_in_pool.iter().any(|url| {
            matches!(
                pool.relay_status(url.as_str()),
                Some(enostr::RelayStatus::Connected)
            )
        });

        if has_connecting && has_connected {
            // Some connected, some connecting: defer with grace period
            tracing::debug!(
                "unknown_id_send: deferring {:?} for grace period (some hints still connecting)",
                id
            );
            grace_period_ids.push((id, hints));
            continue;
        }

        // All hint relays are ready (or none are connecting): send immediately
        hint_routed_ids.insert(id);
        for relay_url in hints_in_pool {
            relay_to_ids.entry(relay_url.clone()).or_default().push(id);
        }
    }

    // Track grace period IDs for later processing
    if !grace_period_ids.is_empty() {
        tracing::debug!(
            "unknown_id_send: tracking {} ids for grace period",
            grace_period_ids.len()
        );
        unknown_ids.track_grace_period_ids(grace_period_ids);
    }

    // Handle IDs to broadcast (no hints or hints not in pool)
    if !ids_to_broadcast.is_empty() {
        let ids_refs: Vec<&UnknownId> = ids_to_broadcast.iter().collect();
        if let Some(filter) = get_unknown_ids_filter(&ids_refs) {
            tracing::debug!(
                "unknown_id_send: broadcasting {} ids",
                ids_to_broadcast.len()
            );
            pool.subscribe("unknownids".to_string(), filter);
        }
    }

    // Handle IDs with hints in pool: send to specific hint relays
    for (relay_url, ids) in relay_to_ids {
        let ids_refs: Vec<&UnknownId> = ids.iter().collect();
        let Some(filter) = get_unknown_ids_filter(&ids_refs) else {
            continue;
        };

        tracing::debug!(
            "unknown_id_send: sending {} ids to hint relay {}",
            ids.len(),
            relay_url
        );

        pool.subscribe_to(
            format!("unknownids-{}", relay_url.as_str()),
            filter,
            [relay_url.as_str()],
        );
    }

    // Track hint-routed IDs for delayed retry
    if !hint_routed_ids.is_empty() {
        tracing::debug!(
            "unknown_id_send: tracking {} hint-routed ids for delayed retry",
            hint_routed_ids.len()
        );
        unknown_ids.track_pending_hint_ids(hint_routed_ids);
    }
}

/// Send grace period IDs that have waited long enough.
///
/// Called after grace period timeout to send IDs to their hint relays,
/// whether or not all hints have connected. This ensures we don't wait
/// indefinitely for slow relays.
#[profiling::function]
pub fn send_grace_period_ids(unknown_ids: &mut UnknownIds, pool: &mut enostr::RelayPool) {
    let ready_ids = unknown_ids.check_grace_period_timeouts();
    if ready_ids.is_empty() {
        return;
    }

    let pool_urls = pool.urls();
    let mut relay_to_ids: HashMap<RelayUrl, Vec<UnknownId>> = HashMap::new();
    let mut hint_routed_ids: HashSet<UnknownId> = HashSet::new();

    for (id, hints) in ready_ids {
        // Filter to hints that are in the pool
        let hints_in_pool: Vec<_> = hints
            .iter()
            .filter(|url| {
                let normalized = enostr::RelayPool::canonicalize_url(url.to_string());
                pool_urls.contains(&normalized)
            })
            .collect();

        if hints_in_pool.is_empty() {
            // All hints disconnected during grace period, skip (will be caught by hint timeout)
            continue;
        }

        hint_routed_ids.insert(id);
        for relay_url in hints_in_pool {
            relay_to_ids.entry(relay_url.clone()).or_default().push(id);
        }
    }

    // Send to hint relays
    for (relay_url, ids) in relay_to_ids {
        let ids_refs: Vec<&UnknownId> = ids.iter().collect();
        let Some(filter) = get_unknown_ids_filter(&ids_refs) else {
            continue;
        };

        tracing::debug!(
            "send_grace_period_ids: sending {} ids to hint relay {} after grace period",
            ids.len(),
            relay_url
        );

        pool.subscribe_to(
            format!("unknownids-{}", relay_url.as_str()),
            filter,
            [relay_url.as_str()],
        );
    }

    // Track for delayed broadcast retry
    if !hint_routed_ids.is_empty() {
        unknown_ids.track_pending_hint_ids(hint_routed_ids);
    }
}
