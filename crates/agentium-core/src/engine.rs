//! The standalone agentium engine: an owned [`Ndb`] plus a background relay-sync
//! loop that implements the [`Transport`] boundary, so the same engine drops
//! into a desktop host or a standalone/iOS process with no host application
//! context.
//!
//! # Database ownership
//!
//! The engine must be agnostic to whether it *owns* or *shares* its nostrdb.
//! [`Ndb`] is `#[derive(Clone)]` over an `Arc`-backed handle: a clone is a cheap
//! reference to the *same* database, and the database is torn down only when the
//! last clone drops. So the engine holds an `Ndb` **by value** — no lifetime
//! parameter, no borrowed/owned split — and the two constructors converge on
//! that one stored type:
//!
//! - [`Engine::open`] — standalone/iOS: the engine creates its own database and
//!   holds it.
//! - [`Engine::with_ndb`] — an embedding host passes a *clone* of its own
//!   database; both point at the same db, and the engine dropping won't tear
//!   down the host's db because the host still holds a clone.
//!
//! Holding by value is also *required*, not merely convenient: the reconcile
//! loop runs in a `tokio::spawn`ed task and needs a `'static` handle, so a
//! borrowed `&Ndb` couldn't move into the task — a cloned `Ndb` is exactly
//! right.
//!
//! # Relay loop
//!
//! Construction `tokio::spawn`s a background task ([`engine_loop`]) that owns a
//! [`RelayPool`] for the live subscription stream and, per subscription, a
//! NIP-77 negentropy [`backfill`] task for bounded history. The [`Transport`]
//! implementation is a thin front end: its methods enqueue commands the loop
//! drains, so the public surface stays synchronous and non-blocking while all
//! I/O happens on the tasks. Because both tasks are spawned,
//! `Engine::open`/`with_ndb` must be called from within a Tokio runtime.
//!
//! ## Keeping the futures `Send`
//!
//! `nostrdb::Filter`/`Note`/`Transaction` wrap raw pointers and are `!Send`, so
//! the loop and backfill tasks must never hold one across an `.await` (a value
//! merely *in scope* across an await counts, even after an explicit `drop`).
//! Two rules keep everything `Send` and spawnable on the shared multi-thread
//! runtime:
//!
//! - Filters cross the command channel and live in the loop's state as
//!   [`SendFilter`] (nostrdb's sendable, non-custom filter wrapper), converted
//!   from the caller's `Filter`s on the caller's thread. They are turned back
//!   into a transient `Filter` — only synchronously, never across an await — to
//!   build a `REQ` or a negentropy fold. (A filter with a custom predicate can't
//!   be a `SendFilter`, but such predicates are local-only and meaningless on
//!   the wire, so dropping them for a remote subscription is correct.)
//! - Fallible relay calls return `Box<dyn Error>` (also `!Send`); [`backfill`]
//!   collapses each such `Result` into a `Send` value before the next await
//!   rather than letting the error linger in scope.
//!
//! (nostrdb_net's [`sync::Relay::sync_into`] was likewise adjusted upstream to
//! drop its parsed `Filter` before awaiting, so its future is `Send` too.)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use negentropy::{Id, NegentropyStorageVector};
use nostrdb::{Filter, Ndb, SendFilter, Transaction};
use nostrdb_net::relay::sync;
use nostrdb_net::{ClientMessage, RelayPool, RelayStatus, WsEvent, WsMessage};
use tokio::sync::{mpsc, Notify};
use tokio::time::interval;

use crate::transport::{SubscriptionId, SubscriptionSpec, Transport};

/// How many event ids to pull per `REQ` when fetching reconciled events, kept
/// under the relay's single-`REQ` replay cap (mirrors nostrdb_net's sync).
const ID_FETCH_CHUNK: usize = 300;

/// How often the loop pings/reconnects relays to keep the pool alive.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// A command from a [`Transport`] method to the background [`engine_loop`].
///
/// Every field is `Send`: filters are wrapped as [`SendFilter`] and relay urls
/// are strings, converted on the caller's thread.
enum EngineCmd {
    SetSubscription {
        id: SubscriptionId,
        url: String,
        live: Vec<SendFilter>,
        history: Vec<SendFilter>,
    },
    DropSubscription(SubscriptionId),
    Publish {
        note_json: String,
        relays: Vec<String>,
    },
}

