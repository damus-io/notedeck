use crate::{
    note::NoteRef,
    notecache::{CachedNote, NoteCache},
    oneshot_api::OneshotRemoteAdvertisedCoverage,
    OneshotApi, Result,
};

use enostr::{Filter, NormRelayUrl, NoteId, Pubkey};
use nostr::RelayUrl;
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::error;

const UNKNOWN_ID_BATCH_SIZE: usize = 500;
const UNKNOWN_ID_SEND_DELAYS: [Duration; 5] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
const UNKNOWN_ID_RETRY_AFTER: Duration = Duration::from_secs(30);
const UNKNOWN_ID_SENT_TTL: Duration = Duration::from_secs(10 * 60);
const UNKNOWN_ID_SENT_PRUNE_INTERVAL: Duration = Duration::from_secs(60);

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

/// Unknown Id searcher
#[derive(Default, Debug)]
pub struct UnknownIds {
    ids: HashMap<UnknownId, HashSet<RelayUrl>>,
    sent: HashMap<UnknownId, SentUnknownId>,
    send_pacing: UnknownIdSendPacing,
    last_sent_prune: Option<Instant>,
}

#[derive(Debug, Clone)]
struct SentUnknownId {
    sent_at: Instant,
    remote_advertised_relays: HashMap<RelayUrl, Instant>,
}

impl SentUnknownId {
    fn new(sent_at: Instant) -> Self {
        Self {
            sent_at,
            remote_advertised_relays: HashMap::new(),
        }
    }

    fn remote_advertised_relay_due(&self, relay: &RelayUrl, now: Instant) -> bool {
        self.remote_advertised_relays
            .get(relay)
            .is_none_or(|sent_at| elapsed_since(now, *sent_at) >= UNKNOWN_ID_RETRY_AFTER)
    }

    fn mark_remote_advertised_relays_sent(
        &mut self,
        relays: impl IntoIterator<Item = RelayUrl>,
        sent_at: Instant,
    ) {
        self.remote_advertised_relays
            .extend(relays.into_iter().map(|relay| (relay, sent_at)));
    }

    fn prune_at(&mut self, now: Instant) {
        self.remote_advertised_relays
            .retain(|_, sent_at| elapsed_since(now, *sent_at) < UNKNOWN_ID_SENT_TTL);
    }

    fn should_retain(&self, now: Instant) -> bool {
        elapsed_since(now, self.sent_at) < UNKNOWN_ID_SENT_TTL
            || !self.remote_advertised_relays.is_empty()
    }
}

#[derive(Debug)]
struct UnknownIdFetchBatch {
    sent_at: Instant,
    filters: Vec<Filter>,
    items: Vec<UnknownIdFetchItem>,
}

#[derive(Debug)]
struct UnknownIdFetchItem {
    id: UnknownId,
    relays: HashSet<RelayUrl>,
}

#[derive(Default, Debug)]
struct UnknownIdSendPacing {
    next_send_at: Option<Instant>,
    delay_index: usize,
}

impl UnknownIdSendPacing {
    fn is_idle(&self) -> bool {
        self.next_send_at.is_none()
    }

    fn is_due(&self, now: Instant) -> bool {
        self.next_send_at
            .is_some_and(|next_send_at| now >= next_send_at)
    }

    fn schedule_after_send(&mut self, now: Instant) {
        let delay = UNKNOWN_ID_SEND_DELAYS[self.delay_index];
        self.next_send_at = Some(now + delay);
        self.delay_index = (self.delay_index + 1).min(UNKNOWN_ID_SEND_DELAYS.len() - 1);
    }

    fn reset(&mut self) {
        self.next_send_at = None;
        self.delay_index = 0;
    }
}

impl UnknownIds {
    pub fn ids_iter(&self) -> impl ExactSizeIterator<Item = &UnknownId> {
        self.ids.keys()
    }

    pub fn clear(&mut self) {
        self.ids = HashMap::default();
        self.sent = HashMap::default();
        self.send_pacing.reset();
        self.last_sent_prune = None;
    }

    fn drain_ready_fetch_batch(&mut self) -> Option<UnknownIdFetchBatch> {
        self.drain_ready_fetch_batch_at(Instant::now())
    }

