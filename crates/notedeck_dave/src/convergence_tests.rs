//! Host-vs-fold convergence harness (headway:dave/soup-neck-green).
//!
//! A Dave session should look the same to everyone viewing it: the host while
//! the turn streams, the host after a restart, a remote observer and the
//! `agentium` CLI. All but the first are one fold over the session's kind-1988
//! notes ([`load_session_messages_for_author`]); the host builds its live chat
//! straight from the backend stream instead. Each test here drives a scripted
//! backend stream through the host's real code path ([`apply_response`],
//! [`handle_stream_end`], the user-send funnel), waits for every note the host
//! published to land in ndb, then asserts that the host's chat and the fold
//! have the same [`view_signature`] — and that the fold is the same again when
//! those notes are backfilled into a fresh ndb in reverse order.
//!
//! Then the host's notes come back through the conversation poll, and the host
//! reconciles its chat to the fold at rest ([`maybe_reconcile_at_rest`]). Every
//! scenario must converge there without a drift, and leave the host's chat
//! equal to the fold.
//!
//! A scenario that fails today is `#[ignore]`d with the converge card that
//! fixes it; that card un-ignores it.

use crate::backend::BackendType;
use crate::config::AiMode;
use crate::conversation::{process_conversation_notes, ProcessedNotes};
use crate::messages::{
    CompactionInfo, PendingPermission, PermissionRequest, QuestionAnswer, RunningTool,
    SubagentInfo, SubagentStatus,
};
use crate::publish::{
    pns_ingest, publish_auto_accept_response, publish_user_permission_response, record_dispatch,
    record_user_message, MAX_TOOL_OUTPUT_WIRE_BYTES,
};
use crate::reconcile::{maybe_reconcile_at_rest, Drift, ReconcileOutcome};
use crate::session::{ChatSession, CompactIntent, SessionId, SessionManager};
use crate::stream_events::{apply_response, handle_stream_end, ApplyCtx};
use crate::tests::{test_config, test_secret_key};
use crate::tools::ToolResponses;
use crate::{embedded_engine, DaveApiResponse, ExecutedTool, Message, PermissionResponse};
use agentium_core::session_events::{
    build_live_event, BuiltEvent, LiveEventTags, ThreadingState, AI_CONVERSATION_KIND,
};
use agentium_core::session_loader::{
    load_session_messages_for_author, view_signature, EventOrder, RowSig,
};
use nostrdb::{Filter, Ndb, SubscriptionStream, Transaction};
use std::collections::HashSet;
use std::path::PathBuf;
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
    /// The host dispatches the trailing user message(s) to the backend.
    Dispatch,
    /// The backend streams a response.
    Backend(DaveApiResponse),
    /// The backend asks permission to run a tool; the harness holds the
    /// response channel so the host's answer has somewhere to go.
    Permission(PermissionRequest),
    /// The backend ends the turn.
    StreamEnd,
    /// Wait until everything published so far is indexed: the moment a user
    /// takes to read a request before answering it, whose answer is built
    /// from the request's note in ndb.
    Settle,
    /// Anything else the host does between responses (a UI action, a grant).
    Act(Box<dyn FnOnce(&mut Host)>),
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
    /// The session's conversation notes as they commit, subscribed before
    /// anything was published.
    indexed: IndexWatch,
    _dir: TempDir,
}

