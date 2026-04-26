use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use enostr::{Pubkey, RelayRoutingPreference};
use nostr::util::JsonUtil;
use nostr::{Keys, PublicKey};
use nostr_double_ratchet::SessionManagerEvent;
use nostrdb::{Filter, NoteKey, Subscription, Transaction};
use notedeck::{
    AppContext, DataPathType, RelaySelection, RelayType, ScopedSubIdentity, SubConfig, SubKey,
    SubOwnerKey,
};

use crate::cache::{ConversationCache, ConversationId};

use super::util::{build_inner_rumor_event, unsigned_event_to_ndb_json};
use super::worker::WorkerHandle;

const MESSAGE_RETRY_DELAY: Duration = Duration::from_millis(200);
const MESSAGE_RETRY_STALE_AFTER: Duration = Duration::from_secs(300);
const MESSAGE_RETRY_MAX_ENTRIES: usize = 4096;
const PEER_APP_KEYS_REFRESH: Duration = Duration::from_secs(1);

struct ActiveSub {
    local_sub: Option<Subscription>,
    filters: Vec<Filter>,
}

/// UI-thread facade for a background `nostr-double-ratchet` worker.
pub(crate) struct RatchetService {
    owner_pubkey: PublicKey,
    owner_keys: Keys,
    worker: WorkerHandle,
    event_rx: crossbeam_channel::Receiver<SessionManagerEvent>,
    send_ready_rx: crossbeam_channel::Receiver<PublicKey>,
    active_subs: HashMap<String, ActiveSub>,
    prepared_recipients: HashSet<PublicKey>,
    send_ready_recipients: HashSet<PublicKey>,
    seen_outer_events: HashSet<[u8; 32]>,
    message_retry_at: HashMap<[u8; 32], Instant>,
    pending_publish: Vec<[u8; 32]>,
    known_app_key_recipients: HashSet<Pubkey>,
    app_key_checked_at: HashMap<Pubkey, Instant>,
}

impl RatchetService {
    /// Construct a ratchet service for the currently selected full-key account.
    pub(crate) fn new(ctx: &AppContext<'_>) -> Option<Self> {
        let filled = ctx.accounts.selected_filled()?;
        let owner_pubkey = PublicKey::from_slice(filled.pubkey.bytes()).ok()?;
        let identity_key = filled.secret_key.to_secret_bytes();
        let owner_keys = Keys::new(filled.secret_key.to_owned());
        let device_id = hex::encode(owner_pubkey.to_bytes());

        let storage_dir = storage_dir(ctx, filled.pubkey);
        let (worker, event_rx, send_ready_rx) =
            match WorkerHandle::spawn(storage_dir, owner_pubkey, identity_key, device_id) {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!("failed to start double-ratchet worker: {err}");
                    return None;
                }
            };