    fn drain_ready_fetch_batch_at(&mut self, now: Instant) -> Option<UnknownIdFetchBatch> {
        self.reset_elapsed_idle_pacing(now);

        if self.ids.is_empty() {
            return None;
        }

        if self.ids.len() < UNKNOWN_ID_BATCH_SIZE
            && !self.send_pacing.is_idle()
            && !self.send_pacing.is_due(now)
        {
            return None;
        }

        let batch = self.drain_fetch_batch_at(now)?;
        self.send_pacing.schedule_after_send(now);
        Some(batch)
    }

    fn drain_fetch_batch_at(&mut self, now: Instant) -> Option<UnknownIdFetchBatch> {
        let items = self
            .ids
            .iter()
            .take(UNKNOWN_ID_BATCH_SIZE)
            .map(|(id, relays)| UnknownIdFetchItem {
                id: *id,
                relays: relays.clone(),
            })
            .collect::<Vec<_>>();
        let filter_ids = items.iter().map(|item| &item.id).collect::<Vec<_>>();
        let filters = get_unknown_ids_filter(&filter_ids)?;

        for item in &items {
            self.ids.remove(&item.id);
            self.sent
                .entry(item.id)
                .and_modify(|sent| {
                    sent.sent_at = now;
                })
                .or_insert_with(|| SentUnknownId::new(now));
        }

        Some(UnknownIdFetchBatch {
            sent_at: now,
            filters,
            items,
        })
    }

    fn mark_remote_advertised_fetch_coverage_sent(&mut self, batch: &UnknownIdFetchBatch) {
        for item in &batch.items {
            let Some(sent) = self.sent.get_mut(&item.id) else {
                continue;
            };
            sent.mark_remote_advertised_relays_sent(item.relays.iter().cloned(), batch.sent_at);
        }
    }

    fn add_missing_unknown_id(&mut self, id: UnknownId, relays: HashSet<RelayUrl>) -> bool {
        self.add_missing_unknown_id_at(id, relays, Instant::now())
    }

    fn add_missing_unknown_id_at(
        &mut self,
        id: UnknownId,
        relays: HashSet<RelayUrl>,
        now: Instant,
    ) -> bool {
        self.reset_elapsed_idle_pacing(now);
        self.prune_sent_at(now);

        if let Some(existing_relays) = self.ids.get_mut(&id) {
            existing_relays.extend(relays);
            return false;
        }

        if let Some(sent) = self.sent.get(&id) {
            let elapsed = elapsed_since(now, sent.sent_at);
            if elapsed < UNKNOWN_ID_RETRY_AFTER {
                let relays = relays
                    .into_iter()
                    .filter(|relay| sent.remote_advertised_relay_due(relay, now))
                    .collect::<HashSet<_>>();
                if !relays.is_empty() {
                    self.ids.entry(id).or_default().extend(relays);
                    return true;
                }
                return false;
            }
        }

        self.ids.entry(id).or_default().extend(relays);
        true
    }

    fn remove_unknown_id(&mut self, id: &UnknownId) {
        self.ids.remove(id);
        self.sent.remove(id);
    }

    fn prune_sent_at(&mut self, now: Instant) {
        if self
            .last_sent_prune
            .is_some_and(|last| elapsed_since(now, last) < UNKNOWN_ID_SENT_PRUNE_INTERVAL)
        {
            return;
        }

        self.sent.retain(|_, sent| {
            sent.prune_at(now);
            sent.should_retain(now)
        });
        self.last_sent_prune = Some(now);
    }

    fn reset_elapsed_idle_pacing(&mut self, now: Instant) {
        if self.ids.is_empty() && self.send_pacing.is_due(now) {
            self.send_pacing.reset();
        }
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

    #[profiling::function]
    pub fn update_from_note(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        note: &Note,
    ) -> bool {
        let before = unknown_ids.ids_iter().len();
        let key = note.key().expect("note key");
        let cached_note = note_cache.cached_note_or_insert(key, note);
        if let Err(e) = get_unknown_note_ids(ndb, cached_note, txn, note, unknown_ids) {
            error!("UnknownIds::update_from_note {e}");
        }
        let after = unknown_ids.ids_iter().len();

        before != after
    }

    pub fn add_unknown_id_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, unk_id: &UnknownId) {
        match unk_id {
            UnknownId::Pubkey(pk) => self.add_pubkey_if_missing(ndb, txn, pk),
            UnknownId::Id(note_id) => self.add_note_id_if_missing(ndb, txn, note_id.bytes()),
        }
    }