impl Host {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let sk = test_secret_key();
        assert!(ndb.add_key(&sk), "ndb must accept the PNS key");
        let indexed = IndexWatch::new(&ndb);
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
            indexed,
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
            Step::Dispatch => record_dispatch(session, &self.ndb, self.secret_key.as_ref()),
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
                &mut HashSet::new(),
                &mut HashSet::new(),
            ),
            Step::Act(act) => act(self),
            Step::Settle => unreachable!("settling is async; `drive` awaits it"),
        }
    }

    /// Run a script through the host.
    async fn drive(&mut self, script: Vec<Step>) {
        for step in script {
            match step {
                Step::Settle => self.settle().await,
                step => self.apply(step),
            }
        }
    }

    /// Wait until every note the host has published so far is indexed.
    async fn settle(&mut self) {
        let ids = self.published_note_ids();
        self.indexed.wait_for(&self.ndb, &ids).await;
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
        let ids = self.published_note_ids();
        let author = self.author();
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
        drop(txn);
        maybe_reconcile_at_rest(session, &self.ndb, &author)
    }

    /// A user message another device (a phone, the `agentium` CLI) sends to
    /// the session: stamped now, stored whenever the scenario says.
    fn remote_user_note(&self, text: &str) -> BuiltEvent {
        build_live_event(
            text,
            "user",
            SESSION,
            None,
            LiveEventTags::default(),
            &mut ThreadingState::new(),
            &self.secret_key.unwrap(),
        )
        .unwrap()
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
        self.indexed.wait_for(&self.ndb, &ids).await;
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

/// Counts the scenario session's conversation notes as they commit to an ndb.
///
/// Subscribed before anything is ingested, so nothing slips past it. Waiting
/// counts deliveries rather than polling for ids: each published note matches
/// the session filter and is written once, so N deliveries is exactly "N
/// committed". The stream is held for the whole scenario because dropping it
/// unsubscribes.
struct IndexWatch {
    stream: SubscriptionStream,
    delivered: usize,
}

impl IndexWatch {
    fn new(ndb: &Ndb) -> Self {
        let filter = Filter::new()
            .kinds([AI_CONVERSATION_KIND as u64])
            .tags([SESSION], 'd')
            .build();
        let sub = ndb.subscribe(&[filter]).unwrap();
        IndexWatch {
            stream: SubscriptionStream::new(ndb.clone(), sub),
            delivered: 0,
        }
    }

    /// Wait until every note in `ids` — all this ndb's session notes so far —
    /// is indexed.
    async fn wait_for(&mut self, ndb: &Ndb, ids: &HashSet<[u8; 32]>) {
        let pending = ids.len().saturating_sub(self.delivered);
        if pending > 0 {
            let arrived = self
                .stream
                .wait_for_notes(pending, INGEST_TIMEOUT)
                .await
                .expect("the host's published notes were never indexed");
            self.delivered += arrived.len();
        }
        let txn = Transaction::new(ndb).unwrap();
        for id in ids {
            assert!(
                ndb.get_notekey_by_id(&txn, id).is_ok(),
                "a published note arrived under a different id"
            );
        }
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
    host.settle().await;

    let ids = host.published_note_ids();
    let sk = host.secret_key.unwrap();

    let host_view = view_signature(&host.session().chat);
    let fold = fold_signature(&host.ndb, &sk);
    assert_eq!(
        host_view, fold,
        "the host's chat and the fold over its notes disagree"
    );

    let backfilled = reversed_backfill_signature(&host.ndb, &sk, &ids).await;
    assert_eq!(
        fold, backfilled,
        "the fold depends on the order the notes were ingested"
    );

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
    script.extend([token("hi "), token("there"), Step::StreamEnd]);
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
    script.push(Step::StreamEnd);
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
            let resolved = host.sessions.update_all_statuses();
            assert_eq!(resolved.len(), 1, "the grant covers the pending request");
            let sk = host.secret_key.unwrap();
            for auto in resolved {
                let session = host.sessions.get_mut(auto.session).unwrap();
                publish_auto_accept_response(session, auto.perm_id, &host.ndb, &sk);
            }
        })),
        running("t1", "Bash", "cargo build"),
        executed("t1", "Bash", "exit 0", None),
        token("built"),
        Step::StreamEnd,
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
        Step::Dispatch,
        token("now the second"),
        Step::StreamEnd,
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
/// message after it too, while it waits and, when `dispatch`, once dispatched.
fn queued_behind_question_reply(dispatch: bool) -> Vec<Step> {
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
    ]);
    if dispatch {
        script.extend([Step::Dispatch, token("hurrying"), Step::StreamEnd]);
    }
    script
}

#[tokio::test]
async fn queued_send_behind_question_reply_waiting() {
    assert_waiting_host_matches_fold(queued_behind_question_reply(false)).await;
}

#[tokio::test]
async fn queued_send_behind_question_reply_dispatched() {
    assert_host_matches_fold(queued_behind_question_reply(true)).await;
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
///
/// Capped *output* can't be tested the same way yet: a capped 40KB copy makes
/// an inner event over 32KB, which nostrdb's NIP-44 unpad rejects (its
/// `calc_padded_len` overflows a `uint16_t` past 32768), so that note never
/// indexes and the session never reconciles.
#[tokio::test]
async fn reconcile_keeps_dropped_diff() {
    let edit = crate::file_update::FileUpdate::new(
        "big.rs".to_string(),
        crate::file_update::FileUpdateType::Write {
            content: "x".repeat(MAX_TOOL_OUTPUT_WIRE_BYTES + 1),
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
    let typed = host.remote_user_note("sent from the phone");
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

    let phone = host.remote_user_note("sent from the phone");
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
