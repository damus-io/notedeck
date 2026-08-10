//! The agentium engine: an owned [`Ndb`] plus session-protocol orchestration
//! that speaks only to a [`Transport`], so the same engine drops into a desktop
//! host (which brings its own relay stack) or a standalone/iOS process (which
//! uses the engine's own relay-sync loop) with no host application context.
//!
//! # Database ownership
//!
//! The engine must be agnostic to whether it *owns* or *shares* its nostrdb.
//! [`Ndb`] is `#[derive(Clone)]` over an `Arc`-backed handle: a clone is a cheap
//! reference to the *same* database, and the database is torn down only when the
//! last clone drops. So the engine holds an `Ndb` **by value** — no lifetime
//! parameter, no borrowed/owned split — and every constructor converges on that
//! one stored type:
//!
//! - [`Engine::open`] — standalone/iOS: the engine creates its own database and
//!   holds it, and spawns its own relay loop.
//! - [`Engine::with_ndb`] — a standalone engine over a database the caller
//!   already opened; also spawns the relay loop.
//! - [`Engine::embedded`] — an embedding host passes a *clone* of its own
//!   database and drives the relay itself; no loop is spawned. Both point at the
//!   same db, and the engine dropping won't tear down the host's db because the
//!   host still holds a clone.
//!
//! Holding by value is also *required* for the standalone loop, not merely
//! convenient: the reconcile loop runs in a `tokio::spawn`ed task and needs a
//! `'static` handle, so a borrowed `&Ndb` couldn't move into the task — a cloned
//! `Ndb` is exactly right.
//!
//! # Identity
//!
//! The engine is constructed with a single 32-byte `device_key`. From it the
//! account keypair is derived (it signs the inner session events — kinds 1988 /
//! 31988 / 31989) and, via HKDF, the PNS keys (which wrap those events in
//! kind-1080 envelopes for the wire). Two devices sharing a session share this
//! one key, so each can sign as the same author and decrypt the other's
//! envelopes. The key is also registered with the database ([`Ndb::add_key`]) so
//! nostrdb's ingest threads unwrap inbound PNS envelopes into queryable inner
//! events without any per-event work by the engine.
//!
//! # Relay loop
//!
//! A *standalone* engine ([`Engine::open`] / [`Engine::with_ndb`]) `tokio::spawn`s
//! a background task ([`engine_loop`]) that owns a [`RelayPool`] for the live
//! subscription stream and, per subscription, a NIP-77 negentropy [`backfill`]
//! task for bounded history. [`Engine::transport_handle`] hands out an
//! [`EngineTransport`] onto that loop — a thin [`Transport`] front end whose
//! methods enqueue commands the loop drains — which the caller passes into the
//! write/connect methods. Because the loop is spawned, `open`/`with_ndb` must be
//! called from within a Tokio runtime.
//!
//! An *embedded* engine ([`Engine::embedded`]) spawns no loop and needs no
//! runtime: the host already owns a relay stack and passes its own [`Transport`]
//! into the same write/connect methods. Either way the engine only ever speaks
//! to a [`Transport`], so its logic is identical in both settings.
//!
//! The write/connect methods take the [`Transport`] as a `&mut` parameter rather
//! than the engine owning one, so a standalone caller can borrow its own
//! [`EngineTransport`] handle mutably *and* the engine for the same call without
//! aliasing.
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use enostr::pns::PNS_KIND;
use enostr::NormRelayUrl;
use futures_util::StreamExt;
use nostrdb::{Filter, Ndb, SendFilter, SubscriptionStream, Transaction};
use nostrdb_net::relay::sync;
use nostrdb_net::{ClientMessage, RelayPool, RelayStatus, WsEvent, WsMessage};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::interval;

use crate::messages::Message;
use crate::session_events::{AI_CONVERSATION_KIND, AI_SESSION_STATE_KIND};
use crate::session_loader::SessionState;
use crate::transport::{SubscriptionId, SubscriptionSpec, Transport};

/// An error from constructing or driving the [`Engine`].
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Opening the underlying nostrdb failed.
    #[error("nostrdb error: {0}")]
    Ndb(#[from] nostrdb::Error),
    /// The 32-byte device key is not a valid secp256k1 secret (zero or ≥ the
    /// curve order), so no signing identity can be derived from it.
    #[error("invalid device key: not a valid secp256k1 secret")]
    InvalidDeviceKey,
    /// A relay URL was malformed.
    #[error("relay url: {0}")]
    Relay(#[from] enostr::Error),
    /// Building or signing an outbound session event failed.
    #[error("event build: {0}")]
    Build(String),
    /// A permission id was not a valid UUID.
    #[error("invalid permission id")]
    InvalidPermId,
    /// No permission request with the given id is known in the session, so no
    /// response can be linked to it.
    #[error("no known permission request for that id in this session")]
    UnknownPermission,
}

/// How far back the PNS discovery subscription reconciles history when the
/// engine connects to a relay (one week).
const PNS_HISTORY_WINDOW_SECS: u64 = 7 * 86400;

/// Cap on the number of live PNS events the discovery subscription replays.
const PNS_LIVE_LIMIT: u64 = 500;

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
    /// A settle barrier (see [`Engine::wait_for_sync`]). Because this rides the
    /// same FIFO command channel, by the time the loop handles it every
    /// previously-enqueued [`SetSubscription`](EngineCmd::SetSubscription) has
    /// already spawned its backfill and bumped the started count — so the loop
    /// can hand back a [`SyncSettle`] that resolves once exactly those backfills
    /// have completed.
    WaitForSync(oneshot::Sender<SyncSettle>),
}

/// Shared backfill-completion tracker owned by the [`engine_loop`] and observed
/// by [`SyncSettle`] waiters.
///
/// `done` counts every backfill task that has *finished* — success, error, or
/// panic all count, via a drop guard — so a settle wait can never hang on a task
/// that died. `notify` wakes waiters on each completion. The started count is
/// held loop-locally (only the loop spawns backfills), so it needs no atomic.
#[derive(Default)]
struct BackfillProgress {
    done: AtomicU64,
    notify: Notify,
}

/// A one-shot handle that resolves once the initial history backfill(s) have
/// settled, returned by [`Engine::wait_for_sync`].
///
/// It captures a `target` snapshot of how many backfills had been started when
/// the settle barrier reached the loop, and resolves once that many have
/// completed. Deterministic because each backfill's `sync_into` returns only
/// after its received events are queryable — so "settled" means the reconciled
/// history is actually readable, not merely that a timer elapsed.
pub struct SyncSettle {
    progress: Arc<BackfillProgress>,
    target: u64,
}

impl SyncSettle {
    /// Resolve once the tracked backfills have completed. Returns immediately if
    /// none were in flight at the barrier (e.g. a connect with no history
    /// filters, or a relay that never got a subscription).
    pub async fn settled(self) {
        loop {
            // Register for the wakeup *before* re-checking, so a completion that
            // lands between the check and the await is not lost.
            let notified = self.progress.notify.notified();
            if self.progress.done.load(Ordering::Acquire) >= self.target {
                return;
            }
            notified.await;
        }
    }
}

/// The agentium engine.
///
/// See the module docs for db ownership, identity, and the relay loop. Its
/// write/connect methods take a [`Transport`]; a standalone engine hands out one
/// onto its own loop via [`Engine::transport_handle`], and dropping the engine
/// stops that loop (the command channel closes and the task returns).
pub struct Engine {
    ndb: Ndb,
    /// The single device identity. Its secret key signs the inner session
    /// events (kind 1988/31988/31989) and is the root from which the PNS keys
    /// are derived; both devices sharing a session share this key. See the
    /// module docs.
    account: enostr::FullKeypair,
    /// The relay outbound session events publish to, and the target of the PNS
    /// discovery subscription when the engine installs its own (via
    /// [`Engine::connect`]). An embedding host that manages its own subscription
    /// sets just this via [`Engine::set_relay`]. `None` means "ingest locally,
    /// don't publish".
    pns_relay: Option<NormRelayUrl>,
    /// The command channel to the self-driving relay loop, present only for a
    /// standalone engine ([`Engine::open`] / [`Engine::with_ndb`]). An embedded
    /// engine ([`Engine::embedded`]) has no loop — its host drives the relay and
    /// passes its own [`Transport`] into the write/connect methods — so this is
    /// `None`. [`Engine::transport_handle`] exposes an owned handle onto it.
    cmd_tx: Option<mpsc::UnboundedSender<EngineCmd>>,
}