    pub fn add_pubkey_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, pubkey: &[u8; 32]) {
        self.add_pubkey_if_missing_with_relays(ndb, txn, pubkey, HashSet::default());
    }

    /// Queue the note author profile with relay coverage from where the note was observed.
    pub fn add_note_author_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, note: &Note<'_>) {
        self.add_pubkey_if_missing_with_relays(
            ndb,
            txn,
            note.pubkey(),
            relay_urls_for_note(note, txn),
        );
    }

    /// Queue a profile with relay coverage from an already-loaded source note.
    pub fn add_pubkey_if_missing_from_note(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        pubkey: &[u8; 32],
        source_note: &Note<'_>,
    ) {
        self.add_pubkey_if_missing_with_relays(
            ndb,
            txn,
            pubkey,
            relay_urls_for_note(source_note, txn),
        );
    }

    fn add_pubkey_if_missing_with_relays(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        pubkey: &[u8; 32],
        relays: HashSet<RelayUrl>,
    ) {
        let unknown_id = UnknownId::Pubkey(Pubkey::new(*pubkey));

        // we already have this profile, skip
        if ndb.get_profile_by_pubkey(txn, pubkey).is_ok() {
            self.remove_unknown_id(&unknown_id);
            return;
        }

        self.add_missing_unknown_id(unknown_id, relays);
    }

    pub fn add_note_id_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, note_id: &[u8; 32]) {
        let unknown_id = UnknownId::Id(NoteId::new(*note_id));

        // we already have this note, skip
        if ndb.get_note_by_id(txn, note_id).is_ok() {
            self.remove_unknown_id(&unknown_id);
            return;
        }

        self.add_missing_unknown_id(unknown_id, HashSet::default());
    }
}

fn elapsed_since(now: Instant, earlier: Instant) -> Duration {
    now.checked_duration_since(earlier).unwrap_or_default()
}

fn relay_urls_for_note(note: &Note<'_>, txn: &Transaction) -> HashSet<RelayUrl> {
    note.relays(txn)
        .filter_map(|relay| RelayUrl::parse(relay).ok())
        .collect()
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
/// Missing ids are queued in `UnknownIds` so that request batching and sent
/// history are applied uniformly.
///
#[profiling::function]
pub fn get_unknown_note_ids<'a>(
    ndb: &Ndb,
    cached_note: &CachedNote,
    txn: &'a Transaction,
    note: &Note<'a>,
    unknown_ids: &mut UnknownIds,
) -> Result<()> {
    let now = Instant::now();
    let observed_relays = relay_urls_for_note(note, txn);

    // the author pubkey
    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
        unknown_ids.add_missing_unknown_id_at(
            UnknownId::Pubkey(Pubkey::new(*note.pubkey())),
            observed_relays.clone(),
            now,
        );
    }

    // pull notes that notes are replying to
    if cached_note.reply.root.is_some() {
        let note_reply = cached_note.reply.borrow(note.tags());
        if let Some(root) = note_reply.root() {
            if ndb.get_note_by_id(txn, root.id).is_err() {
                unknown_ids.add_missing_unknown_id_at(
                    UnknownId::Id(NoteId::new(*root.id)),
                    HashSet::default(),
                    now,
                );
            }
        }

        if !note_reply.is_reply_to_root() {
            if let Some(reply) = note_reply.reply() {
                if ndb.get_note_by_id(txn, reply.id).is_err() {
                    unknown_ids.add_missing_unknown_id_at(
                        UnknownId::Id(NoteId::new(*reply.id)),
                        HashSet::default(),
                        now,
                    );
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
            Mention::Pubkey(npub) if ndb.get_profile_by_pubkey(txn, npub.pubkey()).is_err() => {
                unknown_ids.add_missing_unknown_id_at(
                    UnknownId::Pubkey(Pubkey::new(*npub.pubkey())),
                    HashSet::default(),
                    now,
                );
            }
            Mention::Profile(nprofile)
                if ndb.get_profile_by_pubkey(txn, nprofile.pubkey()).is_err() =>
            {
                let id = UnknownId::Pubkey(Pubkey::new(*nprofile.pubkey()));
                let relays = nprofile
                    .relays_iter()
                    .filter_map(|s| RelayUrl::parse(s).ok())
                    .collect::<HashSet<RelayUrl>>();
                unknown_ids.add_missing_unknown_id_at(id, relays, now);
            }
            Mention::Event(ev) => {
                let relays = ev
                    .relays_iter()
                    .filter_map(|s| RelayUrl::parse(s).ok())
                    .collect::<HashSet<RelayUrl>>();
                match ndb.get_note_by_id(txn, ev.id()) {
                    Err(_) => {
                        unknown_ids.add_missing_unknown_id_at(
                            UnknownId::Id(NoteId::new(*ev.id())),
                            relays.clone(),
                            now,
                        );
                        if let Some(pk) = ev.pubkey() {
                            if ndb.get_profile_by_pubkey(txn, pk).is_err() {
                                unknown_ids.add_missing_unknown_id_at(
                                    UnknownId::Pubkey(Pubkey::new(*pk)),
                                    relays,
                                    now,
                                );
                            }
                        }
                    }
                    Ok(note) => {
                        if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                            let relays = relays
                                .into_iter()
                                .chain(relay_urls_for_note(&note, txn))
                                .collect::<HashSet<RelayUrl>>();
                            unknown_ids.add_missing_unknown_id_at(
                                UnknownId::Pubkey(Pubkey::new(*note.pubkey())),
                                relays,
                                now,
                            );
                        }
                    }
                }
            }
            Mention::Note(note) => match ndb.get_note_by_id(txn, note.id()) {
                Err(_) => {
                    unknown_ids.add_missing_unknown_id_at(
                        UnknownId::Id(NoteId::new(*note.id())),
                        HashSet::default(),
                        now,
                    );
                }
                Ok(note) => {
                    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                        unknown_ids.add_missing_unknown_id_at(
                            UnknownId::Pubkey(Pubkey::new(*note.pubkey())),
                            relay_urls_for_note(&note, txn),
                            now,
                        );
                    }
                }
            },
            _ => {}
        }
    }

    Ok(())
}

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