/// The standalone agentium engine.
///
/// See the module docs for db ownership and the relay loop. The [`Transport`]
/// impl enqueues onto the loop; drop the engine to stop the loop (the command
/// channel closes and the task returns).
pub struct Engine {
    ndb: Ndb,
    cmd_tx: mpsc::UnboundedSender<EngineCmd>,
}

impl Engine {
    /// Open a standalone engine over its own nostrdb at `path` (created if
    /// absent). Use this on a host that has no existing database of its own.
    /// Must be called from within a Tokio runtime (spawns the relay loop).
    pub fn open(path: &str) -> Result<Self, nostrdb::Error> {
        let ndb = Ndb::new(path, &nostrdb::Config::new())?;
        Ok(Self::with_ndb(ndb))
    }

    /// Build an engine over an existing database, taking a cheap [`Ndb`] clone.
    /// Use this on an embedding host: pass `host_ndb.clone()` so the engine and
    /// host share one database and neither's drop tears it down while the other
    /// still holds a handle. Must be called from within a Tokio runtime (spawns
    /// the relay loop).
    pub fn with_ndb(ndb: Ndb) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(engine_loop(ndb.clone(), cmd_rx));
        Self { ndb, cmd_tx }
    }

    /// The engine's database handle.
    pub fn ndb(&self) -> &Ndb {
        &self.ndb
    }
}

/// Wrap caller filters as [`SendFilter`] so they can cross to the loop thread,
/// dropping any with a custom predicate (not sendable, and not meaningful on the
/// wire for a remote subscription).
fn to_send_filters(filters: Vec<Filter>) -> Vec<SendFilter> {
    filters
        .into_iter()
        .filter_map(|f| SendFilter::try_from_filter(f).ok())
        .collect()
}

impl Transport for Engine {
    fn publish_event_json(&mut self, note_json: String, relays: Vec<enostr::NormRelayUrl>) {
        let relays = relays.iter().map(|r| r.to_string()).collect();
        let _ = self.cmd_tx.send(EngineCmd::Publish { note_json, relays });
    }

    fn set_subscription(&mut self, spec: SubscriptionSpec) {
        let _ = self.cmd_tx.send(EngineCmd::SetSubscription {
            id: spec.id,
            url: spec.relay.to_string(),
            live: to_send_filters(spec.live_filters),
            history: to_send_filters(spec.history_filters),
        });
    }

    fn drop_subscription(&mut self, id: &SubscriptionId) {
        let _ = self.cmd_tx.send(EngineCmd::DropSubscription(*id));
    }
}

/// The relay subscription id for a [`SubscriptionId`] — the wire `REQ`/`CLOSE`
/// subscription string.
fn subid(id: &SubscriptionId) -> String {
    format!("{}:{}", id.owner, id.key)
}

/// Build a `REQ` [`ClientMessage`] for `filters`. Cloning each [`SendFilter`]
/// back to a transient `Filter` is done synchronously (never across an await),
/// so the loop future stays `Send`.
fn req_message(sid: String, filters: &[SendFilter]) -> ClientMessage {
    ClientMessage::req(sid, filters.iter().map(|f| f.as_filter().clone()).collect())
}

/// Whether `url` is currently connected in the pool (so a `REQ`/`EVENT` can be
/// sent now rather than deferred until its `Opened` event).
fn relay_connected(pool: &RelayPool, url: &str) -> bool {
    pool.relays
        .iter()
        .any(|r| r.relay.url == url && matches!(r.relay.status, RelayStatus::Connected))
}

