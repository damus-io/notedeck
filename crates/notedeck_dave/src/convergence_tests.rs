//! Host-vs-fold convergence harness (headway:dave/soup-neck-green).
//!
//! A Dave session should look the same to everyone viewing it: the host while
//! the turn streams, the host after a restart, a remote observer and the
//! `agentium` CLI. All but the first are one fold over the session's kind-1988
//! notes ([`load_session_messages_for_author`]); the host builds its live chat
//! straight from the backend stream instead. Each test here drives a scripted
//! backend stream through the host's real code path ([`apply_response`],
//! [`handle_stream_end`], the user-send funnel, [`dispatch_turn`]), waits for every note the host
//! published to land in ndb, then asserts that the host's chat and the fold
//! have the same [`view_signature`] — and that the fold is the same again when
//! those notes are backfilled into a fresh ndb in reverse order.
//!
//! Then the host's notes come back through the conversation poll, and the host
//! reconciles its chat to the fold at rest ([`maybe_reconcile_at_rest`]). Every
//! scenario must converge there without a drift, and leave the host's chat
//! equal to the fold.
//!
//! A remote [`Observer`] watches every scenario too. After each step it is fed
//! what nostrdb indexed the way the conversation poll feeds a remote session,
//! mostly appending rows one batch at a time, and its chat must equal the fold
//! at every step.
//!
//! A scenario that fails today is `#[ignore]`d with the converge card that
//! fixes it; that card un-ignores it.

use crate::backend::shared::prepare_prompt_and_images;
use crate::backend::{AiBackend, BackendType};
use crate::config::AiMode;
use crate::conversation::{
    handle_remote_permission_response, process_conversation_notes, rebuild_chat_from_fold,
    subscribe_conversation_events, ProcessedNotes,
};
use crate::conversation_feed::ConversationFeed;
use crate::messages::{
    format_question_answers, CompactionInfo, PendingPermission, PermissionRequest, QuestionAnswer,
    RunningTool, SubagentInfo, SubagentStatus,
};
use crate::pns_runtime::PnsLocalState;
use crate::publish::{
    pns_ingest, publish_user_permission_response, record_user_message,
    update_statuses_and_publish_auto_resolved,
};
use crate::reconcile::{maybe_reconcile_at_rest, Drift, ReconcileOutcome};
use crate::session::{ChatSession, CompactIntent, SessionId, SessionManager, SessionSource};
use crate::stream_events::{
    apply_response, dispatch_turn, handle_stream_end, reconcile_ended_turns, ApplyCtx, DispatchCtx,
};
use crate::tests::{test_config, test_dave, test_secret_key};
use crate::tools::{Tool, ToolResponses};
use crate::ui::{
    handle_send_action, handle_ui_action, DaveAction, SendActionResult, UiActionResult,
};
use crate::update::PermissionPublish;
use crate::{
    embedded_engine, DaveApiResponse, DaveOverlay, ExecutedTool, ImageAttachment, Message,
    PermissionResponse,
};
use agentium_core::session_events::{
    build_live_event_at, build_live_events, build_permission_response_event,
    build_session_state_event, BuiltEvent, LiveEventTags, ThreadingState, AI_CONVERSATION_KIND,
    AI_SESSION_STATE_KIND, DISPATCHED_ROLE, MAX_WIRE_EVENT_BYTES,
};
use agentium_core::session_loader::{
    load_session_messages_for_author, view_signature, EventOrder, RowSig,
};
use claude_agent_sdk_rs::PermissionMode;
use nostrdb::{Filter, IngestMetadata, Ndb, NoteKey, SubscriptionStream, Transaction};
use notedeck::{DataPath, Waker};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// The `d` tag every scenario's session publishes under. Each scenario gets its
/// own ndb, so one id is enough.
const SESSION: &str = "convergence-test";