impl Engine {
    /// Open a standalone engine over its own nostrdb at `path` (created if
    /// absent). Use this on a host that has no existing database of its own.
    /// Must be called from within a Tokio runtime (spawns the relay loop).
    ///
    /// `device_key` is the 32-byte account secret — see [`Engine::with_ndb`].
    pub fn open(path: &str, device_key: [u8; 32]) -> Result<Self, EngineError> {
        let ndb = Ndb::new(path, &nostrdb::Config::new())?;
        Self::with_ndb(ndb, device_key)
    }

    /// Build an engine over an existing database, taking a cheap [`Ndb`] clone.
    /// Use this on an embedding host: pass `host_ndb.clone()` so the engine and
    /// host share one database and neither's drop tears it down while the other
    /// still holds a handle. Must be called from within a Tokio runtime (spawns
    /// the relay loop).
    ///
    /// `device_key` is the 32-byte account secret. It is registered with the
    /// database via [`Ndb::add_key`] so nostrdb's ingest threads decrypt inbound
    /// kind-1080 PNS envelopes into their queryable inner events, and it is the
    /// engine's signing/derivation identity. Fails with
    /// [`EngineError::InvalidDeviceKey`] if the bytes are not a valid secp256k1
    /// secret.
    pub fn with_ndb(ndb: Ndb, device_key: [u8; 32]) -> Result<Self, EngineError> {
        let mut engine = Self::embedded(ndb.clone(), device_key)?;
        // Spawn the self-driving relay loop and keep its command channel so
        // `transport_handle` can hand out owned handles onto it.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(engine_loop(ndb, cmd_rx));
        engine.cmd_tx = Some(cmd_tx);
        Ok(engine)
    }

    /// Build an engine over an existing database *without* a relay loop, for an
    /// embedding host that already owns a relay stack. Reads work immediately;
    /// the host drives sync itself and passes its own [`Transport`] into the
    /// write/connect methods (and calls [`Engine::set_relay`] to point publishes
    /// at its chosen relay). Unlike [`Engine::with_ndb`] this needs no Tokio
    /// runtime and spawns no thread.
    ///
    /// `device_key` is the 32-byte account secret, registered with the database
    /// via [`Ndb::add_key`] and used as the signing/derivation identity — see
    /// [`Engine::with_ndb`]. Fails with [`EngineError::InvalidDeviceKey`] if the
    /// bytes are not a valid secp256k1 secret.
    pub fn embedded(ndb: Ndb, device_key: [u8; 32]) -> Result<Self, EngineError> {
        let account = enostr::FullKeypair::from_secret_bytes(&device_key)
            .ok_or(EngineError::InvalidDeviceKey)?;
        // Register the key so ndb can decrypt inbound PNS envelopes in-thread.
        // A `false` return means the key was already present, which is benign.
        if !ndb.add_key(&device_key) {
            tracing::debug!("engine: device key already registered with ndb");
        }
        Ok(Self {
            ndb,
            account,
            pns_relay: None,
            cmd_tx: None,
        })
    }

    /// An owned [`Transport`] handle onto this engine's self-driving relay loop,
    /// or `None` for an [`embedded`](Engine::embedded) engine (which has no
    /// loop). Standalone callers pass `&mut` this handle into the write/connect
    /// methods; taking an owned handle (a clone of the loop's command channel)
    /// avoids aliasing the `&mut self` those methods also need.
    pub fn transport_handle(&self) -> Option<EngineTransport> {
        self.cmd_tx.clone().map(|cmd_tx| EngineTransport { cmd_tx })
    }

    /// The engine's database handle.
    pub fn ndb(&self) -> &Ndb {
        &self.ndb
    }

    /// The account public key that signs this engine's session events (and that
    /// its author-scoped reads filter on).
    pub fn account_pubkey(&self) -> enostr::Pubkey {
        self.account.pubkey
    }

    /// The PNS keys (kind-1080 signing keypair + NIP-44 conversation key)
    /// derived from the device key. Used to author the discovery subscription
    /// and to wrap/unwrap session events on the wire.
    fn pns_keys(&self) -> enostr::pns::PnsKeys {
        enostr::pns::derive_pns_keys(&self.account.secret_key.secret_bytes())
    }

    /// Connect to a relay for remote sync.
    ///
    /// Installs the single PNS discovery subscription — kind-1080 events authored
    /// by this identity's PNS pubkey — which streams the whole identity's
    /// encrypted corpus (every session's state and conversation) into the
    /// database, where ndb's ingest threads decrypt it into queryable inner
    /// events. The same relay becomes the publish target for outbound events.
    /// Reconnecting to a different relay just re-points the subscription.
    ///
    /// `transport` is where the subscription is declared: a standalone engine
    /// passes its own [`transport_handle`](Engine::transport_handle); an
    /// embedding host that wants the engine to own the discovery subscription
    /// passes its host transport. A host that manages that subscription itself
    /// should skip `connect` and call [`Engine::set_relay`] instead.
    pub fn connect(
        &mut self,
        transport: &mut impl Transport,
        relay_url: &str,
    ) -> Result<(), EngineError> {
        let relay = NormRelayUrl::new(relay_url)?;
        let pns_author = self.pns_keys().keypair.pubkey;
        let since = now_secs().saturating_sub(PNS_HISTORY_WINDOW_SECS);

        let live = Filter::new()
            .kinds([PNS_KIND as u64])
            .authors([pns_author.bytes()])
            .limit(PNS_LIVE_LIMIT)
            .build();
        let history = Filter::new()
            .kinds([PNS_KIND as u64])
            .authors([pns_author.bytes()])
            .since(since)
            .build();

        transport.set_subscription(SubscriptionSpec {
            id: pns_discovery_sub_id(),
            relay: relay.clone(),
            live_filters: vec![live],
            history_filters: vec![history],
        });
        self.pns_relay = Some(relay);
        Ok(())
    }

    /// Point outbound publishes at `relay` without installing a subscription.
    ///
    /// For an embedding host that runs its own PNS discovery subscription: the
    /// engine still needs to know where to publish the events its write methods
    /// produce. `None` reverts to "ingest locally, don't publish".
    pub fn set_relay(&mut self, relay: Option<NormRelayUrl>) {
        self.pns_relay = relay;
    }

    /// Tear down the discovery subscription (via `transport`) and forget the
    /// relay. Outbound events built after this are ingested locally but not
    /// published until the next [`Engine::connect`].
    pub fn disconnect(&mut self, transport: &mut impl Transport) {
        transport.drop_subscription(&pns_discovery_sub_id());
        self.pns_relay = None;
    }

    /// The relay the engine is currently connected to, if any.
    pub fn connected_relay(&self) -> Option<String> {
        self.pns_relay.as_ref().map(|r| r.to_string())
    }