pub fn unknown_id_send(
    unknown_ids: &mut UnknownIds,
    oneshot: &mut OneshotApi<'_>,
    use_outbox_relays: bool,
) {
    let pending_count = unknown_ids.ids_iter().len();
    let Some(batch) = unknown_ids.drain_ready_fetch_batch() else {
        return;
    };
    let remaining_count = unknown_ids.ids_iter().len();
    tracing::debug!(
        "Getting {} unknown ids from relays, {} remaining",
        pending_count.saturating_sub(remaining_count),
        remaining_count,
    );

    if use_outbox_relays {
        let remote_advertised = remote_advertised_fetch_coverage(&batch.items);
        unknown_ids.mark_remote_advertised_fetch_coverage_sent(&batch);
        oneshot.oneshot_with_remote_advertised_coverage(batch.filters, remote_advertised);
    } else {
        oneshot.oneshot(batch.filters);
    }
}

fn remote_advertised_fetch_coverage(
    items: &[UnknownIdFetchItem],
) -> Vec<OneshotRemoteAdvertisedCoverage> {
    let mut ids_by_relay = HashMap::<NormRelayUrl, Vec<&UnknownId>>::new();
    for item in items {
        for relay in &item.relays {
            ids_by_relay
                .entry(NormRelayUrl::from(relay.clone()))
                .or_default()
                .push(&item.id);
        }
    }

    ids_by_relay
        .into_iter()
        .filter_map(|(relay, ids)| {
            get_unknown_ids_filter(&ids)
                .map(|filters| OneshotRemoteAdvertisedCoverage::new([relay], filters))
        })
        .collect()
}

#[test]
fn drain_filter_batch_keeps_unsent_unknown_ids() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    for i in 0..501u64 {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&i.to_be_bytes());
        unknown_ids.add_missing_unknown_id_at(
            UnknownId::Pubkey(Pubkey::new(bytes)),
            HashSet::default(),
            now,
        );
    }

    let batch = unknown_ids
        .drain_ready_fetch_batch_at(now)
        .expect("unknown filter batch");

    assert!(!batch.filters.is_empty());
    assert_eq!(unknown_ids.ids_iter().len(), 1);
    assert_eq!(unknown_ids.sent.len(), 500);
    assert!(unknown_ids.drain_ready_fetch_batch_at(now).is_none());
    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(50))
        .is_some());
}