/// How long to wait for the host's notes to be indexed before failing.
const INGEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One thing that happens to the host during a scenario.
enum Step {
    /// The user sends a message (the interactive/programmatic send funnel).
    Send(&'static str),
    /// The host dispatches the trailing user message(s) to the backend, the
    /// way the send path does ([`dispatch_turn`]).
    Dispatch,
    /// The backend streams a response.
    Backend(DaveApiResponse),
    /// The backend asks permission to run a tool; the harness holds the
    /// response channel so the host's answer has somewhere to go.
    Permission(PermissionRequest),
    /// The backend ends the turn.
    StreamEnd,
    /// What the turn that just ended left to redispatch: `None` when the host
    /// asked for no redispatch, else the prompt the backend would be sent —
    /// the trailing user turns it collects from the chat.
    ExpectRedispatch(Option<&'static str>),
    /// Wait until everything published so far is indexed: the moment a user
    /// takes to read a request before answering it, whose answer is built
    /// from the request's note in ndb.
    Settle,
    /// Anything else the host does between responses (a UI action, a grant).
    Act(Box<dyn FnOnce(&mut Host)>),
    /// A note another device published reaches the host: it is stored, then
    /// the conversation poll hands it over.
    Deliver(BuiltEvent),
    /// Another device (a phone, the `agentium` CLI) answers the permission
    /// request with this id: its response note is stored, then the host's action poll
    /// resolves the request with it and the conversation poll hands it over.
    RemoteAnswer(uuid::Uuid, RemoteAnswer),
    /// The host restarts: its session comes back from ndb through startup's
    /// background restore, and the script carries on with it.
    Restart,
    /// A checkpoint mid-script: once everything published so far is indexed,
    /// the host's chat and the fold agree, in either ingestion order. Named
    /// for the failure message.
    AssertConverged(&'static str),
}

/// The host side of a scenario: one agentic session, its ndb and signing key.
struct Host {
    sessions: SessionManager,
    sid: SessionId,
    ndb: Ndb,
    secret_key: Option<[u8; 32]>,
    /// Response channels of the permission requests the backend sent, kept
    /// open so resolving a request does not log a closed-channel error.
    permission_rxs: Vec<oneshot::Receiver<PermissionResponse>>,
    /// Sessions the last stream end asked to redispatch, as the app's update
    /// loop collects them. A dispatch takes the session back out.
    needs_send: HashSet<SessionId>,
    /// The session's conversation notes as they commit, subscribed before
    /// anything was published.
    indexed: IndexWatch,
    /// The PNS envelopes those notes arrived in, as they commit.
    envelopes: IndexWatch,
    /// Notes indexed since the observer was last fed, in the order nostrdb
    /// delivered them.
    unobserved: Vec<NoteKey>,
    /// A remote device watching the session, fed after every step.
    observer: Observer,
    _dir: TempDir,
}

impl Host {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let sk = test_secret_key();
        assert!(ndb.add_key(&sk), "ndb must accept the PNS key");
        let indexed = IndexWatch::new(&ndb);
        let envelopes = IndexWatch::envelopes(&ndb);
        let mut sessions = SessionManager::new();
        let sid = sessions.new_session(PathBuf::from("/tmp"), AiMode::Agentic, BackendType::Claude);
        sessions
            .get_mut(sid)
            .unwrap()
            .agentic
            .as_mut()
            .unwrap()
            .event_id = SESSION.to_string();
        Host {
            sessions,
            sid,
            ndb,
            secret_key: Some(sk),
            permission_rxs: Vec::new(),
            needs_send: HashSet::new(),
            indexed,
            envelopes,
            unobserved: Vec::new(),
            observer: Observer::new(),
            _dir: dir,
        }
    }

    fn session(&mut self) -> &mut ChatSession {
        self.sessions.get_mut(self.sid).unwrap()
    }

    /// Apply one scripted step through the host's real code path.
    fn apply(&mut self, step: Step) {
        let sid = self.sid;
        let session = self.sessions.get_mut(sid).unwrap();
        match step {
            Step::Send(text) => record_user_message(
                session,
                &self.ndb,
                self.secret_key.as_ref(),
                text.to_string(),
                Vec::new(),
            ),
            Step::Dispatch => {
                let waker = Waker::noop();
                let session_env = BTreeMap::new();
                let ctx = DispatchCtx {
                    ndb: &self.ndb,
                    secret_key: self.secret_key.as_ref(),
                    user_id: String::new(),
                    tools: Arc::default(),
                    session_env: &session_env,
                    waker: &waker,
                };
                dispatch_turn(session, &ScriptBackend, &ctx);
                self.needs_send.remove(&sid);
            }
            Step::Backend(res) => {
                let ctx = ApplyCtx {
                    ndb: &self.ndb,
                    secret_key: &self.secret_key,
                    persistent_stream: true,
                };
                apply_response(session, sid, res, &ctx);
            }
            Step::Permission(request) => {
                let (response_tx, rx) = oneshot::channel();
                self.permission_rxs.push(rx);
                let ctx = ApplyCtx {
                    ndb: &self.ndb,
                    secret_key: &self.secret_key,
                    persistent_stream: true,
                };
                let pending = PendingPermission {
                    request,
                    response_tx,
                };
                apply_response(
                    session,
                    sid,
                    DaveApiResponse::PermissionRequest(pending),
                    &ctx,
                );
            }
            Step::StreamEnd => handle_stream_end(
                session,
                sid,
                &self.secret_key,
                &self.ndb,
                &mut self.needs_send,
                &mut HashSet::new(),
            ),
            Step::ExpectRedispatch(expected) => {
                let redispatch = self.needs_send.contains(&sid).then(|| {
                    // The resumed-session form: the trailing user turns.
                    prepare_prompt_and_images(&session.chat, &Some(String::new())).0
                });
                assert_eq!(
                    redispatch.as_deref(),
                    expected,
                    "what the turn left to redispatch"
                );
            }
            Step::Act(act) => act(self),
            Step::Settle
            | Step::Deliver(_)
            | Step::RemoteAnswer(..)
            | Step::Restart
            | Step::AssertConverged(_) => {
                unreachable!(
                    "settling, delivery, restarts and checkpoints are async; `drive` awaits them"
                )
            }
        }
    }

    /// Run a script through the host.
    async fn drive(&mut self, script: Vec<Step>) {
        for step in script {
            match step {
                Step::Settle => self.settle().await,
                Step::Deliver(note) => {
                    self.store_remote(&note).await;
                    self.poll_note(&note.note_id);
                }
                Step::RemoteAnswer(perm_id, answer) => {
                    let note = self.remote_response(perm_id, answer);
                    self.store_remote(&note).await;
                    self.poll_action(&note.note_id);
                    self.poll_note(&note.note_id);
                }
                Step::Restart => self.restart().await,
                Step::AssertConverged(at) => {
                    self.assert_converged(at).await;
                }
                step => self.apply(step),
            }
            self.assert_observer_matches_fold().await;
        }
    }

    /// Wait until every note the host has published so far is indexed.
    async fn settle(&mut self) {
        let ids = self.published_note_ids();
        self.wait_indexed(&ids).await;
    }

    /// Wait until every note in `ids` is indexed, keeping the new arrivals
    /// for the observer.
    async fn wait_indexed(&mut self, ids: &HashSet<[u8; 32]>) {
        let arrived = self.indexed.wait_for(&self.ndb, ids).await;
        self.unobserved.extend(arrived);
    }

    /// Feed the observer what has been indexed since it was last fed, as one
    /// poll batch, and assert its chat is the fold.
    async fn assert_observer_matches_fold(&mut self) {
        self.settle().await;
        let keys = std::mem::take(&mut self.unobserved);
        let author = self.author();
        self.observer.poll(&self.ndb, &keys, &author, None);
        let sk = self.secret_key.unwrap();
        assert_eq!(
            view_signature(&self.observer.session.chat),
            fold_signature(&self.ndb, &sk),
            "the observer's chat and the fold disagree"
        );
    }

    /// Wait for everything published so far to be indexed, then assert the
    /// host's chat, the fold over its notes and the fold after a reversed
    /// backfill all agree. `at` names the checkpoint in a failure. Returns
    /// the fold.
    async fn assert_converged(&mut self, at: &str) -> Vec<RowSig> {
        self.settle().await;
        let ids = self.published_note_ids();
        let sk = self.secret_key.unwrap();

        let host_view = view_signature(&self.session().chat);
        let fold = fold_signature(&self.ndb, &sk);
        assert_eq!(
            host_view, fold,
            "{at}: the host's chat and the fold over its notes disagree"
        );

        // Each note came in its own envelope, which the backfill re-ingests.
        self.envelopes.wait_for_count(ids.len()).await;
        let backfilled = reversed_backfill_signature(&self.ndb, &sk, &ids).await;
        assert_eq!(
            fold, backfilled,
            "{at}: the fold depends on the order the notes were ingested"
        );
        fold
    }

    /// How many dispatch markers the host has published.
    fn dispatch_marker_count(&mut self) -> usize {
        let ids = self.published_note_ids();
        let txn = Transaction::new(&self.ndb).unwrap();
        ids.iter()
            .filter(|id| {
                let note = self.ndb.get_note_by_id(&txn, id).unwrap();
                note.tags().into_iter().any(|tag| {
                    tag.get_str(0) == Some("role") && tag.get_str(1) == Some(DISPATCHED_ROLE)
                })
            })
            .count()
    }

    /// Every note id the host published for this session. Each publish
    /// records its note as seen.
    fn published_note_ids(&mut self) -> HashSet<[u8; 32]> {
        self.session()
            .agentic
            .as_ref()
            .unwrap()
            .seen_note_ids
            .clone()
    }

    fn author(&self) -> nostrdb_net::Pubkey {
        nostrdb_net::FullKeypair::from_secret_bytes(&self.secret_key.unwrap())
            .unwrap()
            .pubkey
    }

    /// Reconcile at rest without handing the host its notes back: what the
    /// host does at the end of a turn.
    fn reconcile_now(&mut self) -> ReconcileOutcome {
        let author = self.author();
        let session = self.sessions.get_mut(self.sid).unwrap();
        maybe_reconcile_at_rest(session, &self.ndb, &author)
    }

    /// Hand the host its published notes back the way the conversation poll
    /// does once they are indexed, then reconcile at rest, as the poll does.
    fn poll_and_reconcile(&mut self) -> ReconcileOutcome {
        self.poll_own_notes();
        self.reconcile_now()
    }

    /// Hand the host its published notes back the way the conversation poll
    /// does once they are indexed, without the reconcile that follows.
    fn poll_own_notes(&mut self) {
        let ids = self.published_note_ids();
        let sid = self.sid;
        let txn = Transaction::new(&self.ndb).unwrap();
        let notes = ids
            .iter()
            .map(|id| self.ndb.get_note_by_id(&txn, id).unwrap())
            .collect();
        let session = self.sessions.get_mut(sid).unwrap();
        process_conversation_notes(
            notes,
            session,
            sid,
            false,
            self.secret_key.as_ref(),
            &self.ndb,
        );
    }

    /// Store a note another device published, and wait until it is indexed.
    async fn store_remote(&mut self, note: &BuiltEvent) {
        assert!(pns_ingest(
            &self.ndb,
            &note.note_json,
            &self.secret_key.unwrap()
        ));
        let mut ids = self.published_note_ids();
        ids.insert(note.note_id);
        self.wait_indexed(&ids).await;
    }

    /// Restart the host. Everything it published is indexed, then a fresh
    /// [`Dave`](crate::Dave) restores the session from ndb the way startup
    /// does: the background worker folds it ([`load_session_messages_for_author`])
    /// and [`drain_session_restore`](crate::Dave::drain_session_restore)
    /// installs it. The host carries on with the restored session in place of
    /// the one it had.
    async fn restart(&mut self) {
        self.settle().await;
        let published = self.published_note_ids();
        let sk = self.secret_key.unwrap();
        let account = self.author();

        let data_dir = TempDir::new().unwrap();
        let mut dave = test_dave(&DataPath::new(data_dir.path()));
        dave.pns_local_state = Some(PnsLocalState {
            account,
            has_secret_key: true,
        });
        self.store_local_state(&dave.hostname).await;
        let sub = subscribe_conversation_events(&self.ndb, account).unwrap();
        dave.conversation_feed = Some(ConversationFeed::new(sub));

        let waker = dave.run_restore_worker(&self.ndb, account).await;
        let replayed = dave.drain_session_restore(&self.ndb, Some(&sk), &waker);
        assert!(
            replayed.is_empty(),
            "the host had every note before it went down"
        );

        self.sessions = std::mem::take(&mut dave.session_manager);
        self.sid = self
            .sessions
            .iter()
            .find(|session| {
                session
                    .agentic
                    .as_ref()
                    .is_some_and(|agentic| agentic.event_session_id() == SESSION)
            })
            .expect("the session was restored")
            .id;
        assert!(!self.session().is_remote(), "it is restored as this host's");
        assert_eq!(
            self.published_note_ids(),
            published,
            "the restored session has seen every note it published"
        );
    }

    /// Store the session's kind-31988 state as this host, `hostname`, last
    /// published it, so a restart restores it as a local session.
    async fn store_local_state(&self, hostname: &str) {
        let state = build_session_state_event(
            SESSION,
            "Convergence",
            None,
            "/tmp",
            "idle",
            None,
            hostname,
            "/home/dev",
            "claude",
            "default",
            Some("cli-convergence"),
            None,
            None,
            None,
            None,
            1_000,
            &self.secret_key.unwrap(),
        )
        .unwrap();
        let filter = Filter::new().kinds([AI_SESSION_STATE_KIND as u64]).build();
        let sub = self.ndb.subscribe(&[filter]).unwrap();
        self.ndb
            .process_event_with(&state.to_event_json(), IngestMetadata::new().client(true))
            .unwrap();
        self.ndb
            .wait_for_notes(sub, 1)
            .await
            .expect("the session state was never indexed");
    }

    /// The response another device publishes to request `perm_id`, built
    /// from the request's note as the engine does
    /// (`Engine::respond_permission` / `Engine::respond_question`).
    fn remote_response(&mut self, perm_id: uuid::Uuid, answer: RemoteAnswer) -> BuiltEvent {
        let session = self.session();
        let request_note_id = session
            .agentic
            .as_ref()
            .unwrap()
            .permissions
            .request_note_ids[&perm_id];
        let (allowed, message) = match answer {
            RemoteAnswer::Deny(reason) => (false, reason.to_string()),
            RemoteAnswer::FirstOptions => {
                let questions = session.chat.iter().find_map(|msg| match msg {
                    Message::PermissionRequest(req) if req.id == perm_id => req.view.question_set(),
                    _ => None,
                });
                let answers: Vec<QuestionAnswer> = questions
                    .expect("a question set")
                    .questions
                    .iter()
                    .map(|_| QuestionAnswer {
                        selected: vec![0],
                        other_text: None,
                    })
                    .collect();
                (true, format_question_answers(questions, &answers))
            }
        };
        build_permission_response_event(
            &perm_id,
            &request_note_id,
            allowed,
            Some(&message),
            false,
            false,
            SESSION,
            &mut ThreadingState::new(),
            &test_secret_key(),
        )
        .unwrap()
    }

    /// Hand the session one stored conversation action the way the host's
    /// action poll (`Dave::poll_remote_conversation_actions`) does.
    fn poll_action(&mut self, note_id: &[u8; 32]) {
        let sid = self.sid;
        let txn = Transaction::new(&self.ndb).unwrap();
        let note = self.ndb.get_note_by_id(&txn, note_id).unwrap();
        let session = self.sessions.get_mut(sid).unwrap();
        handle_remote_permission_response(&note, session);
    }

    /// Hand the session one stored note the way the conversation poll does.
    fn poll_note(&mut self, note_id: &[u8; 32]) -> ProcessedNotes {
        let sid = self.sid;
        let txn = Transaction::new(&self.ndb).unwrap();
        let note = self.ndb.get_note_by_id(&txn, note_id).unwrap();
        let session = self.sessions.get_mut(sid).unwrap();
        process_conversation_notes(
            vec![note],
            session,
            sid,
            false,
            self.secret_key.as_ref(),
            &self.ndb,
        )
    }
}

/// How another device answers a permission request ([`Step::RemoteAnswer`]).
enum RemoteAnswer {
    /// Deny it, with the reason the user typed.
    Deny(&'static str),
    /// Answer its question set with each question's first option.
    FirstOptions,
}

/// The backend a script plays: its responses arrive as [`Step::Backend`], so
/// starting a turn hands back no stream of its own.
struct ScriptBackend;

impl AiBackend for ScriptBackend {
    fn stream_request(
        &self,
        _messages: Vec<Message>,
        _tools: Arc<HashMap<String, Tool>>,
        _model: Option<String>,
        _user_id: String,
        _session_id: String,
        _session_env: BTreeMap<String, String>,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        _permission_mode: PermissionMode,
        _waker: Waker,
    ) -> (
        Option<mpsc::Receiver<DaveApiResponse>>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        (None, None)
    }

    fn cleanup_session(&self, _session_id: String) {}

    fn interrupt_session(&self, _session_id: String, _waker: Waker) {}

    fn set_permission_mode(&self, _session_id: String, _mode: PermissionMode, _waker: Waker) {}
}

/// Counts the notes matching a filter as they commit to an ndb: the scenario
/// session's conversation notes ([`IndexWatch::new`]), or the PNS envelopes
/// they arrive in ([`IndexWatch::envelopes`]).
///
/// Subscribed before anything is ingested, so nothing slips past it. Waiting
/// counts deliveries rather than polling for ids: each published note matches
/// the filter and is written once, so N deliveries is exactly "N committed".
/// The stream is held for the whole scenario because dropping it
/// unsubscribes.
struct IndexWatch {
    stream: SubscriptionStream,
    delivered: usize,
}

impl IndexWatch {
    fn new(ndb: &Ndb) -> Self {
        Self::matching(
            ndb,
            Filter::new()
                .kinds([AI_CONVERSATION_KIND as u64])
                .tags([SESSION], 'd')
                .build(),
        )
    }

    /// Counts kind-1080 envelopes. nostrdb indexes a note unwrapped from one
    /// apart from the envelope itself, so a note being indexed doesn't mean
    /// its envelope is yet.
    fn envelopes(ndb: &Ndb) -> Self {
        Self::matching(
            ndb,
            Filter::new()
                .kinds([nostrdb_net::pns::PNS_KIND as u64])
                .build(),
        )
    }

    fn matching(ndb: &Ndb, filter: Filter) -> Self {
        let sub = ndb.subscribe(&[filter]).unwrap();
        IndexWatch {
            stream: SubscriptionStream::new(ndb.clone(), sub),
            delivered: 0,
        }
    }

    /// Wait until `count` notes in all have been indexed.
    async fn wait_for_count(&mut self, count: usize) {
        let pending = count.saturating_sub(self.delivered);
        if pending == 0 {
            return;
        }
        let arrived = self
            .stream
            .wait_for_notes(pending, INGEST_TIMEOUT)
            .await
            .expect("the envelopes were never indexed");
        self.delivered += arrived.len();
    }

    /// Wait until every note in `ids` — all this ndb's session notes so far —
    /// is indexed. Returns the notes that arrived while waiting, in the order
    /// nostrdb delivered them.
    async fn wait_for(&mut self, ndb: &Ndb, ids: &HashSet<[u8; 32]>) -> Vec<NoteKey> {
        let pending = ids.len().saturating_sub(self.delivered);
        let arrived = if pending > 0 {
            self.stream
                .wait_for_notes(pending, INGEST_TIMEOUT)
                .await
                .expect("the host's published notes were never indexed")
        } else {
            Vec::new()
        };
        self.delivered += arrived.len();
        let txn = Transaction::new(ndb).unwrap();
        for id in ids {
            assert!(
                ndb.get_notekey_by_id(&txn, id).is_ok(),
                "a published note arrived under a different id"
            );
        }
        arrived
    }
}

/// A remote device watching the scenario's session: a remote session over the
/// same notes, fed the way the conversation poll feeds one.
///
/// An observer shows the fold, but builds it two ways. A batch that sorts
/// after its tail is appended row by row ([`process_conversation_notes`]'s
/// fast path), and any other rebuilds the chat from ndb
/// ([`rebuild_chat_from_fold`]). The appended rows can drift from the fold on
/// their own, which checks on the loader alone never see.
struct Observer {
    session: ChatSession,
    /// Batches that added rows by appending them, without a rebuild.
    appended_batches: usize,
}

impl Observer {
    fn new() -> Self {
        let mut session = ChatSession::new(
            2,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;
        session.agentic.as_mut().unwrap().event_id = SESSION.to_string();
        Observer {
            session,
            appended_batches: 0,
        }
    }

    /// Hand the session one poll batch, `keys`, then rebuild it if the batch
    /// asked to, as `Dave::deliver_conversation_notes` does. `secret_key` signs
    /// whatever the batch's side effects publish; without one they publish
    /// nothing.
    fn poll(
        &mut self,
        ndb: &Ndb,
        keys: &[NoteKey],
        author: &nostrdb_net::Pubkey,
        secret_key: Option<&[u8; 32]>,
    ) -> ProcessedNotes {
        let txn = Transaction::new(ndb).unwrap();
        let notes = keys
            .iter()
            .map(|key| ndb.get_note_by_key(&txn, *key).unwrap())
            .collect();
        let rows = self.session.chat.len();
        let processed =
            process_conversation_notes(notes, &mut self.session, 2, true, secret_key, ndb);
        drop(txn);
        if processed.rebuild_chat {
            self.rebuild(ndb, author);
        } else if self.session.chat.len() > rows {
            self.appended_batches += 1;
        }
        processed
    }

    /// Rebuild the chat from ndb, as the poll's rebuild pass does.
    fn rebuild(&mut self, ndb: &Ndb, author: &nostrdb_net::Pubkey) {
        let txn = Transaction::new(ndb).unwrap();
        rebuild_chat_from_fold(&mut self.session, ndb, &txn, author);
    }
}

/// The fold every non-host viewer sees: the session loaded from `ndb`.
fn fold_signature(ndb: &Ndb, sk: &[u8; 32]) -> Vec<RowSig> {
    let author = nostrdb_net::FullKeypair::from_secret_bytes(sk)
        .unwrap()
        .pubkey;
    let txn = Transaction::new(ndb).unwrap();
    view_signature(&load_session_messages_for_author(ndb, &txn, &author, SESSION).messages)
}

/// Backfill the PNS envelopes behind `ids` from `from` into a fresh ndb, newest
/// first — the fresh-machine case, where a device receives a session's notes in
/// whatever order the relay hands them over — and return the fold there.
async fn reversed_backfill_signature(
    from: &Ndb,
    sk: &[u8; 32],
    ids: &HashSet<[u8; 32]>,
) -> Vec<RowSig> {
    let envelopes: Vec<String> = {
        let txn = Transaction::new(from).unwrap();
        let mut inner: Vec<_> = ids
            .iter()
            .map(|id| from.get_note_by_id(&txn, id).unwrap())
            .collect();
        inner.sort_by_key(|note| EventOrder::from_note(note));
        inner
            .iter()
            .rev()
            .map(|note| {
                let envelope_id = note
                    .rumor_giftwrap_id()
                    .expect("published notes are PNS rumors");
                let envelope = from.get_note_by_id(&txn, envelope_id).unwrap();
                format!("[\"EVENT\",\"_pns\",{}]", envelope.json().unwrap())
            })
            .collect()
    };

    let dir = TempDir::new().unwrap();
    let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
    assert!(ndb.add_key(sk));
    let mut indexed = IndexWatch::new(&ndb);
    for envelope in &envelopes {
        ndb.process_event(envelope).unwrap();
    }
    indexed.wait_for(&ndb, ids).await;
    fold_signature(&ndb, sk)
}

/// Drive `script` through a fresh host, then assert the host's chat, the fold
/// over what it published, and the fold after a reversed backfill all agree,
/// and that the host converges to the fold at rest.
async fn assert_host_matches_fold(script: Vec<Step>) {
    assert_host_matches_fold_then(script, ReconcileOutcome::Converged).await;
}

/// [`assert_host_matches_fold`] for a script that ends with a message still
/// waiting to be dispatched. A turn is about to start, so the host must not
/// reconcile; its chat already matches the fold.
async fn assert_waiting_host_matches_fold(script: Vec<Step>) {
    assert_host_matches_fold_then(script, ReconcileOutcome::NotReady).await;
}

/// Drive `script`, assert the host's chat and the folds agree, then hand the
/// host its notes back and expect `after_poll` from its reconcile.
async fn assert_host_matches_fold_then(script: Vec<Step>, after_poll: ReconcileOutcome) {
    let mut host = Host::new();
    host.drive(script).await;
    let fold = host.assert_converged("at the end").await;

    assert_eq!(
        host.reconcile_now(),
        ReconcileOutcome::NotReady,
        "a host whose own notes haven't come back through the poll must wait"
    );
    assert_eq!(
        host.poll_and_reconcile(),
        after_poll,
        "the host's reconcile once its notes came back"
    );
    assert_eq!(
        view_signature(&host.session().chat),
        fold,
        "after the reconcile the host's chat is the fold"
    );
}

/// A user message another device (a phone, the `agentium` CLI) sends to the
/// session: stamped now, stored whenever the scenario says.
fn remote_user_note(text: &str) -> BuiltEvent {
    remote_user_note_at(text, now_ms())
}

/// [`remote_user_note`] from a device whose clock reads `at_ms` (unix
/// milliseconds), which may run ahead of the host's.
fn remote_user_note_at(text: &str, at_ms: u64) -> BuiltEvent {
    build_live_event_at(
        text,
        "user",
        SESSION,
        None,
        LiveEventTags::default(),
        &mut ThreadingState::new(),
        &test_secret_key(),
        at_ms,
    )
    .unwrap()
}

/// The host's clock, in unix milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// The steps that open every turn: a user message, dispatched.
fn user_turn(text: &'static str) -> [Step; 2] {
    [Step::Send(text), Step::Dispatch]
}

fn token(text: &str) -> Step {
    Step::Backend(DaveApiResponse::Token(text.to_string()))
}

fn running(tool_use_id: &str, tool_name: &str, summary: &str) -> Step {
    Step::Backend(DaveApiResponse::ToolRunning(RunningTool {
        tool_use_id: tool_use_id.to_string(),
        tool_name: tool_name.to_string(),
        summary: summary.to_string(),
    }))
}

fn executed(
    tool_use_id: &str,
    tool_name: &str,
    summary: &str,
    parent_task_id: Option<&str>,
) -> Step {
    Step::Backend(DaveApiResponse::ToolResult(ExecutedTool {
        tool_name: tool_name.to_string(),
        summary: summary.to_string(),
        output: Some("ok".to_string()),
        parent_task_id: parent_task_id.map(str::to_string),
        file_update: None,
        tool_use_id: Some(tool_use_id.to_string()),
    }))
}

/// The harness smoke test: one user message, one streamed reply. This already
/// converges, so a failure here is the harness, not a gap.
#[tokio::test]
async fn plain_turn() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("hi "),
        token("there"),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// G1: text, a tool, then more text is two assistant segments, each published
/// when it closes so the first sorts ahead of the tool.
#[tokio::test]
async fn multi_segment_turn() {
    let mut script = Vec::from(user_turn("look at the file"));
    script.extend([
        token("let me read it"),
        running("t1", "Read", "src/lib.rs"),
        executed("t1", "Read", "src/lib.rs", None),
        token("it is short"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G2: a result with no tool id and no running row (an auto-accepted tool the
/// backend never announced) is appended where it lands, on both sides.
#[tokio::test]
async fn unpaired_tool_result() {
    let mut script = Vec::from(user_turn("look at the file"));
    script.extend([
        token("let me read it"),
        Step::Backend(DaveApiResponse::ToolResult(ExecutedTool {
            tool_name: "Read".to_string(),
            summary: "src/lib.rs".to_string(),
            output: Some("ok".to_string()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        })),
        token("it is short"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G2: the host upgrades a running tool row in place; the fold pairs the
/// `tool_call` and `tool_result` notes by their shared tool id.
#[tokio::test]
async fn tool_pairing() {
    let mut script = Vec::from(user_turn("run it"));
    script.extend([
        running("t1", "Bash", "cargo test"),
        executed("t1", "Bash", "exit 0", None),
        token("tests pass"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G2: a subagent's internal tool result folds into its row, on the host and
/// (by its `parent-task` tag) in the fold.
#[tokio::test]
async fn subagent_internal_tools() {
    let mut script = Vec::from(user_turn("explore"));
    script.extend([
        Step::Backend(DaveApiResponse::SubagentSpawned(SubagentInfo {
            task_id: "s1".to_string(),
            description: "Map the loader".to_string(),
            subagent_type: "Explore".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background: false,
        })),
        executed("t1", "Grep", "3 matches", Some("s1")),
        Step::Backend(DaveApiResponse::SubagentCompleted {
            task_id: "s1".to_string(),
            result: "found it".to_string(),
        }),
        token("the subagent found it"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G2: a tool still running when the turn ends is finalized on the host, and
/// its made-up result is published so the fold stops its spinner too.
#[tokio::test]
async fn interrupted_tool() {
    let mut script = Vec::from(user_turn("run the slow thing"));
    script.extend([running("t1", "Bash", "sleep 100"), Step::StreamEnd]);
    assert_host_matches_fold(script).await;
}

/// G3: a todo list is published as a `todo` note carrying the TodoWrite JSON,
/// and the fold shows it where the host does.
#[tokio::test]
async fn todo_update() {
    let mut script = Vec::from(user_turn("plan it"));
    script.extend([
        Step::Backend(DaveApiResponse::TodoUpdate(serde_json::json!({
            "todos": [{ "content": "write the harness", "status": "in_progress" }],
        }))),
        token("planned"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G3: a backend failure is published as an `error` note, and the fold renders
/// that role.
#[tokio::test]
async fn failed_error() {
    let mut script = Vec::from(user_turn("do it"));
    script.extend([
        Step::Backend(DaveApiResponse::Failed("rate limited".to_string())),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G3: a turn with no response shows "No response from backend", and
/// publishes it, so the fold shows it too.
#[tokio::test]
async fn empty_response_error() {
    let mut script = Vec::from(user_turn("anyone there?"));
    script.extend([Step::StreamEnd, Step::ExpectRedispatch(None)]);
    assert_host_matches_fold(script).await;
}

/// G4: a tool on the runtime allowlist is auto-accepted; the host publishes
/// the request and an `auto` response, so the fold shows the same resolved row.
#[tokio::test]
async fn allowlist_auto_accept() {
    let input = serde_json::json!({ "command": "ls -la" });
    let grant = input.clone();
    let mut script = Vec::from(user_turn("list files"));
    script.extend([
        Step::Act(Box::new(move |host: &mut Host| {
            let agentic = host.session().agentic.as_mut().unwrap();
            agentic.add_runtime_allow("Bash", &grant);
        })),
        Step::Permission(PermissionRequest::pending(
            uuid::Uuid::new_v4(),
            "Bash".to_string(),
            input,
        )),
        running("t1", "Bash", "ls -la"),
        executed("t1", "Bash", "exit 0", None),
        token("done"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G4: a pending request that a later "Allow Always" covers is resolved by
/// the per-frame status pass, which hands back what it resolved so the host
/// publishes an `auto` response and the fold stops showing it pending.
#[tokio::test]
async fn allow_always_resolves_pending() {
    let input = serde_json::json!({ "command": "cargo build" });
    let grant = input.clone();
    let mut script = Vec::from(user_turn("build it"));
    script.extend([
        Step::Permission(PermissionRequest::pending(
            uuid::Uuid::new_v4(),
            "Bash".to_string(),
            input,
        )),
        Step::Act(Box::new(move |host: &mut Host| {
            let agentic = host.session().agentic.as_mut().unwrap();
            agentic.add_runtime_allow("Bash", &grant);
            let resolved = update_statuses_and_publish_auto_resolved(
                &mut host.sessions,
                &host.ndb,
                host.secret_key.as_ref(),
            );
            assert_eq!(resolved.len(), 1, "the grant covers the pending request");
        })),
        running("t1", "Bash", "cargo build"),
        executed("t1", "Bash", "exit 0", None),
        token("built"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// The user puts an answer to the pending request on hold to type a message
/// first, as Shift+1/2/3 (or Shift and its button) does in the app.
fn hold_answer(tentative: DaveAction) -> Step {
    Step::Act(Box::new(move |host: &mut Host| {
        let result = handle_ui_action(
            tentative,
            &mut host.sessions,
            &ScriptBackend,
            &mut DaveOverlay::None,
            &mut false,
            &egui::Context::default(),
        );
        assert!(matches!(result, UiActionResult::Handled));
    }))
}

/// The user types `message` and presses Send on an answer held by
/// [`hold_answer`]; the host publishes the response.
fn send_held_answer(message: &'static str) -> Step {
    Step::Act(Box::new(move |host: &mut Host| {
        host.session().input = message.to_string();
        let SendActionResult::NeedsRelayPublish(publish) = handle_send_action(
            &mut host.sessions,
            &ScriptBackend,
            &egui::Context::default(),
        ) else {
            panic!("a held answer to a published request publishes a response");
        };
        let sk = host.secret_key.unwrap();
        let engine = embedded_engine(&host.ndb, &sk).unwrap();
        publish_user_permission_response(&mut host.sessions, &engine, &publish);
    }))
}

/// The user rows in the host's chat that read `text`.
fn user_rows(host: &mut Host, text: &str) -> usize {
    host.session()
        .chat
        .iter()
        .filter(|m| matches!(m, Message::User(u) if u.text == text))
        .count()
}

/// An "Allow Always" held for a message waits for it. The per-frame status
/// pass leaves the request pending while the user types, rather than taking
/// it as an auto-accept and dropping the message; the grant lands with the
/// send. So the request shows the user's answer and their reply row, on the
/// host and in the fold, and the grant still auto-accepts the next matching
/// request.
#[tokio::test]
async fn held_allow_always_waits_for_its_message() {
    let (id, next) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let input = serde_json::json!({ "command": "cargo build" });
    let grant = input.clone();
    let mut script = Vec::from(user_turn("build it"));
    script.extend([
        running("t1", "Bash", "cargo build"),
        Step::Permission(PermissionRequest::pending(
            id,
            "Bash".to_string(),
            input.clone(),
        )),
        Step::Settle,
        hold_answer(DaveAction::TentativeAllowAlways),
        Step::Act(Box::new(move |host: &mut Host| {
            let resolved = update_statuses_and_publish_auto_resolved(
                &mut host.sessions,
                &host.ndb,
                host.secret_key.as_ref(),
            );
            assert!(resolved.is_empty(), "a held answer is not an auto-accept");
            let agentic = host.session().agentic.as_ref().unwrap();
            assert!(
                !agentic.should_runtime_allow("Bash", &grant),
                "the grant waits for the send"
            );
        })),
        send_held_answer("release profile only"),
        executed("t1", "Bash", "exit 0", None),
        running("t2", "Bash", "cargo build"),
        Step::Permission(PermissionRequest::pending(next, "Bash".to_string(), input)),
        executed("t2", "Bash", "exit 0", None),
        Step::Act(Box::new(move |host: &mut Host| {
            let chat = &host.session().chat;
            let answered = permission_row(chat, id);
            assert_eq!(
                answered.response,
                Some(crate::messages::PermissionResponseType::Allowed)
            );
            assert!(!answered.auto_accepted, "the user answered it");
            assert!(
                permission_row(chat, next).auto_accepted,
                "the grant covers the next request"
            );
            assert_eq!(user_rows(host, "release profile only"), 1);
        })),
        token("built"),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// A deny held for a reason. The backend hands the reason to the model as a
/// user turn of its own (`deliver_denial_reply`), which the CLI never echoes
/// back as a chat row, and the host shows it once as the response's reply row,
/// as the fold does from the response note. The reply reached the model with
/// the denial, so the turn ends with nothing to redispatch: a host that took
/// the row for a user turn would send the reason a second time.
#[tokio::test]
async fn held_deny_with_reason() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("clean up"));
    script.extend([
        running("t1", "Bash", "rm -rf target"),
        Step::Permission(PermissionRequest::pending(
            id,
            "Bash".to_string(),
            serde_json::json!({ "command": "rm -rf target" }),
        )),
        Step::Settle,
        hold_answer(DaveAction::TentativeDeny),
        send_held_answer("use cargo clean instead"),
        executed("t1", "Bash", "denied", None),
        token("ok, cargo clean"),
        Step::Act(Box::new(|host: &mut Host| {
            assert_eq!(user_rows(host, "use cargo clean instead"), 1);
        })),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// [`held_deny_with_reason`] where the CLI answers the reason's user turn in
/// a turn of its own after the denied one ends: that turn streams in as a
/// wake-up, publishes like any other, and the fold records it.
#[tokio::test]
async fn held_deny_with_reason_answered_in_its_own_turn() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("clean up"));
    script.extend([
        running("t1", "Bash", "rm -rf target"),
        Step::Permission(PermissionRequest::pending(
            id,
            "Bash".to_string(),
            serde_json::json!({ "command": "rm -rf target" }),
        )),
        Step::Settle,
        hold_answer(DaveAction::TentativeDeny),
        send_held_answer("use cargo clean instead"),
        executed("t1", "Bash", "denied", None),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
        Step::AssertConverged("after the denied turn"),
        token("ok, cargo clean"),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// The backend asks one `AskUserQuestion` question, under perm id `id`.
fn ask_question(id: uuid::Uuid) -> Step {
    let questions = serde_json::json!({
        "questions": [{
            "question": "Which approach?",
            "header": "Approach",
            "options": [
                { "label": "Fold", "description": "rebuild from notes" },
                { "label": "Stream", "description": "keep the live chat" },
            ],
        }],
    });
    Step::Permission(PermissionRequest::pending(
        id,
        "AskUserQuestion".to_string(),
        questions,
    ))
}

/// The user answers question `id` with its first option, and the host
/// publishes the response.
fn answer_question(id: uuid::Uuid) -> Step {
    Step::Act(Box::new(move |host: &mut Host| {
        let answers = vec![QuestionAnswer {
            selected: vec![0],
            other_text: None,
        }];
        let publish = crate::update::handle_question_response(&mut host.sessions, id, answers)
            .expect("a local question with a published request publishes a response");
        let sk = host.secret_key.unwrap();
        let engine = embedded_engine(&host.ndb, &sk).unwrap();
        publish_user_permission_response(&mut host.sessions, &engine, &publish);
    }))
}

/// G4: answering a question set shows the formatted answers as a user reply
/// row, on the host and (from the published response) in the fold.
#[tokio::test]
async fn question_reply() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("which way?"));
    script.extend([
        ask_question(id),
        Step::Settle,
        answer_question(id),
        token("going with the fold"),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// G4: a turn can end on a question's reply row. The reply reached the model
/// with the response, so the host redispatches nothing; a host that took the
/// row for a user turn would send the answers again as a new prompt.
#[tokio::test]
async fn turn_ends_on_question_reply() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("which way?"));
    script.extend([
        ask_question(id),
        Step::Settle,
        answer_question(id),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// G5 with G4: a message queued right behind a reply row is redispatched on
/// its own, without the reply; and once dispatched, a message queued before
/// the next turn's first token still waits below that turn's reply.
#[tokio::test]
async fn queued_send_right_behind_question_reply() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("which way?"));
    script.extend([
        ask_question(id),
        Step::Settle,
        answer_question(id),
        Step::Send("also, hurry"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("also, hurry")),
        Step::AssertConverged("while the queued message waits"),
        Step::Dispatch,
        Step::Send("and one more"),
        token("hurrying"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("and one more")),
        Step::Dispatch,
        token("one more, done"),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// A question set another device answers shows the formatted answers as a
/// user reply row on the host, as the fold does from the same response note.
#[tokio::test]
async fn remote_question_reply() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("which way?"));
    script.extend([
        ask_question(id),
        Step::Settle,
        Step::RemoteAnswer(id, RemoteAnswer::FirstOptions),
        token("going with the fold"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// A deny another device sends with a reason shows that reason as a user
/// reply row on the host, as the fold does from the same response note.
#[tokio::test]
async fn remote_deny_with_message() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("clean up"));
    script.extend([
        Step::Permission(PermissionRequest::pending(
            id,
            "Bash".to_string(),
            serde_json::json!({ "command": "rm -rf target" }),
        )),
        Step::Settle,
        Step::RemoteAnswer(id, RemoteAnswer::Deny("use cargo clean instead")),
        token("ok, cargo clean"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G6: compact-and-proceed's local "Proceed…" user message is published, so
/// the fold shows it too. The host dispatches it as the turn ends, as it does
/// any message left waiting.
#[tokio::test]
async fn compact_and_proceed() {
    let mut script = Vec::from(user_turn("approve the plan"));
    script.extend([
        Step::Act(Box::new(|host: &mut Host| {
            host.session().agentic.as_mut().unwrap().compact_intent =
                Some(CompactIntent::ProceedAfterCompaction);
        })),
        Step::Backend(DaveApiResponse::CompactionStarted),
        Step::Backend(DaveApiResponse::CompactionComplete(CompactionInfo {
            pre_tokens: 120_000,
        })),
        token("compacted"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some(crate::session::PROCEED_MESSAGE)),
        Step::Dispatch,
        token("implementing"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G5: a message sent mid-turn is stamped at send time, but the host keeps it
/// trailing until the redispatch. Its `queued` tag holds it at the fold's tail
/// and the dispatch marker then places it where the host did.
#[tokio::test]
async fn queued_send_redispatch() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        token("working on "),
        Step::Send("second, while you work"),
        token("the first"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("second, while you work")),
        Step::Dispatch,
        token("now the second"),
        Step::StreamEnd,
        Step::ExpectRedispatch(None),
    ]);
    assert_host_matches_fold(script).await;
}

/// G5: a queued message the turn ends without dispatching waits at the end of
/// the host's chat, and at the fold's tail.
#[tokio::test]
async fn queued_send_still_waiting() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        token("working on "),
        Step::Send("second, while you work"),
        token("the first"),
        Step::StreamEnd,
    ]);
    assert_waiting_host_matches_fold(script).await;
}

/// G5: a message queued before the turn produced anything still trails the
/// reply on the host, so the fold must hold it back too.
#[tokio::test]
async fn queued_send_before_first_token() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        Step::Send("second, straight away"),
        token("the first"),
        Step::StreamEnd,
        Step::Dispatch,
        token("now the second"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G5 with G4: a question's reply row is the turn's content, so the host puts
/// it above a message queued before the answer. The fold keeps the queued
/// message after it too, while it waits and once dispatched.
#[tokio::test]
async fn queued_send_behind_question_reply() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("which way?"));
    script.extend([
        token("let me ask"),
        ask_question(id),
        Step::Send("also, hurry"),
        Step::Settle,
        answer_question(id),
        token("going with the fold"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("also, hurry")),
        Step::AssertConverged("while the queued message waits"),
        Step::Dispatch,
        token("hurrying"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G5: two messages queued in one turn wait at the tail in the order they
/// were typed, and keep it once dispatched together.
#[tokio::test]
async fn two_queued_in_one_turn() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        token("working on "),
        Step::Send("second"),
        Step::Send("third"),
        token("the first"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("second\nthird")),
        Step::AssertConverged("while both wait"),
        Step::Dispatch,
        token("the second and third"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// G3 with G5: a message queued during a turn the backend answers with
/// nothing. The error row lands above it, and it is redispatched on its own.
#[tokio::test]
async fn queued_send_after_an_empty_response() {
    let mut script = Vec::from(user_turn("anyone there?"));
    script.extend([
        Step::Send("hello?"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("hello?")),
        Step::AssertConverged("while the queued message waits"),
        Step::Dispatch,
        token("here now"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// A run dispatched a second time before the backend answers: the first
/// dispatch took the message off the queue, so the second publishes no
/// marker of its own and the message stays where the first put it.
#[tokio::test]
async fn queued_run_dispatched_twice() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        token("working on "),
        Step::Send("second, while you work"),
        token("the first"),
        Step::StreamEnd,
        Step::Dispatch,
        Step::Dispatch,
        token("now the second"),
        Step::StreamEnd,
    ]);
    let mut host = Host::new();
    host.drive(script).await;
    host.assert_converged("after both dispatches").await;
    assert_eq!(
        host.dispatch_marker_count(),
        1,
        "one marker for the one queued message"
    );
}

/// A queued message can still be waiting once the session is idle: a restart
/// restores it at the tail, or its dispatch found no backend. A message sent
/// then is not queued, but the host dispatches it behind the waiting one. The
/// waiting one's marker places it at the dispatch, so the new one needs a
/// marker too, or the fold puts it first.
#[tokio::test]
async fn send_behind_a_message_still_waiting() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        token("working on "),
        Step::Send("second, while you work"),
        token("the first"),
        Step::StreamEnd,
        Step::Send("hello?"),
        Step::Dispatch,
        token("both, then"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// A phone's message typed while the host was still replying, which reaches
/// the host after the turn ended. The host shows it below the reply and
/// dispatches it straight away, so its dispatch marker must place it there:
/// by when it was typed, the fold would put it above the reply.
#[tokio::test]
async fn late_remote_message_after_the_reply() {
    let typed = remote_user_note("sent from the phone");
    // Strictly before the host's first row, so the fold can't tie-break it
    // after them.
    tokio::time::sleep(Duration::from_millis(2)).await;

    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("hi"),
        Step::StreamEnd,
        Step::Deliver(typed),
        Step::Dispatch,
        token("got your message"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// A restart between turns. The host restores the session from ndb (its
/// chat, dedup set, threading and fold tail) and the next turn carries on
/// from there: the restored rows and the new ones are in the same place on
/// the host and in the fold.
#[tokio::test]
async fn restart_then_send() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("hi"),
        Step::StreamEnd,
        Step::Restart,
        Step::AssertConverged("after the restart"),
    ]);
    script.extend(user_turn("again"));
    script.extend([token("hi again"), Step::StreamEnd]);
    assert_host_matches_fold(script).await;
}

/// [`send_behind_a_message_still_waiting`] through a real restart. The
/// message the turn ended without dispatching comes back from ndb at the
/// tail, still waiting, and a message sent then is dispatched behind it.
#[tokio::test]
async fn restart_then_send_behind_a_message_still_waiting() {
    let mut script = Vec::from(user_turn("first"));
    script.extend([
        token("working on "),
        Step::Send("second, while you work"),
        token("the first"),
        Step::StreamEnd,
        Step::Restart,
        Step::AssertConverged("restored with the message waiting"),
        Step::Act(Box::new(|host: &mut Host| {
            let Some(Message::User(waiting)) = host.session().chat.last() else {
                panic!("the waiting message is restored as the trailing row");
            };
            assert_eq!(waiting.as_str(), "second, while you work");
            assert!(waiting.queued, "it is restored still waiting");
        })),
        Step::Send("hello?"),
        Step::Dispatch,
        token("both, then"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// A phone whose clock runs ahead sends a message after the turn ended. Its
/// stamp is later than everything the host publishes next, the reply to it
/// included, but the host shows it where it arrived, above that reply. Its
/// dispatch marker, on the host's clock, must place it there in the fold
/// too.
#[tokio::test]
async fn remote_message_from_a_clock_running_ahead() {
    let ahead = remote_user_note_at("sent from a phone running fast", now_ms() + 5_000);
    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("hi"),
        Step::StreamEnd,
        Step::Deliver(ahead),
        Step::Dispatch,
        token("got your message"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}

/// Drive `script` through a fresh host and wait for its notes to be indexed.
async fn driven(script: Vec<Step>) -> Host {
    let mut host = Host::new();
    host.drive(script).await;
    host.settle().await;
    host
}

/// An edit's diff past the wire budget is dropped from its `tool_result`
/// note, so the fold shows the tool without it. The host keeps its diff
/// through the reconcile.
#[tokio::test]
async fn reconcile_keeps_dropped_diff() {
    let edit = crate::file_update::FileUpdate::new(
        "big.rs".to_string(),
        crate::file_update::FileUpdateType::Write {
            content: "x".repeat(MAX_WIRE_EVENT_BYTES + 1),
        },
    );
    let mut script = Vec::from(user_turn("write the file"));
    script.extend([
        running("t1", "Write", "big.rs"),
        Step::Backend(DaveApiResponse::ToolResult(ExecutedTool {
            tool_name: "Write".to_string(),
            summary: "big.rs".to_string(),
            output: None,
            parent_task_id: None,
            file_update: Some(edit),
            tool_use_id: Some("t1".to_string()),
        })),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;

    let has_diff = |chat: &[Message]| {
        chat.iter()
            .find_map(|message| match message {
                Message::ToolResponse(resp) => match resp.responses() {
                    ToolResponses::ExecutedTool(tool) => Some(tool.file_update.is_some()),
                    _ => None,
                },
                _ => None,
            })
            .expect("the turn has a tool row")
    };
    let author = host.author();
    let fold_has_diff = {
        let txn = Transaction::new(&host.ndb).unwrap();
        has_diff(&load_session_messages_for_author(&host.ndb, &txn, &author, SESSION).messages)
    };
    assert!(!fold_has_diff, "the wire dropped the diff");

    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    assert!(has_diff(&host.session().chat), "the host kept its diff");
}

/// Tool output too big for the wire is published as a copy cut to fit, which
/// the fold shows; the host keeps the whole output through the reconcile.
///
/// The output is quote- and newline-heavy, so the note's escaping costs far
/// more than the output's raw bytes. That it indexes at all is the budget
/// holding: the pinned nostrdb rejects NIP-44 plaintexts of 32769 to 57344
/// bytes, so a note budgeted by raw output bytes would never come back and
/// `settle` would time out.
#[tokio::test]
async fn reconcile_keeps_capped_tool_output() {
    let output: String = (0..)
        .map(|i| format!("line \"{i}\"\n"))
        .take_while({
            let mut len = 0;
            move |line| {
                len += line.len();
                len <= 2 * MAX_WIRE_EVENT_BYTES
            }
        })
        .collect();
    let mut script = Vec::from(user_turn("print a lot"));
    script.extend([
        running("t1", "Bash", "yes"),
        Step::Backend(DaveApiResponse::ToolResult(ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: "yes".to_string(),
            output: Some(output.clone()),
            parent_task_id: None,
            file_update: None,
            tool_use_id: Some("t1".to_string()),
        })),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;

    let tool_output = |chat: &[Message]| {
        chat.iter()
            .find_map(|message| match message {
                Message::ToolResponse(resp) => match resp.responses() {
                    ToolResponses::ExecutedTool(tool) => tool.output.clone(),
                    _ => None,
                },
                _ => None,
            })
            .expect("the turn has a tool row with output")
    };
    let author = host.author();
    let folded = {
        let txn = Transaction::new(&host.ndb).unwrap();
        tool_output(&load_session_messages_for_author(&host.ndb, &txn, &author, SESSION).messages)
    };
    assert!(folded.len() < output.len(), "the wire carries a cut copy");
    assert!(
        folded.len() > MAX_WIRE_EVENT_BYTES / 4,
        "the cut copy is as big as the budget allows, not a stub: {} bytes",
        folded.len()
    );
    let tail = folded.trim_start_matches("...\n");
    assert!(
        output.ends_with(tail),
        "the cut copy keeps the output's tail"
    );

    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    assert_eq!(
        tool_output(&host.session().chat),
        output,
        "the host kept its whole output"
    );
}

/// Text whose note escapes to several times what one wire note may hold.
fn oversized_text(line: &str) -> String {
    format!("{line} \"quoted\" é🦀\n").repeat(3 * MAX_WIRE_EVENT_BYTES / 20)
}

/// A user message and a reply each too big for one wire note go as several
/// notes, and the fold joins each back into one row: the host, the fold and
/// a reversed backfill agree, and the session rests.
///
/// Each part indexing at all is the split holding: the pinned nostrdb
/// rejects NIP-44 plaintexts of 32769 to 57344 bytes, so a message sent
/// whole would never come back and `settle` would time out.
#[tokio::test]
async fn oversized_messages_converge_as_one_row_each() {
    let ask = oversized_text("ask").leak();
    let reply = oversized_text("reply");
    let mut script = Vec::from(user_turn(ask));
    script.extend([token(&reply), Step::StreamEnd]);
    let mut host = driven(script).await;

    let fold = host.assert_converged("oversized").await;
    assert_eq!(fold.len(), 2, "one row each: {fold:?}");
    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    let texts: Vec<_> = host
        .session()
        .chat
        .iter()
        .map(|message| match message {
            Message::User(user) => user.as_str().to_string(),
            Message::Assistant(assistant) => assistant.text().to_string(),
            other => panic!("unexpected row {other:?}"),
        })
        .collect();
    assert_eq!(texts, [ask.to_string(), reply]);
}

/// Another device's message too big for one note reaches the host as parts.
/// The host takes it once, whole, when its last part arrives.
#[tokio::test]
async fn split_remote_message_dispatches_once_whole() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([token("hi"), Step::StreamEnd]);
    let mut host = driven(script).await;

    let text = oversized_text("from the phone");
    let parts = build_live_events(
        &text,
        "user",
        SESSION,
        None,
        LiveEventTags::default(),
        &mut ThreadingState::new(),
        &test_secret_key(),
    )
    .unwrap();
    let (last, early) = parts.split_last().unwrap();
    assert!(!early.is_empty());
    for part in early {
        host.store_remote(part).await;
        let polled = host.poll_note(&part.note_id);
        assert!(
            polled.remote_user_messages.is_empty(),
            "not before every part is in"
        );
    }

    host.store_remote(last).await;
    let polled = host.poll_note(&last.note_id);
    let sent: Vec<&str> = polled
        .remote_user_messages
        .iter()
        .map(|(_, text)| text.as_str())
        .collect();
    assert_eq!(sent, [text.as_str()]);
    let Some(Message::User(user)) = host.session().chat.last() else {
        panic!("the message is the trailing row");
    };
    assert_eq!(user.note_id, Some(parts[0].note_id));
    assert_eq!(user.as_str(), text);
}

/// A row the host shows but never published is drift: the reconcile reports
/// the row, and the host's chat becomes the fold without it.
#[tokio::test]
async fn reconcile_reports_unpublished_row() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("hi"),
        Step::StreamEnd,
        Step::Act(Box::new(|host: &mut Host| {
            host.session()
                .chat
                .push(Message::System("only on the host".to_string()));
        })),
    ]);
    let mut host = driven(script).await;

    assert_eq!(
        host.poll_and_reconcile(),
        ReconcileOutcome::Drifted(Drift {
            index: 2,
            host: Some(RowSig::System("only on the host".to_string())),
            fold: None,
        })
    );
    let sk = host.secret_key.unwrap();
    assert_eq!(
        view_signature(&host.session().chat),
        fold_signature(&host.ndb, &sk)
    );
    assert_eq!(
        host.reconcile_now(),
        ReconcileOutcome::NotReady,
        "nothing was published since, so there is nothing to do"
    );
}

/// A background subagent outlives its turn. The reconcile at rest rebuilds
/// the chat under it, and its completion on a later wake-up still finds its
/// row, then reconciles again without drift.
#[tokio::test]
async fn reconcile_keeps_background_subagent_live() {
    let mut script = Vec::from(user_turn("explore in the background"));
    script.extend([
        Step::Backend(DaveApiResponse::SubagentSpawned(SubagentInfo {
            task_id: "s1".to_string(),
            description: "Map the loader".to_string(),
            subagent_type: "Explore".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background: true,
        })),
        token("it runs in the background"),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;
    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);

    host.drive(vec![
        Step::Backend(DaveApiResponse::SubagentCompleted {
            task_id: "s1".to_string(),
            result: "mapped it".to_string(),
        }),
        Step::Settle,
    ])
    .await;
    let completed = host.session().chat.iter().any(|message| {
        matches!(message, Message::Subagent(info)
            if info.task_id == "s1" && info.status == SubagentStatus::Completed)
    });
    assert!(completed, "the completion found the rebuilt row");
    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
}

/// A message from another device reaches the host after its turn ended, but
/// was typed before the turn's last row was published (the phone sent it as
/// the reply finished). The fold places it by when it was typed, above that
/// row. The host must still dispatch it, so it stays the trailing user turn:
/// a message waiting for dispatch keeps the session from resting.
#[tokio::test]
async fn late_remote_message_still_dispatches() {
    let mut host = Host::new();
    let typed = remote_user_note("sent from the phone");
    // Strictly before the host's first row, so the fold can't tie-break it
    // after them.
    tokio::time::sleep(Duration::from_millis(2)).await;

    let mut script = Vec::from(user_turn("hello"));
    script.extend([token("hi"), Step::StreamEnd]);
    host.drive(script).await;
    host.settle().await;
    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);

    host.store_remote(&typed).await;
    let polled = host.poll_note(&typed.note_id);
    assert_eq!(polled.remote_user_messages.len(), 1);
    assert_eq!(
        host.reconcile_now(),
        ReconcileOutcome::NotReady,
        "a message waiting for dispatch keeps the session from resting"
    );
    assert!(
        host.session().should_dispatch_remote_message(),
        "the message is still the trailing user turn, so it is dispatched"
    );
}

/// A note stored after the poll ran but before the reconcile read the fold
/// (another device's message landing mid-frame) is left out of it. Folded in,
/// it would be marked seen and the next poll would skip it, so it would show
/// without ever being dispatched or fanned out. The next poll delivers it.
#[tokio::test]
async fn note_stored_after_the_poll_waits_for_the_next_one() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([token("hi"), Step::StreamEnd]);
    let mut host = driven(script).await;

    let phone = remote_user_note("sent from the phone");
    host.store_remote(&phone).await;
    assert_eq!(
        host.poll_and_reconcile(),
        ReconcileOutcome::Converged,
        "the fold leaves out the note the poll hasn't handed over"
    );

    let polled = host.poll_note(&phone.note_id);
    assert_eq!(
        polled.remote_user_messages.len(),
        1,
        "the next poll delivers it as a message to dispatch"
    );
    assert!(host.session().should_dispatch_remote_message());
}

/// The observer is fed live. Past its first batch, which rebuilds because
/// nothing has seeded its tail yet, a turn's rows reach it by appending, so
/// the per-step check compares the fast path against the fold and not only a
/// rebuild.
#[tokio::test]
async fn observer_appends_a_turn_live() {
    let mut script = Vec::from(user_turn("look at the file"));
    script.extend([
        token("let me read it"),
        running("t1", "Read", "src/lib.rs"),
        executed("t1", "Read", "src/lib.rs", None),
        token("it is short"),
        Step::StreamEnd,
    ]);
    let host = driven(script).await;
    assert!(
        host.observer.appended_batches >= 2,
        "the tool's batch and the closing segment append: {} batches did",
        host.observer.appended_batches
    );
}

/// A finished turn, its notes back through the poll, reconciled at rest.
async fn rested() -> Host {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([token("hi"), Step::StreamEnd]);
    let mut host = driven(script).await;
    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    host
}

/// One condition of [`ChatSession::at_rest`] holds a session off the
/// reconcile, and only that one.
///
/// `hold` puts a rested session in the state. Everything else the reconcile
/// waits for is then made true — every note it published is back through the
/// poll, and there is work to do — so only `gate` stands in the way. `release`
/// takes the session back out, after which it reconciles.
async fn assert_rest_gate(gate: &str, hold: Vec<Step>, release: Vec<Step>) {
    let mut host = rested().await;

    host.drive(hold).await;
    host.settle().await;
    host.poll_own_notes();
    let agentic = host.session().agentic.as_mut().unwrap();
    assert!(
        agentic.unindexed_self_notes.is_empty(),
        "{gate}: every note the host published is back"
    );
    agentic.fold_dirty = true;
    assert!(
        !host.session().at_rest(),
        "{gate}: the session isn't at rest"
    );
    assert_eq!(
        host.reconcile_now(),
        ReconcileOutcome::NotReady,
        "{gate}: the session must not reconcile"
    );

    host.drive(release).await;
    host.settle().await;
    assert_eq!(
        host.poll_and_reconcile(),
        ReconcileOutcome::Converged,
        "{gate}: released, the session rests"
    );
}

/// Mid-turn between rows: a tool finished and nothing is streaming, but the
/// turn is still dispatched.
#[tokio::test]
async fn rest_gate_dispatched() {
    let mut hold = Vec::from(user_turn("run it"));
    hold.extend([
        running("t1", "Bash", "cargo test"),
        executed("t1", "Bash", "exit 0", None),
    ]);
    assert_rest_gate(
        "dispatched",
        hold,
        vec![token("tests pass"), Step::StreamEnd],
    )
    .await;
}

/// The backend's task for the session is still running.
#[tokio::test]
async fn rest_gate_backend_task() {
    assert_rest_gate(
        "backend task",
        vec![Step::Act(Box::new(|host: &mut Host| {
            host.session().task_handle = Some(tokio::spawn(async {}));
        }))],
        vec![Step::Act(Box::new(|host: &mut Host| {
            host.session().task_handle = None;
        }))],
    )
    .await;
}

/// A spontaneous wake-up turn streams while the session is idle: no
/// dispatch, but an assistant segment is open.
#[tokio::test]
async fn rest_gate_wake_up_streaming_while_idle() {
    assert_rest_gate(
        "wake-up segment",
        vec![token("woke up")],
        vec![Step::StreamEnd],
    )
    .await;
}

/// A wake-up turn's tool is running, with no dispatch and no open segment.
#[tokio::test]
async fn rest_gate_running_tool() {
    assert_rest_gate(
        "running tool",
        vec![running("t1", "Bash", "sleep 1")],
        vec![executed("t1", "Bash", "exit 0", None), Step::StreamEnd],
    )
    .await;
}

/// A permission request waits for an answer.
#[tokio::test]
async fn rest_gate_pending_permission() {
    let id = uuid::Uuid::new_v4();
    assert_rest_gate(
        "pending permission",
        vec![Step::Permission(PermissionRequest::pending(
            id,
            "Bash".to_string(),
            serde_json::json!({ "command": "rm -rf target" }),
        ))],
        vec![
            Step::RemoteAnswer(id, RemoteAnswer::Deny("not now")),
            Step::StreamEnd,
        ],
    )
    .await;
}

/// A compaction is under way.
#[tokio::test]
async fn rest_gate_compaction() {
    assert_rest_gate(
        "compaction",
        vec![Step::Backend(DaveApiResponse::CompactionStarted)],
        vec![
            Step::Backend(DaveApiResponse::CompactionComplete(CompactionInfo {
                pre_tokens: 120_000,
            })),
            Step::StreamEnd,
        ],
    )
    .await;
}

/// A user message waits at the end of the chat to be dispatched.
#[tokio::test]
async fn rest_gate_waiting_user_message() {
    assert_rest_gate(
        "waiting user message",
        vec![Step::Send("and another thing")],
        vec![Step::Dispatch, token("noted"), Step::StreamEnd],
    )
    .await;
}

/// A remote session already shows the fold, so it never reconciles.
#[tokio::test]
async fn rest_gate_remote_session() {
    assert_rest_gate(
        "remote session",
        vec![Step::Act(Box::new(|host: &mut Host| {
            host.session().source = SessionSource::Remote;
        }))],
        vec![Step::Act(Box::new(|host: &mut Host| {
            host.session().source = SessionSource::Local;
        }))],
    )
    .await;
}

/// A chat-mode backend publishes nothing to fold.
#[tokio::test]
async fn rest_gate_chat_mode_backend() {
    assert_rest_gate(
        "chat-mode backend",
        vec![Step::Act(Box::new(|host: &mut Host| {
            host.session().backend_type = BackendType::OpenAI;
        }))],
        vec![Step::Act(Box::new(|host: &mut Host| {
            host.session().backend_type = BackendType::Claude;
        }))],
    )
    .await;
}

/// A row the host shows in the middle of its chat but never published is
/// drift at that row, against the fold's row there.
#[tokio::test]
async fn reconcile_reports_middle_row_drift() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("hi"),
        Step::StreamEnd,
        Step::Act(Box::new(|host: &mut Host| {
            host.session()
                .chat
                .insert(1, Message::System("only on the host".to_string()));
        })),
    ]);
    let mut host = driven(script).await;

    assert_eq!(
        host.poll_and_reconcile(),
        ReconcileOutcome::Drifted(Drift {
            index: 1,
            host: Some(RowSig::System("only on the host".to_string())),
            fold: Some(RowSig::Assistant("hi".to_string())),
        })
    );
    let sk = host.secret_key.unwrap();
    assert_eq!(
        view_signature(&host.session().chat),
        fold_signature(&host.ndb, &sk)
    );
}

/// A published row the host no longer shows is drift too: the fold has a row
/// past the end of the host's chat, which the reconcile puts back.
#[tokio::test]
async fn reconcile_reports_fold_only_row() {
    let mut script = Vec::from(user_turn("run it"));
    script.extend([
        running("t1", "Bash", "cargo test"),
        executed("t1", "Bash", "exit 0", None),
        token("tests pass"),
        Step::StreamEnd,
        Step::Act(Box::new(|host: &mut Host| {
            host.session().chat.pop();
        })),
    ]);
    let mut host = driven(script).await;

    assert_eq!(
        host.poll_and_reconcile(),
        ReconcileOutcome::Drifted(Drift {
            index: 2,
            host: None,
            fold: Some(RowSig::Assistant("tests pass".to_string())),
        })
    );
    let sk = host.secret_key.unwrap();
    assert_eq!(
        view_signature(&host.session().chat),
        fold_signature(&host.ndb, &sk)
    );
}

/// A background subagent whose row the reconcile moves still finds it when
/// it completes: the reconcile re-indexes the rows it installs.
///
/// The row moves because the host showed a row above it that it never
/// published (drift, which no correct host has today), so the fold puts the
/// subagent one row higher than the host had it.
#[tokio::test]
async fn reconcile_moves_a_background_subagent_row() {
    let mut script = Vec::from(user_turn("explore in the background"));
    script.extend([
        Step::Act(Box::new(|host: &mut Host| {
            host.session()
                .chat
                .push(Message::System("only on the host".to_string()));
        })),
        Step::Backend(DaveApiResponse::SubagentSpawned(SubagentInfo {
            task_id: "s1".to_string(),
            description: "Map the loader".to_string(),
            subagent_type: "Explore".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background: true,
        })),
        token("it runs in the background"),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;
    let subagent_row = |host: &mut Host| host.session().turn_rows().subagent("s1").unwrap();
    assert_eq!(subagent_row(&mut host), 2);
    assert!(matches!(
        host.poll_and_reconcile(),
        ReconcileOutcome::Drifted(Drift { index: 1, .. })
    ));
    assert_eq!(subagent_row(&mut host), 1, "the index follows the row");

    host.drive(vec![
        Step::Backend(DaveApiResponse::SubagentCompleted {
            task_id: "s1".to_string(),
            result: "mapped it".to_string(),
        }),
        Step::Settle,
    ])
    .await;
    let completed = host.session().chat.iter().any(|message| {
        matches!(message, Message::Subagent(info)
            if info.task_id == "s1" && info.status == SubagentStatus::Completed)
    });
    assert!(completed, "the completion found the moved row");
    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
}

/// The request row with id `id` in `chat`.
fn permission_row(chat: &[Message], id: uuid::Uuid) -> &PermissionRequest {
    chat.iter()
        .find_map(|message| match message {
            Message::PermissionRequest(req) if req.id == id => Some(req),
            _ => None,
        })
        .expect("the request's row")
}

/// The fold over the host's notes.
fn fold_messages(host: &Host) -> Vec<Message> {
    let author = host.author();
    let txn = Transaction::new(&host.ndb).unwrap();
    load_session_messages_for_author(&host.ndb, &txn, &author, SESSION).messages
}

/// A permission's tool input too big for the wire is cut in its note, so the
/// fold shows a cut copy and the view inferred from it: here a plan review of
/// a cut plan. The host keeps its whole input, and the view built from that,
/// through the reconcile.
#[tokio::test]
async fn reconcile_keeps_permission_tool_input() {
    let id = uuid::Uuid::new_v4();
    let input = serde_json::json!({
        "plan": "- a step of the plan\n".repeat(2 * MAX_WIRE_EVENT_BYTES / 20),
    });
    let mut script = Vec::from(user_turn("plan it"));
    script.extend([
        Step::Permission(PermissionRequest::pending(
            id,
            "ExitPlanMode".to_string(),
            input.clone(),
        )),
        Step::RemoteAnswer(id, RemoteAnswer::Deny("too long")),
        token("ok, a shorter one"),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;

    // The view renders the plan, so a cut plan renders shorter.
    let view_len = |req: &PermissionRequest| format!("{:?}", req.view).len();
    let host_view = view_len(permission_row(&host.session().chat, id));
    let fold = fold_messages(&host);
    let folded = permission_row(&fold, id);
    assert_ne!(folded.tool_input, input, "the wire cut the input");
    assert!(
        view_len(folded) < host_view,
        "the fold's view is of the cut input"
    );

    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    let kept = permission_row(&host.session().chat, id);
    assert_eq!(kept.tool_input, input, "the host kept its whole input");
    assert_eq!(view_len(kept), host_view, "and the view built from it");
}

/// A question's answer summary isn't sent, so the fold's question row has
/// none. The host keeps the one it computed through the reconcile.
#[tokio::test]
async fn reconcile_keeps_question_answer_summary() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("which way?"));
    script.extend([
        ask_question(id),
        Step::Settle,
        answer_question(id),
        token("going with the fold"),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;

    assert!(permission_row(&host.session().chat, id)
        .answer_summary
        .is_some());
    assert!(
        permission_row(&fold_messages(&host), id)
            .answer_summary
            .is_none(),
        "the wire has no answer summary"
    );

    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    assert!(
        permission_row(&host.session().chat, id)
            .answer_summary
            .is_some(),
        "the host kept its summary"
    );
}

/// Output too big for the wire from a tool a subagent ran is published cut,
/// nested in the subagent's row in the fold. The host keeps the whole output
/// on the nested tool through the reconcile.
#[tokio::test]
async fn reconcile_keeps_subagent_tool_output() {
    let output = "line \"quoted\"\n".repeat(2 * MAX_WIRE_EVENT_BYTES / 14);
    let mut script = Vec::from(user_turn("explore"));
    script.extend([
        Step::Backend(DaveApiResponse::SubagentSpawned(SubagentInfo {
            task_id: "s1".to_string(),
            description: "Map the loader".to_string(),
            subagent_type: "Explore".to_string(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background: false,
        })),
        Step::Backend(DaveApiResponse::ToolResult(ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: "yes".to_string(),
            output: Some(output.clone()),
            parent_task_id: Some("s1".to_string()),
            file_update: None,
            tool_use_id: Some("t1".to_string()),
        })),
        Step::Backend(DaveApiResponse::SubagentCompleted {
            task_id: "s1".to_string(),
            result: "found it".to_string(),
        }),
        Step::StreamEnd,
    ]);
    let mut host = driven(script).await;

    let nested_output = |chat: &[Message]| {
        chat.iter()
            .find_map(|message| match message {
                Message::Subagent(info) => info.tool_results.first()?.output.clone(),
                _ => None,
            })
            .expect("the subagent's row has its tool, with output")
    };
    assert!(
        nested_output(&fold_messages(&host)).len() < output.len(),
        "the wire carries a cut copy"
    );

    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    assert_eq!(
        nested_output(&host.session().chat),
        output,
        "the host kept the whole output"
    );
}

/// Images aren't sent, so the fold's user row has none. The host keeps the
/// images it sent through the reconcile.
#[tokio::test]
async fn reconcile_keeps_user_images() {
    let mut script = vec![Step::Act(Box::new(|host: &mut Host| {
        let session = host.sessions.get_mut(host.sid).unwrap();
        record_user_message(
            session,
            &host.ndb,
            host.secret_key.as_ref(),
            "what is this?".to_string(),
            vec![ImageAttachment::new(
                vec![0x89, b'P', b'N', b'G'],
                "image/png",
            )],
        );
    }))];
    script.extend([Step::Dispatch, token("a png"), Step::StreamEnd]);
    let mut host = driven(script).await;

    let images = |chat: &[Message]| match chat.first() {
        Some(Message::User(user)) => user.images.len(),
        other => panic!("the user's message comes first, not {other:?}"),
    };
    assert_eq!(images(&fold_messages(&host)), 0, "the wire has no images");

    assert_eq!(host.poll_and_reconcile(), ReconcileOutcome::Converged);
    assert_eq!(images(&host.session().chat), 1, "the host kept its image");
}

/// The end of a turn reconciles each session that ended (see
/// [`reconcile_ended_turns`]), but not one about to dispatch again or compact,
/// nor one whose turn didn't end.
#[tokio::test]
async fn ended_turns_reconcile_unless_more_is_coming() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([token("hi"), Step::StreamEnd]);
    let mut host = driven(script).await;
    host.poll_own_notes();
    let sid = host.sid;
    let author = host.author();
    let none = HashSet::new();
    let only = HashSet::from([sid]);
    let dirty = |host: &mut Host| host.session().agentic.as_ref().unwrap().fold_dirty;

    let end = |host: &mut Host, ended: &[SessionId], send, compact| {
        reconcile_ended_turns(&mut host.sessions, ended, send, compact, &host.ndb, &author);
    };
    end(&mut host, &[], &none, &none);
    assert!(dirty(&mut host), "a session whose turn didn't end waits");
    end(&mut host, &[sid], &only, &none);
    assert!(dirty(&mut host), "one about to dispatch again waits");
    end(&mut host, &[sid], &none, &only);
    assert!(dirty(&mut host), "one about to compact waits");
    end(&mut host, &[sid], &none, &none);
    assert!(!dirty(&mut host), "a turn that ended at rest reconciles");
}

/// The conversation poll delivers a batch to a local session and then
/// reconciles it (`Dave::deliver_conversation_notes`), so a turn whose last
/// notes come back through the poll ends with the host's chat as the fold.
#[tokio::test]
async fn poll_delivery_reconciles_a_local_session() {
    let mut script = Vec::from(user_turn("hello"));
    script.extend([token("hi"), Step::StreamEnd]);
    let mut host = driven(script).await;
    let sk = host.secret_key.unwrap();
    let author = host.author();
    let keys: Vec<NoteKey> = {
        let txn = Transaction::new(&host.ndb).unwrap();
        host.published_note_ids()
            .iter()
            .map(|id| host.ndb.get_notekey_by_id(&txn, id).unwrap())
            .collect()
    };

    let data_dir = TempDir::new().unwrap();
    let mut dave = test_dave(&DataPath::new(data_dir.path()));
    dave.session_manager = std::mem::take(&mut host.sessions);
    let remote = dave.deliver_conversation_notes(
        &host.ndb,
        Some(&sk),
        &author,
        HashMap::from([(host.sid, keys)]),
    );
    host.sessions = std::mem::take(&mut dave.session_manager);

    assert!(remote.is_empty(), "the batch is the host's own notes");
    let agentic = host.session().agentic.as_ref().unwrap();
    assert!(agentic.unindexed_self_notes.is_empty());
    assert!(!agentic.fold_dirty, "the delivery reconciled the session");
    assert_eq!(
        view_signature(&host.session().chat),
        fold_signature(&host.ndb, &sk)
    );
}

/// Answering a remote session's request publishes a response but records
/// nothing on that session: its reply row comes from the echo of the note,
/// which a recorded note would be skipped as. Answering a local session's
/// request records it, as the host's own note.
#[tokio::test]
async fn answering_a_remote_request_records_nothing() {
    let id = uuid::Uuid::new_v4();
    let mut script = Vec::from(user_turn("clean up"));
    script.extend([
        Step::Permission(PermissionRequest::pending(
            id,
            "Bash".to_string(),
            serde_json::json!({ "command": "rm -rf target" }),
        )),
        Step::Settle,
    ]);
    let mut host = driven(script).await;
    let sk = host.secret_key.unwrap();
    let engine = embedded_engine(&host.ndb, &sk).unwrap();
    let publish = PermissionPublish {
        perm_id: id,
        event_session_id: SESSION.to_string(),
        request_note_id: host
            .session()
            .agentic
            .as_ref()
            .unwrap()
            .permissions
            .request_note_ids[&id],
        allowed: true,
        message: None,
        cancel_turn: false,
    };
    let recorded = |host: &mut Host| host.published_note_ids().len();

    host.session().source = SessionSource::Remote;
    let before = recorded(&mut host);
    publish_user_permission_response(&mut host.sessions, &engine, &publish);
    assert_eq!(
        recorded(&mut host),
        before,
        "a remote session records nothing"
    );
    let arrived = host
        .indexed
        .stream
        .wait_for_notes(1, INGEST_TIMEOUT)
        .await
        .expect("the response was published");
    host.indexed.delivered += arrived.len();
    {
        let txn = Transaction::new(&host.ndb).unwrap();
        let response = host.ndb.get_note_by_key(&txn, arrived[0]).unwrap();
        assert_eq!(
            agentium_core::session_events::get_tag_value(&response, "role"),
            Some("permission_response")
        );
    }

    host.session().source = SessionSource::Local;
    publish_user_permission_response(&mut host.sessions, &engine, &publish);
    assert_eq!(
        recorded(&mut host),
        before + 1,
        "a local session records its own response"
    );
}

/// A remote session's rebuild folds only what the poll has handed it
/// (`seen_through`). A note stored after the poll ran, before a rebuild
/// (an earlier batch's), is left out of that rebuild and not marked seen. The
/// next poll then delivers it as new and runs its side effects: here the
/// observer's runtime allowlist auto-accepts the request.
#[tokio::test]
async fn remote_rebuild_leaves_a_late_note_to_the_next_poll() {
    let id = uuid::Uuid::new_v4();
    let input = serde_json::json!({ "command": "ls -la" });
    let mut host = driven(Vec::from(user_turn("list files"))).await;
    let grant = input.clone();
    host.observer
        .session
        .agentic
        .as_mut()
        .unwrap()
        .add_runtime_allow("Bash", &grant);

    // Applied without driving, so the observer isn't fed the request.
    host.apply(Step::Permission(PermissionRequest::pending(
        id,
        "Bash".to_string(),
        input,
    )));
    host.settle().await;
    let request_note = host
        .session()
        .agentic
        .as_ref()
        .unwrap()
        .permissions
        .request_note_ids[&id];

    let author = host.author();
    host.observer.rebuild(&host.ndb, &author);
    let observed = &host.observer.session;
    assert!(
        !observed
            .chat
            .iter()
            .any(|m| matches!(m, Message::PermissionRequest(_))),
        "the rebuild leaves out the note the poll hasn't handed over"
    );
    assert!(!observed
        .agentic
        .as_ref()
        .unwrap()
        .seen_note_ids
        .contains(&request_note));

    let keys = std::mem::take(&mut host.unobserved);
    let sk = host.secret_key.unwrap();
    host.observer.poll(&host.ndb, &keys, &author, Some(&sk));
    let req = permission_row(&host.observer.session.chat, id);
    assert_eq!(
        req.response,
        Some(crate::messages::PermissionResponseType::Allowed),
        "the next poll ran the request's auto-accept"
    );
    assert!(req.auto_accepted);
}

/// A note another device stamped with `queued`, as a sender does while the
/// session is mid-turn.
fn remote_queued_note(text: &str) -> BuiltEvent {
    build_live_event_at(
        text,
        "user",
        SESSION,
        None,
        LiveEventTags {
            queued: true,
            ..Default::default()
        },
        &mut ThreadingState::new(),
        &test_secret_key(),
        now_ms(),
    )
    .unwrap()
}

/// Two messages from other devices reach the host mid-turn in the reverse of
/// the order they were typed. The host queues them as they arrive and
/// dispatches them in that order, and their dispatch markers put them in that
/// order in the fold too.
#[tokio::test]
async fn remote_messages_arriving_out_of_order_mid_turn() {
    let typed_first = remote_queued_note("typed first");
    tokio::time::sleep(Duration::from_millis(2)).await;
    let typed_second = remote_queued_note("typed second");

    let mut script = Vec::from(user_turn("hello"));
    script.extend([
        token("working "),
        Step::Deliver(typed_second),
        Step::Deliver(typed_first),
        token("on it"),
        Step::StreamEnd,
        Step::ExpectRedispatch(Some("typed second\ntyped first")),
        Step::Dispatch,
        token("both of them"),
        Step::StreamEnd,
    ]);
    assert_host_matches_fold(script).await;
}