        Some(Self {
            owner_pubkey,
            owner_keys,
            worker,
            event_rx,
            send_ready_rx,
            active_subs: HashMap::new(),
            prepared_recipients: HashSet::new(),
            send_ready_recipients: HashSet::new(),
            seen_outer_events: HashSet::new(),
            message_retry_at: HashMap::new(),
            pending_publish: Vec::new(),
            known_app_key_recipients: HashSet::new(),
            app_key_checked_at: HashMap::new(),
        })
    }

    /// Subscribe to double-ratchet discovery material for a 1:1 conversation peer.
    pub(crate) fn prepare_conversation(
        &mut self,
        conversation_id: ConversationId,
        cache: &ConversationCache,
    ) {
        let Some((_, recipient_pk)) = self.conversation_recipient(conversation_id, cache) else {
            return;
        };

        if self.prepared_recipients.insert(recipient_pk) {
            self.worker.setup_user(recipient_pk);
        }
        if !self.send_ready_recipients.contains(&recipient_pk) {
            self.worker.probe_send_ready(recipient_pk);
        }
    }

    /// Returns whether the 1:1 peer is known to support double ratchet.
    pub(crate) fn conversation_supports_double_ratchet(
        &mut self,
        conversation_id: ConversationId,
        cache: &ConversationCache,
        ctx: &AppContext<'_>,
    ) -> bool {
        let Some((recipient, recipient_pk)) = self.conversation_recipient(conversation_id, cache)
        else {
            return false;
        };

        recipient_pk != self.owner_pubkey
            && (self.send_ready_recipients.contains(&recipient_pk)
                || self.recipient_has_app_keys(ctx, &recipient, false))
    }

    /// Drain worker events, publish queued outer events, and feed matching NDB events to the worker.
    #[profiling::function]
    pub(crate) fn poll(
        &mut self,
        ctx: &mut AppContext<'_>,
        cache: Option<&mut ConversationCache>,
    ) -> bool {
        let mut processed_any = false;
        processed_any |= self.flush_pending_publishes(ctx);
        processed_any |= self.poll_send_ready_recipients();
        processed_any |= self.poll_worker_events(ctx, cache);
        processed_any |= self.poll_send_ready_recipients();
        processed_any |= self.poll_local_subscriptions(ctx);
        processed_any
    }

    /// Attempt to send a 1:1 message through double ratchet.
    ///
    /// Returns `false` when the caller should fall back to NIP-17.
    #[profiling::function]
    pub(crate) fn send_conversation_message(
        &mut self,
        conversation_id: ConversationId,
        content: String,
        cache: &ConversationCache,
        ctx: &mut AppContext<'_>,
    ) -> bool {
        if content.trim().is_empty() {
            return true;
        }
        self.poll_send_ready_recipients();

        let Some(conversation) = cache.get(conversation_id) else {
            tracing::warn!("missing conversation {conversation_id} for ratchet send");
            return true;
        };

        if conversation.metadata.participants.len() > 2 {
            return false;
        }

        let Some((recipient, recipient_pk)) = self.conversation_recipient(conversation_id, cache)
        else {
            return false;
        };
        self.prepare_conversation(conversation_id, cache);

        if recipient_pk == self.owner_pubkey {
            return false;
        }

        let known_app_keys = self.send_ready_recipients.contains(&recipient_pk)
            || self.recipient_has_app_keys(ctx, &recipient, true);
        if !known_app_keys {
            return false;
        }

        let Ok(chat_message_kind) = u16::try_from(nostr_double_ratchet::CHAT_MESSAGE_KIND) else {
            tracing::warn!(
                "double-ratchet: unsupported chat message kind {}",
                nostr_double_ratchet::CHAT_MESSAGE_KIND
            );
            return true;
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let rumor = match build_inner_rumor_event(
            self.owner_pubkey,
            recipient_pk,
            chat_message_kind,
            content,
            Vec::new(),
            now.as_secs(),
            now.as_millis(),
        ) {
            Ok(rumor) => rumor,
            Err(err) => {
                tracing::warn!("double-ratchet: failed to build rumor: {err}");
                return true;
            }
        };

        self.ingest_decrypted_rumor(ctx, rumor.clone(), None);
        self.worker.send_event(recipient_pk, rumor);
        true
    }

    fn conversation_recipient(
        &self,
        conversation_id: ConversationId,
        cache: &ConversationCache,
    ) -> Option<(Pubkey, PublicKey)> {
        let conversation = cache.get(conversation_id)?;
        if conversation.metadata.participants.len() > 2 {
            return None;
        }

        let selected = Pubkey::new(self.owner_pubkey.to_bytes());
        let recipient = conversation
            .metadata
            .participants
            .iter()
            .find(|pk| **pk != selected)
            .copied()
            .unwrap_or(selected);
        let recipient_pk = PublicKey::from_slice(recipient.bytes()).ok()?;

        Some((recipient, recipient_pk))
    }

    /// Stop remote/local subscriptions and the background worker.
    pub(crate) fn shutdown(&mut self, ctx: &mut AppContext<'_>) {
        self.unsubscribe_all(ctx);
        self.worker.shutdown();
    }

    fn poll_worker_events(
        &mut self,
        ctx: &mut AppContext<'_>,
        mut cache: Option<&mut ConversationCache>,
    ) -> bool {
        let mut processed_any = false;
        while let Ok(ev) = self.event_rx.try_recv() {
            processed_any = true;
            match ev {
                SessionManagerEvent::Subscribe { subid, filter_json } => {
                    self.subscribe(ctx, subid, filter_json);
                }
                SessionManagerEvent::Unsubscribe(subid) => {
                    self.unsubscribe(ctx, &subid);
                }
                SessionManagerEvent::Publish(unsigned) => {
                    match unsigned.sign_with_keys(&self.owner_keys) {
                        Ok(signed) => self.queue_publish(ctx, signed),
                        Err(err) => tracing::warn!("double-ratchet: failed to sign event: {err}"),
                    }
                }
                SessionManagerEvent::PublishSigned(signed) => {
                    self.queue_publish(ctx, signed);
                }
                SessionManagerEvent::PublishSignedForInnerEvent { event, .. } => {
                    self.queue_publish(ctx, event);
                }
                SessionManagerEvent::DecryptedMessage {
                    sender,
                    content,
                    event_id,
                    ..
                } => {
                    if let Some(id) = event_id.as_deref().and_then(event_id_bytes) {
                        self.seen_outer_events.insert(id);
                        self.message_retry_at.remove(&id);
                    }

                    let mut rumor: nostr::UnsignedEvent = match serde_json::from_str(&content) {
                        Ok(rumor) => rumor,
                        Err(err) => {
                            tracing::warn!(
                                "double-ratchet: failed to parse decrypted rumor JSON: {err}"
                            );
                            continue;
                        }
                    };

                    // Treat the session peer as authoritative for inbound attribution.
                    rumor.pubkey = sender;
                    self.ingest_decrypted_rumor(ctx, rumor, event_id.as_deref());
                    if let Some(cache) = cache.as_deref_mut() {
                        self.poll_conversation_subscription(ctx, cache);
                    }
                }
                SessionManagerEvent::ReceivedEvent(_) => {}
            }
        }
        processed_any
    }

    fn poll_send_ready_recipients(&mut self) -> bool {
        let mut processed_any = false;
        while let Ok(recipient) = self.send_ready_rx.try_recv() {
            if self.send_ready_recipients.insert(recipient) {
                processed_any = true;
            }
        }
        processed_any
    }

    fn subscribe(&mut self, ctx: &mut AppContext<'_>, subid: String, filter_json: String) {
        let filter = match Filter::from_json(&filter_json) {
            Ok(filter) => filter,
            Err(err) => {
                tracing::warn!("double-ratchet: failed to parse filter json: {err}");
                return;
            }
        };

        let filters = vec![filter];
        let local_sub = ctx.ndb.subscribe(&filters).ok();
        let identity = sub_identity(&subid);
        let spec = SubConfig {
            relays: RelaySelection::AccountsRead,
            filters: filters.clone(),
            routing_preference: RelayRoutingPreference::default(),
        };
        let _ = ctx.remote.scoped_subs(ctx.accounts).set_sub(identity, spec);

        self.active_subs
            .insert(subid, ActiveSub { local_sub, filters });
    }

    fn unsubscribe(&mut self, ctx: &mut AppContext<'_>, subid: &str) {
        if let Some(active) = self.active_subs.remove(subid) {
            if let Some(sub) = active.local_sub {
                let _ = ctx.ndb.unsubscribe(sub);
            }
        }
        let _ = ctx
            .remote
            .scoped_subs(ctx.accounts)
            .clear_sub(sub_identity(subid));
    }

    fn unsubscribe_all(&mut self, ctx: &mut AppContext<'_>) {
        let subids: Vec<String> = self.active_subs.keys().cloned().collect();
        for subid in subids {
            self.unsubscribe(ctx, &subid);
        }
    }

    fn queue_publish(&mut self, ctx: &mut AppContext<'_>, event: nostr::Event) {
        let id = event.id.to_bytes();
        self.seen_outer_events.insert(id);

        let json = event.as_json();
        if let Err(err) = ctx.ndb.process_client_event(&json) {
            tracing::warn!("double-ratchet: failed to ingest local outer event: {err}");
        }

        if !self.pending_publish.contains(&id) {
            self.pending_publish.push(id);
        }
    }

    fn flush_pending_publishes(&mut self, ctx: &mut AppContext<'_>) -> bool {
        if self.pending_publish.is_empty() {
            return false;
        }

        let mut published_any = false;
        let mut remaining = Vec::new();
        let Ok(txn) = Transaction::new(ctx.ndb) else {
            return false;
        };

        for id in self.pending_publish.drain(..) {
            match ctx.ndb.get_note_by_id(&txn, &id) {
                Ok(note) => {
                    ctx.remote
                        .publisher(ctx.accounts)
                        .publish_note(&note, RelayType::AccountsWrite);
                    published_any = true;
                }
                Err(_) => remaining.push(id),
            }
        }

        self.pending_publish = remaining;
        published_any
    }

    fn poll_local_subscriptions(&mut self, ctx: &mut AppContext<'_>) -> bool {
        let active_subs: Vec<(Option<Subscription>, Vec<Filter>)> = self
            .active_subs
            .values()
            .map(|active| (active.local_sub, active.filters.clone()))
            .collect();
        let mut processed_any = false;

        for (sub, filters) in active_subs {
            let Ok(txn) = Transaction::new(ctx.ndb) else {
                continue;
            };

            if let Some(sub) = sub {
                let keys = ctx.ndb.poll_for_notes(sub, 32);
                if !keys.is_empty() {
                    processed_any = true;
                }

                for key in keys {
                    let Ok(note) = ctx.ndb.get_note_by_key(&txn, key) else {
                        continue;
                    };
                    if self.process_outer_note(note) {
                        processed_any = true;
                    }
                }
            }

            let Ok(results) = ctx.ndb.query(&txn, &filters, 32) else {
                continue;
            };
            for result in results {
                if self.process_outer_note(result.note) {
                    processed_any = true;
                }
            }
        }

        processed_any
    }

    fn process_outer_note(&mut self, note: nostrdb::Note<'_>) -> bool {
        let id = *note.id();
        let Ok(json) = note.json() else {
            return false;
        };
        match nostr::Event::from_json(json) {
            Ok(event) => {
                if event.kind.as_u16() == nostr_double_ratchet::MESSAGE_EVENT_KIND as u16 {
                    if self.seen_outer_events.contains(&id) {
                        return false;
                    }

                    let now = Instant::now();
                    if self
                        .message_retry_at
                        .get(&id)
                        .is_some_and(|retry_at| *retry_at > now)
                    {
                        return false;
                    }

                    self.prune_message_retries(now);
                    self.message_retry_at.insert(id, now + MESSAGE_RETRY_DELAY);
                    self.worker.process_event(event);
                    return false;
                }

                if !self.seen_outer_events.insert(id) {
                    return false;
                }
                self.worker.process_event(event);
                true
            }
            Err(err) => {
                tracing::warn!("double-ratchet: failed to parse outer event: {err}");
                false
            }
        }
    }

    fn prune_message_retries(&mut self, now: Instant) {
        if let Some(stale_before) = now.checked_sub(MESSAGE_RETRY_STALE_AFTER) {
            self.message_retry_at
                .retain(|_, retry_at| *retry_at >= stale_before);
        }

        if self.message_retry_at.len() <= MESSAGE_RETRY_MAX_ENTRIES {
            return;
        }

        let mut entries: Vec<([u8; 32], Instant)> = self
            .message_retry_at
            .iter()
            .map(|(id, retry_at)| (*id, *retry_at))
            .collect();
        entries.sort_by_key(|(_, retry_at)| *retry_at);

        let remove_count = self.message_retry_at.len() - MESSAGE_RETRY_MAX_ENTRIES;
        for (id, _) in entries.into_iter().take(remove_count) {
            self.message_retry_at.remove(&id);
        }
    }

    fn recipient_has_app_keys(
        &mut self,
        ctx: &AppContext<'_>,
        recipient: &Pubkey,
        force_refresh: bool,
    ) -> bool {
        if self.known_app_key_recipients.contains(recipient) {
            return true;
        }

        let now = Instant::now();
        if !force_refresh
            && self
                .app_key_checked_at
                .get(recipient)
                .is_some_and(|last_checked| {
                    now.duration_since(*last_checked) < PEER_APP_KEYS_REFRESH
                })
        {
            return false;
        }
        self.app_key_checked_at.insert(*recipient, now);

        if query_recipient_has_app_keys(ctx, recipient) {
            self.known_app_key_recipients.insert(*recipient);
            return true;
        }

        false
    }

    fn ingest_decrypted_rumor(
        &self,
        ctx: &mut AppContext<'_>,
        mut rumor: nostr::UnsignedEvent,
        outer_event_id: Option<&str>,
    ) {
        rumor.id = None;
        rumor.ensure_id();

        if rumor.kind.as_u16() != nostr_double_ratchet::CHAT_MESSAGE_KIND as u16 {
            return;
        }

        let json = match unsigned_event_to_ndb_json(rumor, outer_event_id) {
            Ok(json) => json,
            Err(err) => {
                tracing::warn!("double-ratchet: failed to encode decrypted rumor: {err}");
                return;
            }
        };

        if let Err(err) = ctx.ndb.process_client_event(&json) {
            tracing::warn!("double-ratchet: failed to ingest decrypted rumor: {err}");
        }
    }

    fn poll_conversation_subscription(
        &self,
        ctx: &mut AppContext<'_>,
        cache: &mut ConversationCache,
    ) {
        let ConversationCache { state, .. } = cache;
        let sub = match state {
            crate::cache::ConversationListState::Loading {
                subscription: Some(sub),
            }
            | crate::cache::ConversationListState::Initialized(Some(sub)) => *sub,
            _ => return,
        };

        let keys = ctx.ndb.poll_for_notes(sub, 32);
        ingest_note_keys(ctx, cache, &keys);
    }
}

