//! The agentium engine: an owned [`Ndb`] plus session-protocol orchestration
//! over a long-lived [`nostrdb_net::relay::sync::Session`], so the same engine
//! drops into a standalone/iOS process (which lets the engine own a relay-sync
//! loop) or an embedding host (which drives sync itself) with no host application
//! context.
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
//!   holds it, and spawns its own [`Session`] sync loop.
//! - [`Engine::with_ndb`] — a standalone engine over a database the caller
//!   already opened; also spawns the [`Session`].
//! - [`Engine::embedded`] — an embedding host passes a *clone* of its own
//!   database and drives sync itself; no [`Session`] is spawned. Both point at
//!   the same db, and the engine dropping won't tear down the host's db because
//!   the host still holds a clone.
//!
//! Holding by value is also *required* for the standalone [`Session`], not merely
//! convenient: the [`Session`] runs a `tokio::spawn`ed loop that needs a
//! `'static` handle, so it takes a cloned `Ndb`.
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
//! # Relay sync
//!
//! A *standalone* engine ([`Engine::open`] / [`Engine::with_ndb`]) owns a
//! [`Session`] — nostrdb_net's long-lived client sync loop, shared with the
//! notedeck host and the CLIs. [`Engine::connect`] declares the single PNS
//! discovery subscription on it (a live `REQ` plus a NIP-77 negentropy backfill
//! of bounded history), and the write methods publish their PNS envelopes through
//! it. Because the [`Session`] spawns its loop, `open`/`with_ndb` must be called
//! from within a Tokio runtime.
//!
//! An *embedded* engine ([`Engine::embedded`]) owns no [`Session`] and needs no
//! runtime: the host already owns a relay stack, runs its own discovery
//! subscription, and publishes the events the engine's `prepare_*` methods build.
//! Its write methods still ingest locally, they just don't publish.
//!
//! The [`Session`] keeps its own futures `Send` (its `!Send` `nostrdb::Filter`s
//! cross the loop as [`SendFilter`](nostrdb::SendFilter) and never linger across
//! an await); the engine only hands it plain `Filter`s and pre-serialized event
//! JSON, so none of that discipline leaks up here.

use enostr::pns::PNS_KIND;
use enostr::NormRelayUrl;
use futures_util::StreamExt;
use nostrdb::{Filter, Ndb, SubscriptionStream, Transaction};
use nostrdb_net::relay::sync::Session;

use crate::messages::Message;
use crate::session_events::{AI_CONVERSATION_KIND, AI_SESSION_STATE_KIND};
use crate::session_loader::SessionState;

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

/// The stable [`Session`] subscription id for the PNS discovery subscription.
const PNS_DISCOVERY_SUB_ID: &str = "agentium/pns:discovery";

/// The agentium engine.
///
/// See the module docs for db ownership, identity, and relay sync. A standalone
/// engine owns a [`Session`]; dropping the engine drops it, stopping its loop. An
/// embedded engine owns none and lets its host drive sync.
pub struct Engine {
    ndb: Ndb,
    /// The single device identity. Its secret key signs the inner session
    /// events (kind 1988/31988/31989) and is the root from which the PNS keys
    /// are derived; both devices sharing a session share this key. See the
    /// module docs.
    account: enostr::FullKeypair,
    /// The relay a standalone engine publishes to and points its PNS discovery
    /// subscription at, set by [`Engine::connect`]. `None` means "ingest locally,
    /// don't publish" — the state of an embedded engine, whose host owns sync.
    pns_relay: Option<NormRelayUrl>,
    /// The long-lived relay sync loop, present only for a standalone engine
    /// ([`Engine::open`] / [`Engine::with_ndb`]). An embedded engine
    /// ([`Engine::embedded`]) has none — its host drives sync and publishes the
    /// events the engine's `prepare_*` methods build — so this is `None`.
    session: Option<Session>,
}