/// The background relay loop: owns the [`RelayPool`], ingests inbound events into
/// `ndb`, and applies [`EngineCmd`]s. Returns when the command channel closes
/// (all [`Transport`] handles — i.e. the [`Engine`] — dropped).
async fn engine_loop(ndb: Ndb, mut cmd_rx: mpsc::UnboundedReceiver<EngineCmd>) {
    let notify = Arc::new(Notify::new());
    let wakeup = {
        let notify = notify.clone();
        move || notify.notify_one()
    };

    let mut pool = RelayPool::new();
    // Desired live subscriptions per relay url (subid -> filters), re-sent
    // whenever a relay (re)connects.
    let mut desired: HashMap<String, HashMap<String, Vec<SendFilter>>> = HashMap::new();
    // Publish frames queued until their target relay is connected.
    let mut pending: HashMap<String, Vec<String>> = HashMap::new();
    let mut keepalive = interval(KEEPALIVE_INTERVAL);

    loop {
        // Drain everything the pool has ready: ingest events, and flush desired
        // subs / queued publishes to relays as they come up.
        while let Some(ev) = pool.try_recv().map(|e| e.into_owned()) {
            match ev.event {
                WsEvent::Opened => {
                    if let Some(subs) = desired.get(&ev.relay) {
                        for (sid, filters) in subs {
                            pool.send_to(&req_message(sid.clone(), filters), &ev.relay);
                        }
                    }
                    if let Some(frames) = pending.remove(&ev.relay) {
                        for frame in frames {
                            pool.send_to(&ClientMessage::raw(frame), &ev.relay);
                        }
                    }
                }
                WsEvent::Message(WsMessage::Text(text)) => {
                    // Only EVENT frames ingest; EOSE/NOTICE/OK aren't events, so
                    // a parse/ingest failure here is expected, not an error.
                    if let Err(e) = ndb.process_event(&text) {
                        tracing::trace!("engine: skipped non-event relay message: {e}");
                    }
                }
                _ => {}
            }
        }

        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break }; // engine dropped
                apply_cmd(&ndb, &mut pool, &mut desired, &mut pending, &wakeup, cmd);
            }
            // The pool signalled new data; loop back around to drain it.
            _ = notify.notified() => {}
            _ = keepalive.tick() => pool.keepalive_ping(wakeup.clone()),
        }
    }
}