#[test]
fn drain_fetch_batch_preserves_relay_coverage() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    let relay = RelayUrl::parse("wss://observed-profile.example.com").expect("relay");
    let unknown_id = UnknownId::Pubkey(Pubkey::new([1; 32]));

    assert!(unknown_ids.add_missing_unknown_id_at(unknown_id, HashSet::from([relay.clone()]), now));

    let batch = unknown_ids
        .drain_ready_fetch_batch_at(now)
        .expect("unknown filter batch");

    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.items[0].id, unknown_id);
    assert_eq!(batch.items[0].relays, HashSet::from([relay]));
    assert!(!batch.filters.is_empty());
}

#[test]
fn clear_resets_debounce_state_for_next_unknown_batch() {
    let mut unknown_ids = UnknownIds::default();
    unknown_ids.add_missing_unknown_id(UnknownId::Pubkey(Pubkey::new([1; 32])), HashSet::default());

    unknown_ids.clear();

    unknown_ids.add_missing_unknown_id(UnknownId::Pubkey(Pubkey::new([2; 32])), HashSet::default());

    assert!(unknown_ids.drain_ready_fetch_batch().is_some());
}

#[test]
fn first_discovery_batch_sends_immediately() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([1; 32])),
        HashSet::default(),
        now
    ));
    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Id(NoteId::new([2; 32])),
        HashSet::default(),
        now
    ));

    assert!(unknown_ids.drain_ready_fetch_batch_at(now).is_some());
}

#[test]
fn followup_batches_are_paced_with_backoff_rounds() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([1; 32])),
        HashSet::default(),
        now
    ));
    assert!(unknown_ids.drain_ready_fetch_batch_at(now).is_some());

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([2; 32])),
        HashSet::default(),
        now + Duration::from_millis(10)
    ));
    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(49))
        .is_none());
    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(50))
        .is_some());

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([3; 32])),
        HashSet::default(),
        now + Duration::from_millis(60)
    ));
    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(149))
        .is_none());
    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(150))
        .is_some());
}

#[test]
fn pacing_delays_advance_to_two_second_ceiling() {
    let mut unknown_ids = UnknownIds::default();
    let mut now = Instant::now();

    let sends = [
        (1u8, Duration::ZERO),
        (2, Duration::from_millis(50)),
        (3, Duration::from_millis(100)),
        (4, Duration::from_millis(500)),
        (5, Duration::from_secs(1)),
        (6, Duration::from_secs(2)),
        (7, Duration::from_secs(2)),
    ];

    for (byte, delay) in sends {
        unknown_ids.add_missing_unknown_id_at(
            UnknownId::Pubkey(Pubkey::new([byte; 32])),
            HashSet::default(),
            now,
        );

        if !delay.is_zero() {
            assert!(unknown_ids
                .drain_ready_fetch_batch_at(now + delay - Duration::from_millis(1))
                .is_none());
        }

        now += delay;
        assert!(unknown_ids.drain_ready_fetch_batch_at(now).is_some());
    }
}

#[test]
fn batch_size_bypasses_active_pacing_deadline() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([1; 32])),
        HashSet::default(),
        now
    ));
    assert!(unknown_ids.drain_ready_fetch_batch_at(now).is_some());

    for i in 0..500u64 {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&(i + 2).to_be_bytes());
        unknown_ids.add_missing_unknown_id_at(
            UnknownId::Pubkey(Pubkey::new(bytes)),
            HashSet::default(),
            now + Duration::from_millis(1),
        );
    }

    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(1))
        .is_some());
}

#[test]
fn empty_deadline_resets_pacing_for_next_unknown_batch() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([1; 32])),
        HashSet::default(),
        now
    ));
    assert!(unknown_ids.drain_ready_fetch_batch_at(now).is_some());

    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(50))
        .is_none());

    assert!(unknown_ids.add_missing_unknown_id_at(
        UnknownId::Pubkey(Pubkey::new([2; 32])),
        HashSet::default(),
        now + Duration::from_millis(60)
    ));
    assert!(unknown_ids
        .drain_ready_fetch_batch_at(now + Duration::from_millis(60))
        .is_some());
}

#[test]
fn recently_sent_unknown_id_is_not_requeued() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    let unknown_id = UnknownId::Pubkey(Pubkey::new([1; 32]));

    assert!(unknown_ids.add_missing_unknown_id_at(unknown_id, HashSet::default(), now));
    unknown_ids
        .drain_fetch_batch_at(now)
        .expect("unknown filter batch");

    assert!(!unknown_ids.add_missing_unknown_id_at(
        unknown_id,
        HashSet::default(),
        now + Duration::from_secs(1)
    ));
    assert_eq!(unknown_ids.ids_iter().len(), 0);
}