impl Engine {
    /// Open a standalone engine over its own nostrdb at `path` (created if
    /// absent). Use this on a host that has no existing database of its own.
    /// Must be called from within a Tokio runtime (spawns the [`Session`] loop).
    ///
    /// `device_key` is the 32-byte account secret — see [`Engine::with_ndb`].
    pub fn open(path: &str, device_key: [u8; 32]) -> Result<Self, EngineError> {
        let ndb = Ndb::new(path, &nostrdb::Config::new())?;
        Self::with_ndb(ndb, device_key)
    }

    /// Build a standalone engine over an existing database, taking a cheap
    /// [`Ndb`] clone. Use this when the caller already opened the database (e.g.
    /// the CLIs, which share nostrdb_net's managed cache) but wants agentium-core
    /// to own its own relay sync. Must be called from within a Tokio runtime
    /// (spawns the [`Session`] loop).
    ///
    /// `device_key` is the 32-byte account secret. It is registered with the
    /// database via [`Ndb::add_key`] so nostrdb's ingest threads decrypt inbound
    /// kind-1080 PNS envelopes into their queryable inner events, and it is the
    /// engine's signing/derivation identity. Fails with
    /// [`EngineError::InvalidDeviceKey`] if the bytes are not a valid secp256k1
    /// secret.
    pub fn with_ndb(ndb: Ndb, device_key: [u8; 32]) -> Result<Self, EngineError> {
        let mut engine = Self::embedded(ndb.clone(), device_key)?;
        // Own a `Session` over the same db so the standalone engine drives its own
        // remote sync. `Session::new` `tokio::spawn`s its loop, hence the runtime
        // requirement.
        engine.session = Some(Session::new(ndb));
        Ok(engine)
    }