impl Drop for RatchetService {
    fn drop(&mut self) {
        self.worker.shutdown();
    }
}

fn event_id_bytes(event_id: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(event_id).ok()?;
    bytes.try_into().ok()
}

fn ingest_note_keys(ctx: &mut AppContext<'_>, cache: &mut ConversationCache, keys: &[NoteKey]) {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return;
    };

    for key in keys {
        let Ok(note) = ctx.ndb.get_note_by_key(&txn, *key) else {
            continue;
        };
        cache.ingest_chatroom_msg(note, *key, ctx.ndb, &txn, ctx.note_cache, ctx.unknown_ids);
    }
}

fn query_recipient_has_app_keys(ctx: &AppContext<'_>, recipient: &Pubkey) -> bool {
    let Ok(txn) = Transaction::new(ctx.ndb) else {
        return false;
    };
    let filter = Filter::new()
        .authors([recipient.bytes()])
        .kinds([nostr_double_ratchet::APP_KEYS_EVENT_KIND as u64])
        .limit(4)
        .build();
    let Ok(results) = ctx.ndb.query(&txn, std::slice::from_ref(&filter), 4) else {
        return false;
    };

    for result in results {
        let Ok(json) = result.note.json() else {
            continue;
        };
        let Ok(event) = nostr::Event::from_json(json) else {
            continue;
        };
        if !nostr_double_ratchet::is_app_keys_event(&event) {
            continue;
        }

        if nostr_double_ratchet::AppKeys::from_event(&event)
            .map(|app_keys| !app_keys.get_all_devices().is_empty())
            .unwrap_or(false)
        {
            return true;
        }
    }

    false
}

fn storage_dir(ctx: &AppContext<'_>, pubkey: &Pubkey) -> PathBuf {
    let keys_dir = ctx.path.path(DataPathType::Keys);
    let storage_root = keys_dir.parent().unwrap_or(keys_dir.as_path());
    storage_root.join("double-ratchet").join(pubkey.hex())
}

fn sub_identity(subid: &str) -> ScopedSubIdentity {
    ScopedSubIdentity::account(
        SubOwnerKey::builder("double-ratchet").finish(),
        SubKey::builder("double-ratchet").with(subid).finish(),
    )
}