    /// Await the initial history reconcile settling — the deterministic
    /// replacement for a caller guessing with a quiet-timer heuristic.
    ///
    /// Call it right after [`Engine::connect`]: the connect installs the PNS
    /// discovery subscription whose history filters spawn a NIP-77 negentropy
    /// [`backfill`], and the returned future resolves once that backfill (and any
    /// other in flight when this is called) has completed — meaning the
    /// reconciled events are actually queryable, since each `sync_into` returns
    /// only after its received events are ingested. Read the session snapshot
    /// once this resolves and you see the whole synced batch, not a race with it.
    ///
    /// Ordering is exact, not best-effort: the settle barrier rides the same FIFO
    /// command channel as `connect`'s subscription, so by the time the loop
    /// handles it the backfill has already been counted. Resolves immediately for
    /// a connect that requested no history, or once the relay's history is in.
    ///
    /// Returns `None` for an [`embedded`](Engine::embedded) engine — it has no
    /// self-driving loop, so its host owns sync and observes settle its own way.
    /// Standalone callers should still bound the wait with a timeout, since a
    /// reachable-but-silent relay could otherwise stall the reconcile.
    pub async fn wait_for_sync(&self) -> Option<()> {
        let cmd_tx = self.cmd_tx.as_ref()?;
        let (tx, rx) = oneshot::channel();
        // A send error means the loop is gone (engine torn down); a recv error
        // means it dropped the reply before answering. Either way there is
        // nothing to wait on, so treat both as "no settle signal available".
        cmd_tx.send(EngineCmd::WaitForSync(tx)).ok()?;
        rx.await.ok()?.settled().await;
        Some(())
    }

    /// List the remote sessions known to this identity, newest revision of each
    /// kind-31988 state event (deleted and legacy-format events excluded).
    pub fn list_sessions(&self) -> Vec<SessionState> {
        let Ok(txn) = Transaction::new(&self.ndb) else {
            return Vec::new();
        };
        crate::session_loader::load_session_states_for_author(&self.ndb, &txn, &self.account.pubkey)
    }

    /// The conversation for one session as an ordered list of messages,
    /// reconstructed from its kind-1988 events (seq-ordered, permission state
    /// merged). Returns empty if the session is unknown.
    pub fn session_messages(&self, session_id: &str) -> Vec<Message> {
        let Ok(txn) = Transaction::new(&self.ndb) else {
            return Vec::new();
        };
        crate::session_loader::load_session_messages_for_author(
            &self.ndb,
            &txn,
            &self.account.pubkey,
            session_id,
        )
        .messages
    }

    /// Watch a session for live changes.
    ///
    /// Returns a [`SessionWatch`] whose [`SessionWatch::changed`] resolves each
    /// time a new kind-1988 event for the session lands in the database. The
    /// intended loop is *wait, then re-read the snapshot*:
    ///
    /// ```ignore
    /// let mut watch = engine.watch_session(id)?;
    /// while watch.changed().await {
    ///     render(engine.session_messages(id));
    /// }
    /// ```
    pub fn watch_session(&self, session_id: &str) -> Result<SessionWatch, EngineError> {
        let filter = Filter::new()
            .kinds([AI_CONVERSATION_KIND as u64])
            .authors([self.account.pubkey.bytes()])
            .tags([session_id], 'd')
            .build();
        let sub = self.ndb.subscribe(std::slice::from_ref(&filter))?;
        Ok(SessionWatch {
            stream: SubscriptionStream::new(self.ndb.clone(), sub),
        })
    }

    /// Watch the session list for changes.
    ///
    /// Returns a [`SessionWatch`] whose [`SessionWatch::changed`] resolves each
    /// time a kind-31988 session-state event arrives or is replaced — i.e. a new
    /// session appears or an existing one's status changes. Re-read the list with
    /// [`Engine::list_sessions`] on each wake.
    pub fn watch_sessions(&self) -> Result<SessionWatch, EngineError> {
        let filter = Filter::new()
            .kinds([AI_SESSION_STATE_KIND as u64])
            .authors([self.account.pubkey.bytes()])
            .build();
        let sub = self.ndb.subscribe(std::slice::from_ref(&filter))?;
        Ok(SessionWatch {
            stream: SubscriptionStream::new(self.ndb.clone(), sub),
        })
    }

    /// Send a user message into a session.
    ///
    /// Builds a kind-1988 `user` event threaded onto the session's existing
    /// conversation and publishes it. Works whether or not the session is known
    /// locally (a brand-new session simply starts a fresh thread). `transport`
    /// carries the PNS envelope to the relay — see [`Engine::transport_handle`].
    pub fn send_message(
        &self,
        transport: &mut impl Transport,
        session_id: &str,
        text: &str,
    ) -> Result<(), EngineError> {
        let built = self.make_user_message(session_id, text)?;
        self.publish_session_event(transport, built)
    }

