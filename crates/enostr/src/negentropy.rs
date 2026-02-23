//! NIP-77 negentropy set reconciliation for relay event syncing.
//!
//! Provides a [`NegentropySync`] state machine that any app can use to
//! discover and fetch missing events from a relay. The caller owns the
//! relay pool and ndb — this module just drives the protocol.
//!
//! # Usage
//!
//! ```ignore
//! // In your update loop's relay event callback, collect negentropy events:
//! let mut neg_events = Vec::new();
//! try_process_events_core(ctx, ui.ctx(), |app_ctx, ev| {
//!     if ev.relay == my_relay {
//!         neg_events.extend(NegEvent::from_relay(&ev.event));
//!     }
//! });
//!
//! // Then process everything in one call:
//! self.neg_sync.process(neg_events, ctx.ndb, ctx.pool, &filter, &relay_url);
//! ```

use crate::{ClientMessage, RelayPool};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostrdb::{Filter, Ndb, Transaction};

/// Maximum number of event IDs to request in a single REQ.
const FETCH_BATCH_SIZE: usize = 100;

#[derive(Debug, PartialEq, Eq)]
enum SyncState {
    Idle,
    Reconciling,
}

/// A negentropy-relevant event extracted from a raw relay message.
///
/// Apps collect these inside their relay event callback, then pass
/// them to [`NegentropySync::process`].
pub enum NegEvent {
    /// A NEG-MSG response from the relay.
    Msg { sub_id: String, payload: String },
    /// A NEG-ERR response from the relay.
    Err { sub_id: String, reason: String },
    /// The relay (re)connected — triggers an immediate sync.
    RelayOpened,
}

impl NegEvent {
    /// Try to extract a negentropy event from a raw websocket event.
    ///
    /// Returns `None` if the message isn't a negentropy protocol message.
    /// Relay open events should be pushed separately by the app.
    pub fn from_relay(ws: &ewebsock::WsEvent) -> Option<Self> {
        let text = match ws {
            ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(t)) => t,
            _ => return None,
        };

        if text.starts_with("[\"NEG-MSG\"") {
            let v: serde_json::Value = serde_json::from_str(text).ok()?;
            let arr = v.as_array()?;
            if arr.len() >= 3 && arr[0].as_str()? == "NEG-MSG" {
                return Some(NegEvent::Msg {
                    sub_id: arr[1].as_str()?.to_string(),
                    payload: arr[2].as_str()?.to_string(),
                });
            }
        } else if text.starts_with("[\"NEG-ERR\"") {
            let v: serde_json::Value = serde_json::from_str(text).ok()?;
            let arr = v.as_array()?;
            if arr.len() >= 3 && arr[0].as_str()? == "NEG-ERR" {
                return Some(NegEvent::Err {
                    sub_id: arr[1].as_str()?.to_string(),
                    reason: arr[2].as_str()?.to_string(),
                });
            }
        }

        None
    }
}

/// NIP-77 negentropy reconciliation state machine.
///
/// Compares the client's local event set against a relay and fetches
/// any missing events. Generic over event kinds — the caller provides
/// the filter.
pub struct NegentropySync {
    state: SyncState,
    sub_id: Option<String>,
    neg: Option<Negentropy<'static, NegentropyStorageVector>>,
    /// Whether a sync has been requested (startup, reconnect, or re-sync after fetch).
    sync_requested: bool,
    /// IDs accumulated across multi-round reconciliation.
    need_ids: Vec<[u8; 32]>,
}

impl NegentropySync {
    pub fn new() -> Self {
        Self {
            state: SyncState::Idle,
            sub_id: None,
            neg: None,
            sync_requested: false,
            need_ids: Vec::new(),
        }
    }

    /// Request a sync on the next `process()` call.
    ///
    /// Call this on startup and reconnect. Also called internally
    /// after fetching missing events to verify catch-up is complete.
    pub fn trigger_now(&mut self) {
        self.sync_requested = true;
    }

    /// Process collected relay events and run periodic sync.
    ///
    /// Call this once per frame after collecting [`NegEvent`]s from
    /// the relay event loop. Handles the full protocol lifecycle:
    /// initiating sync, multi-round reconciliation, fetching missing
    /// events, error recovery, and periodic re-sync.
    ///
    /// Returns the number of missing events fetched this call, so
    /// the caller can decide whether to re-trigger another round.
    pub fn process(
        &mut self,
        events: Vec<NegEvent>,
        ndb: &Ndb,
        pool: &mut RelayPool,
        filter: &Filter,
        relay_url: &str,
    ) -> usize {
        let mut fetched = 0;

        for event in events {
            match event {
                NegEvent::RelayOpened => {
                    self.trigger_now();
                }
                NegEvent::Msg { sub_id, payload } => {
                    if self.sub_id.as_deref() != Some(&sub_id) {
                        continue;
                    }
                    fetched += self.handle_msg(&payload, pool, relay_url);
                }
                NegEvent::Err { sub_id, reason } => {
                    if self.sub_id.as_deref() != Some(&sub_id) {
                        continue;
                    }
                    tracing::warn!("negentropy NEG-ERR: {reason}");
                    self.reset_after_error();
                }
            }
        }

        // Initiate sync if requested and idle
        if self.sync_requested && self.state == SyncState::Idle {
            self.sync_requested = false;
            if let Some(open_msg) = self.initiate(ndb, filter) {
                pool.send_to(&ClientMessage::Raw(open_msg), relay_url);
                tracing::info!("negentropy: initiated sync");
            }
        }

        fetched
    }