    /// Build an engine over an existing database *without* a [`Session`], for an
    /// embedding host that already owns the relay sync. Reads work immediately;
    /// the host drives sync itself (its own [`Session`] pulls this identity's
    /// PNS/SNS envelopes in and fans freshly-ingested ones back out), and the
    /// engine only builds+ingests events via its `prepare_*` methods. Unlike
    /// [`Engine::with_ndb`] this needs no Tokio runtime and spawns no thread.
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
            session: None,
        })
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

    /// Connect a standalone engine to a relay for remote sync.
    ///
    /// Installs the single PNS discovery subscription on the engine's [`Session`]
    /// — kind-1080 events authored by this identity's PNS pubkey — which streams
    /// the whole identity's encrypted corpus (every session's state and
    /// conversation) into the database, where ndb's ingest threads decrypt it into
    /// queryable inner events. The same relay becomes the publish target for
    /// outbound events. Reconnecting to a different relay just re-points the
    /// subscription.
    ///
    /// A no-op for an [`embedded`](Engine::embedded) engine (no [`Session`]): its
    /// host owns the discovery subscription. Such a host points the engine's
    /// publishes at its relay via [`Engine::set_relay`] instead.
    pub fn connect(&mut self, relay_url: &str) -> Result<(), EngineError> {
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

        if let Some(session) = &self.session {
            session.set_subscription(
                PNS_DISCOVERY_SUB_ID,
                relay.to_string(),
                vec![live],
                vec![history],
            );
        }
        self.pns_relay = Some(relay);
        Ok(())
    }

    /// Tear down the discovery subscription on the engine's [`Session`] and forget
    /// the relay. Outbound events built after this are ingested locally but not
    /// published until the next [`Engine::connect`]. A no-op for an embedded
    /// engine (no [`Session`]).
    pub fn disconnect(&mut self) {
        if let Some(session) = &self.session {
            session.drop_subscription(PNS_DISCOVERY_SUB_ID);
        }
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
    /// negentropy backfill on the engine's [`Session`], and the returned future
    /// resolves once that backfill (and any other in flight when this is called)
    /// has completed — meaning the reconciled events are actually queryable, since
    /// each `sync_into` returns only after its received events are ingested. Read
    /// the session snapshot once this resolves and you see the whole synced batch,
    /// not a race with it.
    ///
    /// Ordering is exact, not best-effort: the [`Session`]'s settle barrier rides
    /// the same FIFO command channel as `connect`'s subscription, so by the time
    /// its loop handles it the backfill has already been counted. Resolves
    /// immediately for a connect that requested no history, or once the relay's
    /// history is in.
    ///
    /// Returns `None` for an [`embedded`](Engine::embedded) engine — it has no
    /// [`Session`], so its host owns sync and observes settle its own way.
    /// Standalone callers should still bound the wait with a timeout, since a
    /// reachable-but-silent relay could otherwise stall the reconcile.
    pub async fn wait_for_sync(&self) -> Option<()> {
        self.session.as_ref()?.wait_for_sync().await;
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
    /// conversation and publishes it through the engine's [`Session`]. Works
    /// whether or not the session is known locally (a brand-new session simply
    /// starts a fresh thread).
    ///
    /// Returns the [`BuiltEvent`](crate::session_events::BuiltEvent) it published
    /// so a caller (e.g. `agentium send`) can report the resulting event id;
    /// callers that don't need it just discard the value.
    pub fn send_message(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let built = self.make_user_message(session_id, text)?;
        self.publish_session_event(&built)?;
        Ok(built)
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
    ///
    /// A non-empty `title` rides the command as a `custom_title` tag so the host
    /// gives the new session an explicit, sticky title rather than deriving one
    /// from its first message (see [`build_spawn_command_event`]).
    ///
    /// [`build_spawn_command_event`]: crate::session_events::build_spawn_command_event
    pub fn spawn_session(
        &self,
        target_host: &str,
        cwd: &str,
        backend: &str,
        title: Option<&str>,
    ) -> Result<String, EngineError> {
        let spawn_id = uuid::Uuid::new_v4().to_string();
        let built = self.make_spawn_command(target_host, cwd, backend, title, &spawn_id, None)?;
        self.publish_session_event(&built)?;
        Ok(spawn_id)
    }

    /// Resume a closed (possibly soft-deleted) session on a remote host.
    ///
    /// Mirrors [`spawn_session`](Engine::spawn_session) but publishes a
    /// `command = "resume_session"` variant of the kind-31989 command carrying
    /// the target session's stable id (`target_session_id`, the `agentium:` ref)
    /// and the CLI session id for `claude --resume` (`cli_session_id`). The host
    /// reopens *that* session — reviving its tombstone and rehydrating history —
    /// rather than creating a new one. Returns the `spawn_id` for symmetry.
    pub fn resume_session(
        &self,
        target_host: &str,
        cwd: &str,
        backend: &str,
        target_session_id: &str,
        cli_session_id: &str,
    ) -> Result<String, EngineError> {
        let spawn_id = uuid::Uuid::new_v4().to_string();
        let resume = crate::session_events::ResumeSpawn {
            target_session_id,
            cli_session_id,
        };
        // A resume reopens an existing session, so it carries no title override —
        // the revived session keeps whatever title it already had.
        let built =
            self.make_spawn_command(target_host, cwd, backend, None, &spawn_id, Some(&resume))?;
        self.publish_session_event(&built)?;
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
        let built = self.make_spawn_command(target_host, cwd, backend, None, spawn_id, None)?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Build a kind-31989 resume command, ingest it locally, and return it for
    /// the caller to publish through its own transport — the batching-host
    /// counterpart to [`resume_session`](Engine::resume_session), exactly as
    /// [`prepare_spawn_command`](Engine::prepare_spawn_command) is to
    /// [`spawn_session`](Engine::spawn_session). The command reopens the session
    /// named by `target_session_id` on `target_host` (reviving its tombstone and
    /// resuming via `cli_session_id`) rather than minting a new one. The caller
    /// supplies `spawn_id` and publishes the returned event itself; nothing is
    /// sent here.
    pub fn prepare_resume_command(
        &self,
        target_host: &str,
        cwd: &str,
        backend: &str,
        spawn_id: &str,
        target_session_id: &str,
        cli_session_id: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let resume = crate::session_events::ResumeSpawn {
            target_session_id,
            cli_session_id,
        };
        let built =
            self.make_spawn_command(target_host, cwd, backend, None, spawn_id, Some(&resume))?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Build the inner kind-31989 spawn-command event (no ingest, no publish).
    ///
    /// `resume` = `None` builds a plain spawn; `Some` builds a resume command
    /// (see [`build_spawn_command_event`](crate::session_events::build_spawn_command_event)).
    fn make_spawn_command(
        &self,
        target_host: &str,
        cwd: &str,
        backend: &str,
        title: Option<&str>,
        spawn_id: &str,
        resume: Option<&crate::session_events::ResumeSpawn<'_>>,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        crate::session_events::build_spawn_command_event(
            target_host,
            cwd,
            backend,
            title,
            spawn_id,
            resume,
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
        self.publish_session_event(&built)
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
            false, // explicit host-side decision, not auto-accepted
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
            false, // explicit host-side answer, not auto-accepted
            session_id,
            &mut threading,
            &self.seckey(),
        )
        .map_err(|e| EngineError::Build(e.to_string()))?;
        self.publish_session_event(&built)
    }

    /// Request a permission-mode change on a session's host (e.g. `"default"`,
    /// `"acceptEdits"`, `"plan"`). Publishes a kind-1988 command the host applies
    /// to its local backend.
    pub fn set_permission_mode(&self, session_id: &str, mode: &str) -> Result<(), EngineError> {
        let built = self.make_set_permission_mode(session_id, mode)?;
        self.publish_session_event(&built)
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

    /// Interrupt a session's in-flight turn on its host.
    ///
    /// Publishes a kind-1988 interrupt command (`role = "interrupt"`) that the
    /// host applies to its local backend, aborting the current turn/tool loop —
    /// the remote-session counterpart to the local Escape interrupt. A no-op
    /// against a session that isn't currently running a turn.
    pub fn interrupt_session(&self, session_id: &str) -> Result<(), EngineError> {
        let built = self.make_interrupt(session_id)?;
        self.publish_session_event(&built)
    }

    /// Build a kind-1988 interrupt command, ingest it locally, and return it for
    /// the caller to publish through its own transport — for a host that batches
    /// its own relay writes. Unlike [`interrupt_session`](Engine::interrupt_session),
    /// this does not publish.
    pub fn prepare_interrupt(
        &self,
        session_id: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let built = self.make_interrupt(session_id)?;
        self.wrap_and_ingest(&built)?;
        Ok(built)
    }

    /// Build the inner kind-1988 interrupt event (no ingest, no publish).
    fn make_interrupt(
        &self,
        session_id: &str,
    ) -> Result<crate::session_events::BuiltEvent, EngineError> {
        let mut threading = self.session_threading(session_id);
        crate::session_events::build_interrupt_event(session_id, &mut threading, &self.seckey())
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

    /// Publish an event this engine already built (e.g. a host answering a spawn
    /// command with a kind-31988 state) through its [`Session`]: wrap it in its PNS
    /// envelope, ingest locally, and send it to the configured relay. A thin public
    /// front for [`publish_session_event`](Self::publish_session_event); a no-op on
    /// the wire for an engine with no [`Session`]/relay.
    pub fn publish_event(
        &self,
        built: &crate::session_events::BuiltEvent,
    ) -> Result<(), EngineError> {
        self.publish_session_event(built)
    }

    /// Wrap, ingest locally, and publish a freshly-built inner event through the
    /// engine's [`Session`]. Local ingest always happens; publishing is skipped
    /// when there is no [`Session`] (an embedded engine — its host fans the
    /// freshly-ingested envelope out) or no publish relay is set.
    fn publish_session_event(
        &self,
        built: &crate::session_events::BuiltEvent,
    ) -> Result<(), EngineError> {
        let wrapped = self.wrap_and_ingest(built)?;
        match (&self.session, self.pns_relay.as_ref()) {
            (Some(session), Some(relay)) => session.publish(wrapped, vec![relay.to_string()]),
            _ => tracing::debug!("engine: no session/publish relay; event ingested locally only"),
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

/// The current Unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{await_note, spawn_relay, temp_ndb, TEST_SECKEY};
    use std::time::Duration;
    use tempfile::TempDir;

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
    /// `engine`'s [`Session`] to its connected relay. Models the controller's
    /// "prepare locally, then publish it myself" flow — the `prepare_*` methods
    /// ingest but don't publish, so the caller wraps the returned inner event and
    /// sends it through the same [`Session`] a standalone engine owns.
    fn publish_prepared(engine: &Engine, built: &crate::session_events::BuiltEvent, relay: &str) {
        let pns = enostr::pns::derive_pns_keys(&TEST_SECKEY);
        let wrapped = crate::session_events::wrap_pns(&built.note_json, &pns).expect("wrap pns");
        engine
            .session
            .as_ref()
            .expect("standalone engine has a session")
            .publish(wrapped, vec![relay.to_string()]);
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
        a.connect(&url).expect("a connect");
        a.send_message(session_id, "hi from A").expect("send");

        let b_dir = TempDir::new().expect("tmp dir");
        let mut b =
            Engine::open(b_dir.path().to_str().expect("path"), TEST_SECKEY).expect("engine b");
        b.connect(&url).expect("b connect");

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
        host.connect(&url).expect("host connect");

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
        publish_prepared(&host, &request, &url);

        // --- controller: connect and wait for the request to sync in ---------
        let c_dir = TempDir::new().expect("tmp dir");
        let mut controller = Engine::open(c_dir.path().to_str().expect("path"), TEST_SECKEY)
            .expect("controller engine");
        controller.connect(&url).expect("controller connect");

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
        publish_prepared(&controller, &response, &url);

        let message = controller
            .prepare_message(session_id, "also add a test")
            .expect("prepare message");
        publish_prepared(&controller, &message, &url);

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

        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            engine.respond_permission("s", &unknown, true, None, false),
            Err(EngineError::UnknownPermission)
        ));
        assert!(matches!(
            engine.respond_permission("s", "not-a-uuid", true, None, false),
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

        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            engine.respond_question("s", &unknown, vec![]),
            Err(EngineError::UnknownPermission)
        ));
        assert!(matches!(
            engine.respond_question("s", "not-a-uuid", vec![]),
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
        let spawn_id = engine
            .spawn_session("laptop", "/tmp/project", "claude", None)
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
        engine
            .set_permission_mode("some-session", "plan")
            .expect("set mode");
    }

    /// `prepare_set_permission_mode` ingests the event locally (so local reads
    /// see it immediately) and returns it, without needing a session.
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

        // The caller hands the engine a clone; dropping the engine (which drops
        // its `Session` and stops that loop) must not tear down the shared db.
        let engine = Engine::with_ndb(host.clone(), TEST_SECKEY).expect("engine");
        drop(engine);
        Transaction::new(&host).expect("host db still usable after engine drop");
    }

    /// An embedded engine has no [`Session`], so it exposes no settle signal —
    /// the host owns sync and observes settle its own way.
    #[tokio::test]
    async fn wait_for_sync_is_none_for_embedded_engine() {
        let (_dir, ndb) = temp_ndb();
        let engine = Engine::embedded(ndb, TEST_SECKEY).expect("embedded engine");
        assert_eq!(engine.wait_for_sync().await, None);
    }
}