    /// Build a kind-1988 user message, ingest it locally, and return it for the
    /// caller to publish through its own transport — for a host that batches its
    /// own relay writes. Unlike [`send_message`](Engine::send_message), this does
    /// not publish.
    pub fn prepare_message(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let built = self.make_user_message(session_id, text)?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Build the inner kind-1988 `user` event threaded onto the session's
    /// existing conversation (no ingest, no publish). A brand-new session simply
    /// starts a fresh thread.
    fn make_user_message(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let mut threading = self.session_threading(session_id);
        let cwd = self.session_cwd(session_id);
        crate::session_events::build_live_event(
            text,
            "user",
            session_id,
            cwd.as_deref(),
            None,
            None,
            &mut threading,
            &self.seckey(),
        )
        .map_err(|e| EngineError::Build(e.to_string()))
    }

    /// Spawn a new session on a remote host.
    ///
    /// Publishes a fire-and-forget kind-31989 spawn command; the target host
    /// discovers it, creates the session, and publishes back a kind-31988 state
    /// event that later shows up in [`Engine::list_sessions`]. Returns the
    /// `spawn_id` that links this request to that eventual state.
    pub fn spawn_session(
        &self,
        transport: &mut impl Transport,
        target_host: &str,
        cwd: &str,
        backend: &str,
    ) -> Result<String, EngineError> {
        let spawn_id = uuid::Uuid::new_v4().to_string();
        let built = self.make_spawn_command(target_host, cwd, backend, &spawn_id)?;
        self.publish_session_event(transport, built)?;
        Ok(spawn_id)
    }

    /// Build a kind-31989 spawn command, ingest it locally, and return it for
    /// the caller to publish through its own transport — for a host that batches
    /// its own relay writes. Unlike [`spawn_session`](Engine::spawn_session), the
    /// caller supplies `spawn_id` (so it can correlate the eventual kind-31988
    /// state) and publishes the returned event itself; nothing is sent here.
    pub fn prepare_spawn_command(
        &self,
        target_host: &str,
        cwd: &str,
        backend: &str,
        spawn_id: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let built = self.make_spawn_command(target_host, cwd, backend, spawn_id)?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Build the inner kind-31989 spawn-command event (no ingest, no publish).
    fn make_spawn_command(
        &self,
        target_host: &str,
        cwd: &str,
        backend: &str,
        spawn_id: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        crate::session_events::build_spawn_command_event(
            target_host,
            cwd,
            backend,
            spawn_id,
            &self.seckey(),
        )
        .map_err(|e| EngineError::Build(e.to_string()))
    }

    /// Respond to a pending permission request.
    ///
    /// Resolves the request's note id from the session's events, then publishes a
    /// kind-1988 permission response linked to it. `cancel_turn` denies *and*
    /// interrupts the current turn. Errors with [`EngineError::UnknownPermission`]
    /// if no request with `perm_id` is present in the session.
    pub fn respond_permission(
        &self,
        transport: &mut impl Transport,
        session_id: &str,
        perm_id: &str,
        allow: bool,
        message: Option<String>,
        cancel_turn: bool,
    ) -> Result<(), EngineError> {
        let built = self.make_permission_response(
            session_id,
            perm_id,
            allow,
            message.as_deref(),
            cancel_turn,
        )?;
        self.publish_session_event(transport, built)
    }

    /// Build a kind-1988 permission response, ingest it locally, and return it
    /// for the caller to publish through its own transport — for a host that
    /// batches its own relay writes. Unlike
    /// [`respond_permission`](Engine::respond_permission), this does not publish.
    /// Question-set answers ride in `message` as a pre-formatted payload (see
    /// [`respond_question`](Engine::respond_question) for that payload's shape).
    pub fn prepare_permission_response(
        &self,
        session_id: &str,
        perm_id: &str,
        allow: bool,
        message: Option<&str>,
        cancel_turn: bool,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let built =
            self.make_permission_response(session_id, perm_id, allow, message, cancel_turn)?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Resolve the request's note id from the session's events and build the
    /// inner kind-1988 permission-response event (no ingest, no publish). Errors
    /// with [`EngineError::UnknownPermission`] if no request with `perm_id` is
    /// present in the session.
    fn make_permission_response(
        &self,
        session_id: &str,
        perm_id: &str,
        allow: bool,
        message: Option<&str>,
        cancel_turn: bool,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let perm_uuid = uuid::Uuid::parse_str(perm_id).map_err(|_| EngineError::InvalidPermId)?;

        let request_note_id = {
            let txn = Transaction::new(&self.ndb)?;
            crate::session_loader::load_session_messages_for_author(
                &self.ndb,
                &txn,
                &self.account.pubkey,
                session_id,
            )
            .permissions
            .request_note_ids
            .get(&perm_uuid)
            .copied()
        };
        let Some(request_note_id) = request_note_id else {
            return Err(EngineError::UnknownPermission);
        };

        let mut threading = self.session_threading(session_id);
        crate::session_events::build_permission_response_event(
            &perm_uuid,
            &request_note_id,
            allow,
            message,
            cancel_turn,
            session_id,
            &mut threading,
            &self.seckey(),
        )
        .map_err(|e| EngineError::Build(e.to_string()))
    }

    /// Respond to a pending question-set permission request.
    ///
    /// Some permission requests aren't a plain allow/deny but a structured
    /// [`AskUserQuestion`](crate::messages::PermissionView::QuestionSet) prompt —
    /// one or more questions, each with selectable options. This resolves the
    /// request's note id, maps each answer's selected option *indices* back to
    /// their option *labels*, and publishes an approving kind-1988 response whose
    /// message carries the `{"answers": {header: {selected:[…], other:…}}}`
    /// payload the host decodes as the tool result.
    ///
    /// `answers` is positional: `answers[i]` answers the request's `i`th question.
    /// Errors with [`EngineError::UnknownPermission`] if no request with
    /// `request_id` is present in the session.
    pub fn respond_question(
        &self,
        transport: &mut impl Transport,
        session_id: &str,
        request_id: &str,
        answers: Vec<crate::messages::QuestionAnswer>,
    ) -> Result<(), EngineError> {
        let perm_uuid =
            uuid::Uuid::parse_str(request_id).map_err(|_| EngineError::InvalidPermId)?;

        let (request_note_id, payload) = {
            let txn = Transaction::new(&self.ndb)?;
            let loaded = crate::session_loader::load_session_messages_for_author(
                &self.ndb,
                &txn,
                &self.account.pubkey,
                session_id,
            );
            let Some(request_note_id) =
                loaded.permissions.request_note_ids.get(&perm_uuid).copied()
            else {
                return Err(EngineError::UnknownPermission);
            };
            // Pull the option labels off the original request so selected indices
            // can be resolved to human-readable answers.
            let questions = loaded.messages.iter().find_map(|msg| match msg {
                Message::PermissionRequest(req) if req.id == perm_uuid => req.view.question_set(),
                _ => None,
            });
            (
                request_note_id,
                crate::messages::format_question_answers(questions, &answers),
            )
        };

        let mut threading = self.session_threading(session_id);
        let built = crate::session_events::build_permission_response_event(
            &perm_uuid,
            &request_note_id,
            true,
            Some(&payload),
            false,
            session_id,
            &mut threading,
            &self.seckey(),
        )
        .map_err(|e| EngineError::Build(e.to_string()))?;
        self.publish_session_event(transport, built)
    }

    /// Request a permission-mode change on a session's host (e.g. `"default"`,
    /// `"acceptEdits"`, `"plan"`). Publishes a kind-1988 command the host applies
    /// to its local backend.
    pub fn set_permission_mode(
        &self,
        transport: &mut impl Transport,
        session_id: &str,
        mode: &str,
    ) -> Result<(), EngineError> {
        let built = self.make_set_permission_mode(session_id, mode)?;
        self.publish_session_event(transport, built)
    }

    /// Build a kind-1988 set-permission-mode command, ingest it locally, and
    /// return it for the caller to publish through its own transport — for a host
    /// that batches its own relay writes. Unlike
    /// [`set_permission_mode`](Engine::set_permission_mode), this does not publish.
    pub fn prepare_set_permission_mode(
        &self,
        session_id: &str,
        mode: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let built = self.make_set_permission_mode(session_id, mode)?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Build the inner kind-1988 set-permission-mode event (no ingest, no publish).
    fn make_set_permission_mode(
        &self,
        session_id: &str,
        mode: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let mut threading = self.session_threading(session_id);
        crate::session_events::build_set_permission_mode_event(
            mode,
            session_id,
            &mut threading,
            &self.seckey(),
        )
        .map_err(|e| EngineError::Build(e.to_string()))
    }

    /// The 32-byte account secret that signs inner session events.
    fn seckey(&self) -> [u8; 32] {
        self.account.secret_key.secret_bytes()
    }

    /// Seed a [`ThreadingState`](crate::session_events::ThreadingState) from a
    /// session's existing events so a new live event threads onto the chain.
    /// Returns a fresh state for an unknown/empty session.
    fn session_threading(&self, session_id: &str) -> crate::session_events::ThreadingState {
        let mut threading = crate::session_events::ThreadingState::new();
        let Ok(txn) = Transaction::new(&self.ndb) else {
            return threading;
        };
        let loaded = crate::session_loader::load_session_messages_for_author(
            &self.ndb,
            &txn,
            &self.account.pubkey,
            session_id,
        );
        if let (Some(root), Some(last)) = (loaded.root_note_id, loaded.last_note_id) {
            threading.seed(root, last);
        }
        threading
    }

    /// The session's working directory from its latest state event, if any.
    fn session_cwd(&self, session_id: &str) -> Option<String> {
        let txn = Transaction::new(&self.ndb).ok()?;
        let state = crate::session_loader::latest_valid_session_for_author(
            &self.ndb,
            &txn,
            &self.account.pubkey,
            session_id,
        )?;
        (!state.cwd.is_empty()).then_some(state.cwd)
    }

    /// Wrap a freshly-built inner event in its PNS envelope and ingest it locally
    /// (ndb decrypts it back into the queryable inner event), returning the
    /// envelope JSON so a caller can also publish it. Ingesting our own event
    /// makes local reads reflect it immediately, exactly as an inbound relay
    /// envelope would.
    fn wrap_and_ingest(
        &self,
        built: &crate::session_events::BuiltEvent,
    ) -> Result<String, EngineError> {
        let pns = self.pns_keys();
        let wrapped = crate::session_events::wrap_pns(&built.note_json, &pns)
            .map_err(|e| EngineError::Build(e.to_string()))?;
        if let Err(e) = self
            .ndb
            .process_event(&format!(r#"["EVENT","_pns",{wrapped}]"#))
        {
            tracing::warn!("engine: failed to ingest own event: {e}");
        }
        Ok(wrapped)
    }

    /// Wrap, ingest locally, and publish a freshly-built inner event. Local
    /// ingest always happens; publishing is deferred if no publish relay is set.
    fn publish_session_event(
        &self,
        transport: &mut impl Transport,
        built: crate::session_events::BuiltEvent,
    ) -> Result<(), EngineError> {
        let wrapped = self.wrap_and_ingest(&built)?;
        match self.pns_relay.clone() {
            Some(relay) => transport.publish_event_json(wrapped, vec![relay]),
            None => tracing::debug!("engine: no publish relay set; event ingested locally only"),
        }
        Ok(())
    }
}

/// An event-driven waiter over an ndb subscription — a session's live kind-1988
/// events ([`Engine::watch_session`]) or the kind-31988 session list
/// ([`Engine::watch_sessions`]).
///
/// Backed by an ndb subscription stream, so [`SessionWatch::changed`] only wakes
/// on a real new event (no polling). The subscription is released when the watch
/// is dropped. Pair it with a snapshot read ([`Engine::session_messages`] /
/// [`Engine::list_sessions`]) to re-read on each change.
pub struct SessionWatch {
    stream: SubscriptionStream,
}

impl SessionWatch {
    /// Wait for the next change. Resolves `true` when new events arrived (re-read
    /// the snapshot); `false` if the subscription ended (e.g. the database was
    /// torn down).
    pub async fn changed(&mut self) -> bool {
        self.stream.next().await.is_some()
    }
}

/// The stable transport subscription id for the PNS discovery subscription.
fn pns_discovery_sub_id() -> SubscriptionId {
    SubscriptionId::new("agentium/pns", "discovery")
}

/// The current Unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

/// An owned [`Transport`] onto a standalone [`Engine`]'s self-driving relay
/// loop, produced by [`Engine::transport_handle`].
///
/// It holds a clone of the loop's command channel, so it is `Send`, cheap to
/// create, and — being a value distinct from the engine — can be borrowed
/// `&mut` while the engine is borrowed for the same write call (the aliasing an
/// `impl Transport for Engine` would otherwise cause). Every method just
/// enqueues a command the loop drains, so nothing blocks.
pub struct EngineTransport {
    cmd_tx: mpsc::UnboundedSender<EngineCmd>,
}

impl Transport for EngineTransport {
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

    let mut state = LoopState::default();
    let mut keepalive = interval(KEEPALIVE_INTERVAL);

    loop {
        // Drain everything the pool has ready: ingest events, and flush desired
        // subs / queued publishes to relays as they come up.
        while let Some(ev) = state.pool.try_recv().map(|e| e.into_owned()) {
            match ev.event {
                WsEvent::Opened => {
                    if let Some(subs) = state.desired.get(&ev.relay) {
                        for (sid, filters) in subs {
                            state
                                .pool
                                .send_to(&req_message(sid.clone(), filters), &ev.relay);
                        }
                    }
                    if let Some(frames) = state.pending.remove(&ev.relay) {
                        for frame in frames {
                            state.pool.send_to(&ClientMessage::raw(frame), &ev.relay);
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
                apply_cmd(&ndb, &mut state, &wakeup, cmd);
            }
            // The pool signalled new data; loop back around to drain it.
            _ = notify.notified() => {}
            _ = keepalive.tick() => state.pool.keepalive_ping(wakeup.clone()),
        }
    }
}

/// The mutable state the [`engine_loop`] threads through each [`apply_cmd`]:
/// the relay pool, the desired-subscription and pending-publish maps, and the
/// backfill settle tracker. Bundled into one struct so commands are applied by
/// `&mut LoopState` rather than a long argument list.
#[derive(Default)]
struct LoopState {
    pool: RelayPool,
    /// Desired live subscriptions per relay url (subid -> filters), re-sent
    /// whenever a relay (re)connects.
    desired: HashMap<String, HashMap<String, Vec<SendFilter>>>,
    /// Publish frames queued until their target relay is connected.
    pending: HashMap<String, Vec<String>>,
    /// Backfill settle tracking (see [`Engine::wait_for_sync`]): the shared
    /// done-counter the detached backfill tasks bump, paired with `started`.
    progress: Arc<BackfillProgress>,
    /// Count of backfills spawned so far, snapshotted by a `WaitForSync` barrier
    /// as its settle target (against `progress.done`).
    started: u64,
}

/// Apply one [`EngineCmd`] against the loop's [`LoopState`].
fn apply_cmd(
    ndb: &Ndb,
    state: &mut LoopState,
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
            let _ = state.pool.add_url(url.clone(), wakeup.clone());
            // Send the live REQ now if the relay is already up; otherwise it is
            // flushed from `desired` on the relay's `Opened` event.
            if relay_connected(&state.pool, &url) {
                state.pool.send_to(&req_message(sid.clone(), &live), &url);
            }
            state
                .desired
                .entry(url.clone())
                .or_default()
                .insert(sid, live);
            // NIP-77 negentropy history backfill, off the loop on its own task.
            // Track its completion (via a drop guard so a panic still counts)
            // so a `WaitForSync` barrier can observe when history has settled.
            if !history.is_empty() {
                state.started += 1;
                let ndb = ndb.clone();
                let progress = state.progress.clone();
                tokio::spawn(async move {
                    let _guard = BackfillDoneGuard(progress);
                    backfill(ndb, url, history).await;
                });
            }
        }
        EngineCmd::DropSubscription(id) => {
            let sid = subid(&id);
            for subs in state.desired.values_mut() {
                subs.remove(&sid);
            }
            state.pool.unsubscribe(sid);
        }
        EngineCmd::Publish { note_json, relays } => {
            let frame = format!(r#"["EVENT",{note_json}]"#);
            for url in relays {
                let _ = state.pool.add_url(url.clone(), wakeup.clone());
                if relay_connected(&state.pool, &url) {
                    state.pool.send_to(&ClientMessage::raw(frame.clone()), &url);
                } else {
                    state.pending.entry(url).or_default().push(frame.clone());
                }
            }
        }
        EngineCmd::WaitForSync(reply) => {
            // Snapshot how many backfills have been started by now; the returned
            // handle resolves once that many have completed. Because this command
            // rode the FIFO channel behind every prior `SetSubscription`, that
            // snapshot already includes their backfills. A dropped `reply` (the
            // waiter went away) is harmless.
            let _ = reply.send(SyncSettle {
                progress: state.progress.clone(),
                target: state.started,
            });
        }
    }
}

/// Drop guard that records a backfill task's completion on the shared
/// [`BackfillProgress`]. Using `Drop` (rather than a bump at the end of the
/// task body) means an error return or a panic unwinding through the task still
/// counts as done, so a [`SyncSettle`] wait can never hang on a dead backfill.
struct BackfillDoneGuard(Arc<BackfillProgress>);

impl Drop for BackfillDoneGuard {
    fn drop(&mut self) {
        self.0.done.fetch_add(1, Ordering::Release);
        self.0.notify.notify_waiters();
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
    // Reduce each filter to its wire JSON and sealed local set *synchronously* —
    // the transient `nostrdb::Filter` from `as_filter()` drops at each `;`, so it
    // never crosses an await — then hand both to `pull_reconcile`, whose future
    // stays `Send` precisely because it takes no `Filter`.
    for filter in filters {
        let Ok(filter_json) = filter.as_filter().json() else {
            continue;
        };
        let local = match sync::local_set(&ndb, filter.as_filter()) {
            Ok(local) => local,
            Err(e) => {
                tracing::warn!("engine backfill: local set failed: {e}");
                continue;
            }
        };
        if let Err(e) = sync::pull_reconcile(&mut relay, &ndb, &filter_json, local).await {
            tracing::warn!("engine backfill: {url}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        await_note, event_frame, signed_note, spawn_relay, temp_ndb, TEST_SECKEY,
    };
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

    /// PNS-wrap `inner_json` under the [`TEST_SECKEY`] identity and ingest the
    /// kind-1080 envelope, mirroring how an inbound relay event reaches ndb: the
    /// engine's registered device key lets ndb decrypt it into the queryable
    /// inner event.
    fn pns_seed(ndb: &Ndb, inner_json: &str) {
        let pns = enostr::pns::derive_pns_keys(&TEST_SECKEY);
        let wrapped = crate::session_events::wrap_pns(inner_json, &pns).expect("wrap pns");
        ndb.process_event(&format!(r#"["EVENT","_seed",{wrapped}]"#))
            .expect("ingest pns envelope");
    }

    /// PNS-wrap a freshly-built inner event and publish its envelope through
    /// `tx` to `relay`. Models both a host injecting a backend-produced event and
    /// the controller's "prepare locally, then publish from my own queue" flow —
    /// the `prepare_*` methods ingest but don't publish, so the caller wraps the
    /// returned inner event and sends it itself, exactly as dave's drain does.
    fn publish_prepared(
        tx: &mut impl Transport,
        built: &crate::session_events::BuiltEvent,
        relay: &str,
    ) {
        let pns = enostr::pns::derive_pns_keys(&TEST_SECKEY);
        let wrapped = crate::session_events::wrap_pns(&built.note_json, &pns).expect("wrap pns");
        tx.publish_event_json(
            wrapped,
            vec![enostr::NormRelayUrl::new(relay).expect("relay url")],
        );
    }

    /// The `message` field from the newest `permission_response` event on
    /// `session_id` in `ndb`, i.e. the answer payload `respond_question` /
    /// `respond_permission` publishes. `None` if no response is present.
    fn latest_permission_response_message(ndb: &Ndb, session_id: &str) -> Option<String> {
        let txn = Transaction::new(ndb).expect("txn");
        let filter = Filter::new()
            .kinds([AI_CONVERSATION_KIND as u64])
            .tags([session_id], 'd')
            .build();
        let results = ndb.query(&txn, &[filter], 100).expect("query");
        // Newest first: query returns created_at-descending, so take the first
        // note tagged as a permission_response.
        for result in results {
            let note = &result.note;
            if crate::session_events::get_tag_value(note, "role") != Some("permission_response") {
                continue;
            }
            let content: serde_json::Value = serde_json::from_str(note.content()).ok()?;
            return content
                .get("message")
                .and_then(|m| m.as_str())
                .map(ToOwned::to_owned);
        }
        None
    }

    /// The read API surfaces a session and its conversation once the PNS
    /// envelopes carrying them have been decrypted into the engine's db.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_api_lists_and_reads_a_session() {
        use crate::session_events::{
            build_live_event, build_session_state_event, ThreadingState, AI_SESSION_STATE_KIND,
        };

        let eng_dir = TempDir::new().expect("tmp dir");
        let engine =
            Engine::open(eng_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");

        let session_id = "read-test-session";
        let state = build_session_state_event(
            session_id,
            "My Session",
            None,
            "/tmp",
            "idle",
            None,
            "host",
            "/home",
            "claude",
            "default",
            None,
            None,
            1_770_000_000,
            &TEST_SECKEY,
        )
        .expect("state event");
        pns_seed(engine.ndb(), &state.note_json);

        let mut threading = ThreadingState::new();
        let msg = build_live_event(
            "hello there",
            "user",
            session_id,
            Some("/tmp"),
            None,
            None,
            &mut threading,
            &TEST_SECKEY,
        )
        .expect("live event");
        pns_seed(engine.ndb(), &msg.note_json);

        // Both inner events must decrypt and index before we read them.
        assert!(
            await_note(
                engine.ndb(),
                state.note_id,
                AI_SESSION_STATE_KIND as u64,
                Duration::from_secs(5)
            )
            .await,
            "session-state event should decrypt into the db"
        );
        assert!(
            await_note(
                engine.ndb(),
                msg.note_id,
                AI_CONVERSATION_KIND as u64,
                Duration::from_secs(5)
            )
            .await,
            "conversation event should decrypt into the db"
        );

        let sessions = engine.list_sessions();
        assert_eq!(sessions.len(), 1, "one session should be listed");
        assert_eq!(sessions[0].claude_session_id, session_id);
        assert_eq!(sessions[0].title, "My Session");

        let messages = engine.session_messages(session_id);
        assert_eq!(messages.len(), 1, "one conversation message");
        assert!(matches!(&messages[0], Message::User(_)));
    }

    /// [`SessionWatch::changed`] wakes when a new event for the watched session
    /// is decrypted into the db.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_session_wakes_on_new_event() {
        use crate::session_events::{build_live_event, ThreadingState};

        let eng_dir = TempDir::new().expect("tmp dir");
        let engine =
            Engine::open(eng_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");

        let session_id = "watch-test-session";
        // Subscribe before seeding so the stream catches the arrival.
        let mut watch = engine.watch_session(session_id).expect("watch");

        let mut threading = ThreadingState::new();
        let msg = build_live_event(
            "ping",
            "user",
            session_id,
            None,
            None,
            None,
            &mut threading,
            &TEST_SECKEY,
        )
        .expect("live event");
        pns_seed(engine.ndb(), &msg.note_json);

        let woke = tokio::time::timeout(Duration::from_secs(5), watch.changed())
            .await
            .expect("watch should wake before timeout");
        assert!(woke, "watch should report a change");
        assert_eq!(engine.session_messages(session_id).len(), 1);
    }

    /// [`Engine::watch_sessions`] wakes when a new kind-31988 session state is
    /// decrypted into the db, so the list can be re-read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_sessions_wakes_on_new_session() {
        use crate::session_events::build_session_state_event;

        let eng_dir = TempDir::new().expect("tmp dir");
        let engine =
            Engine::open(eng_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");

        let mut watch = engine.watch_sessions().expect("watch");
        let state = build_session_state_event(
            "new-session",
            "A Session",
            None,
            "/tmp",
            "idle",
            None,
            "host",
            "/home",
            "claude",
            "default",
            None,
            None,
            1_770_000_000,
            &TEST_SECKEY,
        )
        .expect("state event");
        pns_seed(engine.ndb(), &state.note_json);

        let woke = tokio::time::timeout(Duration::from_secs(5), watch.changed())
            .await
            .expect("watch should wake before timeout");
        assert!(woke, "watch should report a change");
        assert_eq!(engine.list_sessions().len(), 1);
    }

    /// The flagship remote flow: a message sent by one engine reaches a second
    /// engine that shares the identity and relay. Exercises the whole write path
    /// (build → PNS-wrap → publish) against the read path (discovery sub →
    /// decrypt → session_messages).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_message_round_trips_between_engines() {
        let (_relay_dir, relay_ndb) = temp_ndb();
        let relay = spawn_relay(relay_ndb.clone());
        let url = relay.url();
        let session_id = "round-trip-session";

        let a_dir = TempDir::new().expect("tmp dir");
        let mut a =
            Engine::open(a_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine a");
        let mut a_tx = a.transport_handle().expect("a transport");
        a.connect(&mut a_tx, &url).expect("a connect");
        a.send_message(&mut a_tx, session_id, "hi from A")
            .expect("send");

        let b_dir = TempDir::new().expect("tmp dir");
        let mut b =
            Engine::open(b_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine b");
        let mut b_tx = b.transport_handle().expect("b transport");
        b.connect(&mut b_tx, &url).expect("b connect");

        // Check-then-wait so we catch the event whether it lands before or after
        // the watch is installed.
        let mut watch = b.watch_session(session_id).expect("watch");
        let received = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !b.session_messages(session_id).is_empty() {
                    return true;
                }
                if !watch.changed().await {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);

        assert!(received, "engine B should receive A's message");
        let messages = b.session_messages(session_id);
        assert!(matches!(&messages[0], Message::User(_)));
        relay.shutdown();
    }

    /// End-to-end over a real relay: the controller-side write flow desktop dave
    /// now uses. A *host* engine publishes a permission request (the event a
    /// claude-code backend produces), and a *controller* engine uses the
    /// build-only `prepare_*` API — then publishes the returned events from its
    /// own transport, exactly as dave's batched drain does — to answer the
    /// request and send a follow-up message. Both land back on the host.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn controller_prepare_writes_round_trip_to_the_host() {
        use crate::session_events::{build_permission_request_event, ThreadingState};

        let (_relay_dir, relay_ndb) = temp_ndb();
        let relay = spawn_relay(relay_ndb.clone());
        let url = relay.url();
        let session_id = "controller-round-trip";
        let perm_id = uuid::Uuid::new_v4();

        // --- host: publish the backend-produced permission request -----------
        let h_dir = TempDir::new().expect("tmp dir");
        let mut host =
            Engine::open(h_dir.path().to_str().expect("path"), TEST_SECKEY).expect("host engine");
        let mut host_tx = host.transport_handle().expect("host transport");
        host.connect(&mut host_tx, &url).expect("host connect");

        let mut threading = ThreadingState::new();
        let request = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({ "command": "ls" }),
            session_id,
            &mut threading,
            &TEST_SECKEY,
        )
        .expect("request event");
        publish_prepared(&mut host_tx, &request, &url);

        // --- controller: connect and wait for the request to sync in ---------
        let c_dir = TempDir::new().expect("tmp dir");
        let mut controller = Engine::open(c_dir.path().to_str().expect("path"), TEST_SECKEY)
            .expect("controller engine");
        let mut ctrl_tx = controller.transport_handle().expect("controller transport");
        controller
            .connect(&mut ctrl_tx, &url)
            .expect("controller connect");

        let mut ctrl_watch = controller
            .watch_session(session_id)
            .expect("controller watch");
        let saw_request = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let has_req = controller
                    .session_messages(session_id)
                    .iter()
                    .any(|m| matches!(m, Message::PermissionRequest(req) if req.id == perm_id));
                if has_req {
                    return true;
                }
                if !ctrl_watch.changed().await {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            saw_request,
            "controller should sync the host's permission request"
        );

        // --- controller: prepare (build + local-ingest) then publish itself --
        let response = controller
            .prepare_permission_response(session_id, &perm_id.to_string(), true, Some("ok"), false)
            .expect("prepare permission response");
        publish_prepared(&mut ctrl_tx, &response, &url);

        let message = controller
            .prepare_message(session_id, "also add a test")
            .expect("prepare message");
        publish_prepared(&mut ctrl_tx, &message, &url);

        // --- host: both the response and the follow-up message land ----------
        let mut host_watch = host.watch_session(session_id).expect("host watch");
        let got_both = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response_landed = latest_permission_response_message(host.ndb(), session_id)
                    .as_deref()
                    == Some("ok");
                let message_landed = host.session_messages(session_id).iter().any(
                    |m| matches!(m, Message::User(user) if user.as_str() == "also add a test"),
                );
                if response_landed && message_landed {
                    return true;
                }
                if !host_watch.changed().await {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            got_both,
            "host should receive the controller's prepared response and message"
        );
        relay.shutdown();
    }

    /// A permission response can only be built once its request is known, so
    /// unknown ids and malformed ids are rejected distinctly.
    #[tokio::test]
    async fn respond_permission_requires_a_known_request() {
        let dir = TempDir::new().expect("tmp dir");
        let engine = Engine::open(dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");

        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            engine.respond_permission(&mut tx, "s", &unknown, true, None, false),
            Err(EngineError::UnknownPermission)
        ));
        assert!(matches!(
            engine.respond_permission(&mut tx, "s", "not-a-uuid", true, None, false),
            Err(EngineError::InvalidPermId)
        ));
    }

    /// With the request seeded into the db, a response resolves its note id and
    /// publishes without error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn respond_permission_links_to_a_seeded_request() {
        use crate::session_events::{build_permission_request_event, ThreadingState};

        let dir = TempDir::new().expect("tmp dir");
        let engine = Engine::open(dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        let session_id = "perm-session";
        let perm_id = uuid::Uuid::new_v4();

        let mut threading = ThreadingState::new();
        let req = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({ "command": "ls" }),
            session_id,
            &mut threading,
            &TEST_SECKEY,
        )
        .expect("request event");
        pns_seed(engine.ndb(), &req.note_json);
        assert!(
            await_note(
                engine.ndb(),
                req.note_id,
                AI_CONVERSATION_KIND as u64,
                Duration::from_secs(5)
            )
            .await,
            "the permission request must be queryable before responding"
        );

        engine
            .respond_permission(
                &mut tx,
                session_id,
                &perm_id.to_string(),
                true,
                Some("ok".into()),
                false,
            )
            .expect("respond");
    }

    /// A question-set response, like a plain one, needs its request known first.
    #[tokio::test]
    async fn respond_question_requires_a_known_request() {
        let dir = TempDir::new().expect("tmp dir");
        let engine = Engine::open(dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");

        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            engine.respond_question(&mut tx, "s", &unknown, vec![]),
            Err(EngineError::UnknownPermission)
        ));
        assert!(matches!(
            engine.respond_question(&mut tx, "s", "not-a-uuid", vec![]),
            Err(EngineError::InvalidPermId)
        ));
    }

    /// Seeding an `AskUserQuestion` request and answering it publishes an
    /// approving response whose message carries the answers as plain prose —
    /// `Header: label, label, other` — with selected *indices* resolved to
    /// option *labels*, and with no JSON escaped inside `message`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn respond_question_formats_selected_labels_by_header() {
        use crate::messages::QuestionAnswer;
        use crate::session_events::{build_permission_request_event, ThreadingState};

        let dir = TempDir::new().expect("tmp dir");
        let engine = Engine::open(dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        let session_id = "question-session";
        let perm_id = uuid::Uuid::new_v4();

        // An AskUserQuestion request with one multi-select question. The view is
        // inferred as PermissionView::QuestionSet from the tool name + shape.
        let tool_input = serde_json::json!({
            "questions": [{
                "question": "Which languages?",
                "header": "Languages",
                "multiSelect": true,
                "options": [
                    { "label": "Rust", "description": "systems" },
                    { "label": "Swift", "description": "apple" },
                    { "label": "Zig", "description": "small" },
                ],
            }],
        });
        let mut threading = ThreadingState::new();
        let req = build_permission_request_event(
            &perm_id,
            "AskUserQuestion",
            &tool_input,
            session_id,
            &mut threading,
            &TEST_SECKEY,
        )
        .expect("request event");
        pns_seed(engine.ndb(), &req.note_json);
        assert!(
            await_note(
                engine.ndb(),
                req.note_id,
                AI_CONVERSATION_KIND as u64,
                Duration::from_secs(5)
            )
            .await,
            "the question request must be queryable before answering"
        );

        // The request must round-trip into a QuestionSet view for the answer
        // formatting to resolve labels (rather than fall back to raw answers).
        let messages = engine.session_messages(session_id);
        assert!(
            messages.iter().any(|m| matches!(
                m,
                Message::PermissionRequest(p) if p.view.question_set().is_some()
            )),
            "seeded request should surface as a QuestionSet"
        );

        // Answer by selecting options 0 and 2 (Rust, Zig) plus an "Other". Watch
        // for the response before answering, so the subscription can't miss an
        // ingest that races ahead of us.
        let mut watch = engine.watch_session(session_id).expect("watch");
        engine
            .respond_question(
                &mut tx,
                session_id,
                &perm_id.to_string(),
                vec![QuestionAnswer {
                    selected: vec![0, 2],
                    other_text: Some("Haskell".into()),
                }],
            )
            .expect("respond_question");

        // The response is a kind-1988 permission_response for this session, but it
        // reaches the db via async PNS decryption, so wait for it to land; its
        // content.message is the formatted answers payload.
        let payload = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(msg) = latest_permission_response_message(engine.ndb(), session_id) {
                    return msg;
                }
                if !watch.changed().await {
                    panic!("session watch ended before the response arrived");
                }
            }
        })
        .await
        .expect("a permission_response should have been published");
        // Plain prose, not a JSON blob: the header labels the line and the
        // selected indices are resolved to their option labels, with the
        // free-text "other" appended.
        assert_eq!(payload, "Languages: Rust, Zig, Haskell");
        assert!(
            !payload.contains('{') && !payload.contains('\\'),
            "the answers must not be JSON escaped into the message: {payload:?}"
        );
    }

    /// `format_question_answers` renders one `Header: …` prose line per
    /// question, joins multiple with newlines, resolves labels, and degrades
    /// gracefully when a header or the whole question metadata is missing.
    #[test]
    fn format_question_answers_renders_prose() {
        use crate::messages::{
            format_question_answers, QuestionAnswer, QuestionOption, QuestionSetInput, UserQuestion,
        };

        let question = |header: &str, question: &str, labels: &[&str]| UserQuestion {
            question: question.to_string(),
            header: header.to_string(),
            multi_select: true,
            options: labels
                .iter()
                .map(|l| QuestionOption {
                    label: l.to_string(),
                    description: String::new(),
                })
                .collect(),
        };

        // Two questions: a multi-select with an "other", and a single-select
        // whose header is empty (falls back to the question text).
        let set = QuestionSetInput {
            questions: vec![
                question("Languages", "Which?", &["Rust", "Swift", "Zig"]),
                question("", "Pick a theme", &["Light", "Dark"]),
            ],
        };
        let answers = vec![
            QuestionAnswer {
                selected: vec![0, 2],
                other_text: Some("Haskell".into()),
            },
            QuestionAnswer {
                selected: vec![1],
                other_text: None,
            },
        ];
        assert_eq!(
            format_question_answers(Some(&set), &answers),
            "Languages: Rust, Zig, Haskell\nPick a theme: Dark",
        );

        // No question metadata: indices can't resolve to labels, so fall back to
        // the raw index and a numbered header — still prose, never JSON.
        assert_eq!(
            format_question_answers(None, &answers),
            "Question 1: 0, 2, Haskell\nQuestion 2: 1",
        );
    }

    #[tokio::test]
    async fn spawn_session_returns_a_uuid_spawn_id() {
        let dir = TempDir::new().expect("tmp dir");
        let engine = Engine::open(dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        let spawn_id = engine
            .spawn_session(&mut tx, "laptop", "/tmp/project", "claude")
            .expect("spawn");
        assert!(
            uuid::Uuid::parse_str(&spawn_id).is_ok(),
            "spawn_id should be a uuid"
        );
    }

    #[tokio::test]
    async fn set_permission_mode_builds_and_ingests() {
        let dir = TempDir::new().expect("tmp dir");
        let engine = Engine::open(dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        engine
            .set_permission_mode(&mut tx, "some-session", "plan")
            .expect("set mode");
    }

    /// An embedded engine drives no relay loop, so it hands out no transport
    /// handle — its host publishes the events the `prepare_*` methods return.
    #[test]
    fn embedded_engine_has_no_transport_handle() {
        let (_dir, ndb) = temp_ndb();
        let engine = Engine::embedded(ndb, TEST_SECKEY).expect("embedded engine");
        assert!(
            engine.transport_handle().is_none(),
            "embedded engine has no loop to hand a transport onto"
        );
    }

    /// `prepare_set_permission_mode` ingests the event locally (so local reads
    /// see it immediately) and returns it, without needing a transport.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepare_set_permission_mode_ingests_without_publish() {
        let (_dir, ndb) = temp_ndb();
        let engine = Engine::embedded(ndb, TEST_SECKEY).expect("embedded engine");

        let built = engine
            .prepare_set_permission_mode("some-session", "plan")
            .expect("prepare mode");

        assert!(
            await_note(
                engine.ndb(),
                built.note_id,
                AI_CONVERSATION_KIND as u64,
                Duration::from_secs(5)
            )
            .await,
            "the prepared event should be ingested and queryable locally"
        );
    }

    /// `prepare_message` builds a kind-1988 user event, ingests it locally so
    /// local reads see it immediately, and returns it — no transport required.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prepare_message_ingests_without_publish() {
        let (_dir, ndb) = temp_ndb();
        let engine = Engine::embedded(ndb, TEST_SECKEY).expect("embedded engine");

        let built = engine
            .prepare_message("chat-session", "hello remote host")
            .expect("prepare message");

        assert!(
            await_note(
                engine.ndb(),
                built.note_id,
                AI_CONVERSATION_KIND as u64,
                Duration::from_secs(5)
            )
            .await,
            "the prepared user message should be ingested and queryable locally"
        );
    }

    /// The build-only permission response resolves its request from the db just
    /// like [`Engine::respond_permission`], so unknown / malformed ids are
    /// rejected before anything is ingested.
    #[test]
    fn prepare_permission_response_rejects_unknown_request() {
        let (_dir, ndb) = temp_ndb();
        let engine = Engine::embedded(ndb, TEST_SECKEY).expect("embedded engine");

        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            engine.prepare_permission_response("s", &unknown, true, None, false),
            Err(EngineError::UnknownPermission)
        ));
        assert!(matches!(
            engine.prepare_permission_response("s", "not-a-uuid", true, None, false),
            Err(EngineError::InvalidPermId)
        ));
    }

    #[tokio::test]
    async fn open_creates_a_usable_db() {
        let tmp = TempDir::new().expect("tmp dir");
        let engine = Engine::open(tmp.path().to_str().expect("path"), TEST_SECKEY).expect("open");
        // A fresh db opens a transaction without error.
        Transaction::new(engine.ndb()).expect("txn");
    }

    #[tokio::test]
    async fn with_ndb_shares_one_database() {
        let (_dir, host) = temp_ndb();

        // The host hands the engine a clone; dropping the engine (which stops the
        // loop) must not tear down the shared db.
        let engine = Engine::with_ndb(host.clone(), TEST_SECKEY).expect("engine");
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
        let engine =
            Engine::open(eng_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        tx.set_subscription(spec(&url, vec![Filter::new().kinds([1]).build()], vec![]));

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
        let engine_a =
            Engine::open(a_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine a");
        let mut tx_a = engine_a.transport_handle().expect("transport");
        let (note_json, id) = signed_note(1, "round trip");
        tx_a.publish_event_json(
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
        let engine_b =
            Engine::open(b_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine b");
        let mut tx_b = engine_b.transport_handle().expect("transport");
        tx_b.set_subscription(spec(&url, vec![Filter::new().kinds([1]).build()], vec![]));

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
        let engine =
            Engine::open(eng_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        tx.set_subscription(spec(
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

    /// The settle signal is deterministic: once [`Engine::wait_for_sync`]
    /// resolves, the reconciled history is already queryable — so a read taken
    /// immediately afterward, with *no* polling, sees it. This is exactly what
    /// lets a caller replace a quiet-timer drain heuristic with a single read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_for_sync_settles_after_history_is_queryable() {
        let (_relay_dir, relay_ndb) = temp_ndb();
        let (note_json, id) = signed_note(1, "settled history");
        relay_ndb
            .process_client_event(&event_frame(&note_json))
            .expect("seed relay");
        let relay = spawn_relay(relay_ndb.clone());
        let url = relay.url();
        assert!(await_note(&relay_ndb, id, 1, Duration::from_secs(5)).await);

        let eng_dir = TempDir::new().expect("tmp dir");
        let engine =
            Engine::open(eng_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine");
        let mut tx = engine.transport_handle().expect("transport");
        // History-only (live filter matches nothing) so the event can arrive
        // solely through the tracked backfill, then settle on it.
        tx.set_subscription(spec(
            &url,
            vec![Filter::new().kinds([9999]).build()],
            vec![Filter::new().kinds([1]).build()],
        ));

        // The barrier is enqueued after the subscription on the same FIFO
        // channel, so it waits for that backfill specifically. Bound it so a
        // regression can't hang the suite.
        let settled = tokio::time::timeout(Duration::from_secs(10), engine.wait_for_sync())
            .await
            .expect("wait_for_sync should not time out");
        assert_eq!(
            settled,
            Some(()),
            "standalone engine yields a settle signal"
        );

        // Deterministic: no await/poll between settle and read — the note must
        // already be present, proving settle means "history is queryable".
        let txn = Transaction::new(engine.ndb()).expect("txn");
        assert!(
            engine.ndb().get_note_by_id(&txn, &id).is_ok(),
            "reconciled history must be queryable the instant wait_for_sync resolves"
        );
        drop(txn);
        relay.shutdown();
    }

    /// An embedded engine has no self-driving loop, so it exposes no settle
    /// signal — the host owns sync and observes settle its own way.
    #[tokio::test]
    async fn wait_for_sync_is_none_for_embedded_engine() {
        let (_dir, ndb) = temp_ndb();
        let engine = Engine::embedded(ndb, TEST_SECKEY).expect("embedded engine");
        assert_eq!(engine.wait_for_sync().await, None);
    }
}