/// Apply one [`EngineCmd`] against the loop's pool and desired/pending state.
fn apply_cmd(
    ndb: &Ndb,
    pool: &mut RelayPool,
    desired: &mut HashMap<String, HashMap<String, Vec<SendFilter>>>,
    pending: &mut HashMap<String, Vec<String>>,
    wakeup: &(impl Fn() + Send + Sync + Clone + 'static),
    cmd: EngineCmd,
) {
    match cmd {
        EngineCmd::SetSubscription {
            id,
            url,
            live,
            history,
        } => {
            let sid = subid(&id);
            let _ = pool.add_url(url.clone(), wakeup.clone());
            // Send the live REQ now if the relay is already up; otherwise it is
            // flushed from `desired` on the relay's `Opened` event.
            if relay_connected(pool, &url) {
                pool.send_to(&req_message(sid.clone(), &live), &url);
            }
            desired.entry(url.clone()).or_default().insert(sid, live);
            // NIP-77 negentropy history backfill, off the loop on its own task.
            if !history.is_empty() {
                tokio::spawn(backfill(ndb.clone(), url, history));
            }
        }
        EngineCmd::DropSubscription(id) => {
            let sid = subid(&id);
            for subs in desired.values_mut() {
                subs.remove(&sid);
            }
            pool.unsubscribe(sid);
        }
        EngineCmd::Publish { note_json, relays } => {
            let frame = format!(r#"["EVENT",{note_json}]"#);
            for url in relays {
                let _ = pool.add_url(url.clone(), wakeup.clone());
                if relay_connected(pool, &url) {
                    pool.send_to(&ClientMessage::raw(frame.clone()), &url);
                } else {
                    pending.entry(url).or_default().push(frame.clone());
                }
            }
        }
    }
}

/// Pull bounded history for a subscription from `url` using NIP-77 negentropy.
///
/// Opens a dedicated reconcile connection (separate from the live pool), and for
/// each history filter reconciles the local set against the relay, then fetches
/// the ids the relay holds that the local db lacks. If the relay can't
/// reconcile, falls back to a plain `REQ` pull of the filter.
async fn backfill(ndb: Ndb, url: String, filters: Vec<SendFilter>) {
    let mut relay = match sync::Relay::connect(&url).await {
        Ok(relay) => relay,
        Err(e) => {
            tracing::warn!("engine backfill: connect {url} failed: {e}");
            return;
        }
    };

    // Consume by value: an owned `SendFilter` is `Send` and may cross the awaits
    // below, whereas a `&SendFilter` would require `SendFilter: Sync` (it isn't).
    for filter in filters {
        let Ok(filter_json) = filter.as_filter().json() else {
            continue;
        };
        let storage = match local_negentropy_set(&ndb, filter.as_filter()) {
            Ok(storage) => storage,
            Err(e) => {
                tracing::warn!("engine backfill: local set failed: {e}");
                continue;
            }
        };

        // Collapse the reconcile `Result` (whose `Box<dyn Error>` is !Send) into
        // a `Send` `Option` *before* any further await — otherwise the `match`
        // scrutinee temporary keeps the `Box` slot alive across the `sync_into`
        // awaits below and the whole future becomes !Send. `None` means the relay
        // couldn't reconcile, so fall back to a plain NIP-01 `REQ`.
        let need = match relay.reconcile(&filter_json, storage).await {
            Ok(diff) => Some(diff.need),
            Err(e) => {
                tracing::debug!("engine backfill: reconcile unavailable ({e}); REQ fallback");
                None
            }
        };
        match need {
            Some(need) => {
                for chunk in need.chunks(ID_FETCH_CHUNK) {
                    let ids: Vec<String> = chunk.iter().map(hex::encode).collect();
                    let req = serde_json::json!({ "ids": ids }).to_string();
                    if let Err(e) = relay.sync_into(&ndb, &req).await {
                        tracing::warn!("engine backfill: fetch failed: {e}");
                        break;
                    }
                }
            }
            None => {
                if let Err(e) = relay.sync_into(&ndb, &filter_json).await {
                    tracing::warn!("engine backfill: REQ fallback failed: {e}");
                }
            }
        }
    }
}

/// The sealed negentropy set of the cached events matching `filter`, keyed by
/// `(created_at, id)` — the local side handed to [`sync::Relay::reconcile`].
///
/// Opens the transaction inline and drops it before returning, so no non-`Send`
/// value escapes into the async caller.
fn local_negentropy_set(ndb: &Ndb, filter: &Filter) -> Result<NegentropyStorageVector, String> {
    let txn = Transaction::new(ndb).map_err(|e| format!("txn: {e}"))?;
    let mut storage = NegentropyStorageVector::new();
    ndb.fold(
        &txn,
        std::slice::from_ref(filter),
        &mut storage,
        |acc, note| {
            // insert only fails on a bad id length, impossible for a stored note.
            let _ = acc.insert(note.created_at(), Id::from_byte_array(*note.id()));
            acc
        },
    )
    .map_err(|e| format!("fold: {e}"))?;
    storage.seal().map_err(|e| format!("seal: {e}"))?;
    Ok(storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{await_note, event_frame, signed_note, spawn_relay, temp_ndb};
    use tempfile::TempDir;

    /// A test [`SubscriptionSpec`] targeting `url`.
    fn spec(url: &str, live: Vec<Filter>, history: Vec<Filter>) -> SubscriptionSpec {
        SubscriptionSpec {
            id: SubscriptionId::new("test", "sub"),
            relay: enostr::NormRelayUrl::new(url).expect("relay url"),
            live_filters: live,
            history_filters: history,
        }
    }

    #[tokio::test]
    async fn open_creates_a_usable_db() {
        let tmp = TempDir::new().expect("tmp dir");
        let engine = Engine::open(tmp.path().to_str().expect("path")).expect("open");
        // A fresh db opens a transaction without error.
        Transaction::new(engine.ndb()).expect("txn");
    }

    #[tokio::test]
    async fn with_ndb_shares_one_database() {
        let (_dir, host) = temp_ndb();

        // The host hands the engine a clone; dropping the engine (which stops the
        // loop) must not tear down the shared db.
        let engine = Engine::with_ndb(host.clone());
        drop(engine);
        Transaction::new(&host).expect("host db still usable after engine drop");
    }

    #[test]
    fn subid_is_stable_and_scoped() {
        let id = SubscriptionId::new("discovery", "inbox");
        assert_eq!(subid(&id), "discovery:inbox");
    }

    /// End-to-end: an [`Engine`] pointed at a real (in-process) relay via
    /// [`Transport::set_subscription`] streams a matching stored event from that
    /// relay into its own db. Exercises the live pool: connect → `REQ` → ingest.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_subscription_streams_events_from_relay() {
        let (_relay_dir, relay_ndb) = temp_ndb();
        let (note_json, id) = signed_note(1, "hello from the relay");
        relay_ndb
            .process_client_event(&event_frame(&note_json))
            .expect("seed relay");
        let relay = spawn_relay(relay_ndb.clone());
        let url = relay.url();

        let eng_dir = TempDir::new().expect("tmp dir");
        let mut engine = Engine::open(eng_dir.path().to_str().expect("path")).expect("engine");
        engine.set_subscription(spec(&url, vec![Filter::new().kinds([1]).build()], vec![]));

        assert!(
            await_note(engine.ndb(), id, 1, Duration::from_secs(10)).await,
            "the relay's event should stream into the engine db"
        );
        relay.shutdown();
    }

    /// End-to-end publish path: one engine `publish_event_json`s an event to a
    /// relay, and a second engine `set_subscription`d to that relay receives it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publish_then_subscribe_round_trip() {
        let (_relay_dir, relay_ndb) = temp_ndb();
        let relay = spawn_relay(relay_ndb.clone());
        let url = relay.url();

        // Engine A publishes a kind-1 event to the relay.
        let a_dir = TempDir::new().expect("tmp dir");
        let mut engine_a = Engine::open(a_dir.path().to_str().expect("path")).expect("engine a");
        let (note_json, id) = signed_note(1, "round trip");
        engine_a.publish_event_json(
            note_json,
            vec![enostr::NormRelayUrl::new(&url).expect("url")],
        );

        // The relay ingests the published event.
        assert!(
            await_note(&relay_ndb, id, 1, Duration::from_secs(10)).await,
            "relay should store the published event"
        );

        // Engine B subscribes and receives it via `REQ` replay.
        let b_dir = TempDir::new().expect("tmp dir");
        let mut engine_b = Engine::open(b_dir.path().to_str().expect("path")).expect("engine b");
        engine_b.set_subscription(spec(&url, vec![Filter::new().kinds([1]).build()], vec![]));

        assert!(
            await_note(engine_b.ndb(), id, 1, Duration::from_secs(10)).await,
            "engine B should receive the published event from the relay"
        );
        relay.shutdown();
    }

    /// End-to-end NIP-77 negentropy backfill: the live filter deliberately does
    /// not match (kind 9999) while the history filter does (kind 1), so the
    /// seeded event can only reach the engine through the negentropy [`backfill`]
    /// reconcile — the relay speaks `NEG-OPEN`, so this hits the real reconcile
    /// path, not the plain-`REQ` fallback.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn negentropy_backfill_pulls_history() {
        let (_relay_dir, relay_ndb) = temp_ndb();
        let (note_json, id) = signed_note(1, "historical");
        relay_ndb
            .process_client_event(&event_frame(&note_json))
            .expect("seed relay");
        let relay = spawn_relay(relay_ndb.clone());
        let url = relay.url();
        // Make sure the seed is queryable before the engine reconciles against it.
        assert!(await_note(&relay_ndb, id, 1, Duration::from_secs(5)).await);

        let eng_dir = TempDir::new().expect("tmp dir");
        let mut engine = Engine::open(eng_dir.path().to_str().expect("path")).expect("engine");
        engine.set_subscription(spec(
            &url,
            vec![Filter::new().kinds([9999]).build()],
            vec![Filter::new().kinds([1]).build()],
        ));

        assert!(
            await_note(engine.ndb(), id, 1, Duration::from_secs(10)).await,
            "the historical event should arrive via the negentropy backfill"
        );
        relay.shutdown();
    }
}