#[test]
fn recently_sent_unknown_id_requeues_new_relay_coverage() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    let unknown_id = UnknownId::Pubkey(Pubkey::new([1; 32]));
    let relay_a = RelayUrl::parse("wss://observed-a.example.com").expect("relay");
    let relay_b = RelayUrl::parse("wss://observed-b.example.com").expect("relay");

    assert!(unknown_ids.add_missing_unknown_id_at(
        unknown_id,
        HashSet::from([relay_a.clone()]),
        now
    ));
    let first_batch = unknown_ids
        .drain_fetch_batch_at(now)
        .expect("unknown filter batch");
    unknown_ids.mark_remote_advertised_fetch_coverage_sent(&first_batch);

    assert!(unknown_ids.add_missing_unknown_id_at(
        unknown_id,
        HashSet::from([relay_a.clone(), relay_b.clone()]),
        now + Duration::from_secs(1)
    ));

    let second_batch = unknown_ids
        .drain_fetch_batch_at(now + Duration::from_secs(1))
        .expect("unknown filter batch");
    unknown_ids.mark_remote_advertised_fetch_coverage_sent(&second_batch);

    assert_eq!(second_batch.items.len(), 1);
    assert_eq!(second_batch.items[0].id, unknown_id);
    assert_eq!(
        second_batch.items[0].relays,
        HashSet::from([relay_b.clone()])
    );
    assert_eq!(
        unknown_ids
            .sent
            .get(&unknown_id)
            .expect("sent unknown")
            .remote_advertised_relays
            .keys()
            .cloned()
            .collect::<HashSet<_>>(),
        HashSet::from([relay_a.clone(), relay_b])
    );
    assert!(!unknown_ids.add_missing_unknown_id_at(
        unknown_id,
        HashSet::from([relay_a]),
        now + Duration::from_secs(2)
    ));
}

#[test]
fn recently_sent_unknown_id_requeues_unattempted_relay_coverage() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    let unknown_id = UnknownId::Pubkey(Pubkey::new([1; 32]));
    let relay = RelayUrl::parse("wss://observed-unattempted.example.com").expect("relay");

    assert!(unknown_ids.add_missing_unknown_id_at(unknown_id, HashSet::from([relay.clone()]), now));
    unknown_ids
        .drain_fetch_batch_at(now)
        .expect("unknown filter batch");

    assert!(unknown_ids.add_missing_unknown_id_at(
        unknown_id,
        HashSet::from([relay.clone()]),
        now + Duration::from_secs(1)
    ));

    let batch = unknown_ids
        .drain_fetch_batch_at(now + Duration::from_secs(1))
        .expect("unknown filter batch");

    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.items[0].id, unknown_id);
    assert_eq!(batch.items[0].relays, HashSet::from([relay]));
}

#[test]
fn sent_unknown_id_requeues_after_retry_delay() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    let unknown_id = UnknownId::Pubkey(Pubkey::new([1; 32]));

    assert!(unknown_ids.add_missing_unknown_id_at(unknown_id, HashSet::default(), now));
    unknown_ids
        .drain_fetch_batch_at(now)
        .expect("unknown filter batch");

    assert!(unknown_ids.add_missing_unknown_id_at(
        unknown_id,
        HashSet::default(),
        now + UNKNOWN_ID_RETRY_AFTER
    ));
    assert_eq!(unknown_ids.ids_iter().len(), 1);
}

#[test]
fn sent_unknown_id_history_is_pruned() {
    let mut unknown_ids = UnknownIds::default();
    let now = Instant::now();
    let unknown_id = UnknownId::Pubkey(Pubkey::new([1; 32]));

    assert!(unknown_ids.add_missing_unknown_id_at(unknown_id, HashSet::default(), now));
    unknown_ids
        .drain_fetch_batch_at(now)
        .expect("unknown filter batch");

    let after_ttl = now + UNKNOWN_ID_SENT_TTL + UNKNOWN_ID_SENT_PRUNE_INTERVAL;
    assert!(unknown_ids.add_missing_unknown_id_at(unknown_id, HashSet::default(), after_ttl));
    assert_eq!(unknown_ids.sent.len(), 0);
    assert_eq!(unknown_ids.ids_iter().len(), 1);
}