    fn initiate(&mut self, ndb: &Ndb, filter: &Filter) -> Option<String> {
        let txn = Transaction::new(ndb).ok()?;

        let mut storage = NegentropyStorageVector::new();
        let result = ndb.fold(
            &txn,
            std::slice::from_ref(filter),
            &mut storage,
            |storage, note| {
                let created_at = note.created_at();
                let id = Id::from_byte_array(*note.id());
                let _ = storage.insert(created_at, id);
                storage
            },
        );

        if result.is_err() {
            return None;
        }

        storage.seal().ok()?;

        let mut neg = Negentropy::owned(storage, 0).ok()?;
        let init_msg = neg.initiate().ok()?;
        let init_hex = hex::encode(&init_msg);

        let filter_json = filter.json().ok()?;
        let sub_id = uuid::Uuid::new_v4().to_string();

        let msg = format!(
            r#"["NEG-OPEN","{}",{},"{}"]"#,
            sub_id, filter_json, init_hex
        );

        self.neg = Some(neg);
        self.sub_id = Some(sub_id);
        self.state = SyncState::Reconciling;
        self.need_ids.clear();

        Some(msg)
    }

    /// Handle a NEG-MSG from the relay. Returns the number of missing
    /// events fetched (0 while still reconciling, >0 when complete and
    /// events were fetched).
    fn handle_msg(&mut self, msg_hex: &str, pool: &mut RelayPool, relay_url: &str) -> usize {
        let neg = match self.neg.as_mut() {
            Some(n) => n,
            None => {
                tracing::warn!("negentropy: received msg with no active session");
                return 0;
            }
        };

        let msg_bytes = match hex::decode(msg_hex) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("negentropy hex decode: {e}");
                self.reset_after_error();
                return 0;
            }
        };

        let mut have_ids = Vec::new();
        let mut need_ids = Vec::new();

        match neg.reconcile_with_ids(&msg_bytes, &mut have_ids, &mut need_ids) {
            Ok(Some(next_msg)) => {
                self.need_ids
                    .extend(need_ids.iter().map(|id| id.to_bytes()));
                let next_hex = hex::encode(&next_msg);
                let sub_id = self.sub_id.as_ref().unwrap();
                let msg = format!(r#"["NEG-MSG","{}","{}"]"#, sub_id, next_hex);
                pool.send_to(&ClientMessage::Raw(msg), relay_url);
                0
            }
            Ok(None) => {
                // Reconciliation complete
                self.need_ids
                    .extend(need_ids.iter().map(|id| id.to_bytes()));
                let missing = std::mem::take(&mut self.need_ids);

                // Send NEG-CLOSE
                if let Some(sub_id) = &self.sub_id {
                    let close = format!(r#"["NEG-CLOSE","{}"]"#, sub_id);
                    pool.send_to(&ClientMessage::Raw(close), relay_url);
                }

                self.state = SyncState::Idle;
                self.neg = None;

                let count = missing.len();
                if count > 0 {
                    tracing::info!("negentropy: fetching {} missing events", count);
                    Self::fetch_missing(&missing, pool, relay_url);
                }
                count
            }
            Err(e) => {
                tracing::warn!("negentropy reconcile: {e}");
                self.reset_after_error();
                0
            }
        }
    }

    fn reset_after_error(&mut self) {
        self.state = SyncState::Idle;
        self.sync_requested = false;
        self.sub_id = None;
        self.neg = None;
        self.need_ids.clear();
    }

    fn fetch_missing(ids: &[[u8; 32]], pool: &mut RelayPool, relay_url: &str) {
        for chunk in ids.chunks(FETCH_BATCH_SIZE) {
            let sub_id = uuid::Uuid::new_v4().to_string();
            let filter = Filter::new().ids(chunk.iter()).build();
            let req = ClientMessage::req(sub_id, vec![filter]);
            pool.send_to(&req, relay_url);
        }
    }
}

impl Default for NegentropySync {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neg_event_from_relay_msg() {
        let ws = ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(
            r#"["NEG-MSG","abc123","deadbeef"]"#.to_string(),
        ));
        match NegEvent::from_relay(&ws).unwrap() {
            NegEvent::Msg { sub_id, payload } => {
                assert_eq!(sub_id, "abc123");
                assert_eq!(payload, "deadbeef");
            }
            _ => panic!("expected Msg"),
        }
    }

    #[test]
    fn test_neg_event_from_relay_err() {
        let ws = ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(
            r#"["NEG-ERR","abc123","RESULTS_TOO_BIG"]"#.to_string(),
        ));
        match NegEvent::from_relay(&ws).unwrap() {
            NegEvent::Err { sub_id, reason } => {
                assert_eq!(sub_id, "abc123");
                assert_eq!(reason, "RESULTS_TOO_BIG");
            }
            _ => panic!("expected Err"),
        }
    }

    #[test]
    fn test_neg_event_ignores_other() {
        let ws = ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(
            r#"["EVENT","sub","{}"]"#.to_string(),
        ));
        assert!(NegEvent::from_relay(&ws).is_none());
    }

    #[test]
    fn test_no_sync_by_default() {
        let sync = NegentropySync::new();
        assert!(!sync.sync_requested);
    }

    #[test]
    fn test_trigger_now() {
        let mut sync = NegentropySync::new();
        assert!(!sync.sync_requested);
        sync.trigger_now();
        assert!(sync.sync_requested);
    }
}
