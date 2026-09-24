//! Kind-31989 session commands between hosts: decoding a spawn/resume command
//! addressed to this host, applying it (with spawn idempotency), and queueing
//! our own spawn/resume commands for a remote host.

use crate::backend::{BackendType, Model};
use crate::{session_events, session_loader, update, Dave, DaveOverlay, SessionId};
use claude_agent_sdk_rs::PermissionMode;
use nostrdb::Transaction;
use notedeck::AppContext;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A pending spawn command waiting to be built and published.
pub(crate) struct PendingSpawnCommand {
    pub(crate) target_host: String,
    pub(crate) cwd: PathBuf,
    pub(crate) backend: BackendType,
    /// UUID that links this command to the placeholder session and the
    /// kind-31988 response from the remote host.
    pub(crate) spawn_id: String,
}

/// A pending kind-31989 resume command waiting to be built and published,
/// mirroring [`PendingSpawnCommand`]. Queued when a deleted `agentium:` chip for
/// a session on *another* host is clicked: we can't revive it locally (no
/// backend here), so we ask its host to reopen + revive + resume it. The revived
/// kind-31988 state streams back and re-renders the chip live.
pub(crate) struct PendingResumeCommand {
    pub(crate) target_host: String,
    pub(crate) cwd: PathBuf,
    pub(crate) backend: BackendType,
    pub(crate) spawn_id: String,
    /// kind-31988 d-tag of the session to revive (its stable `agentium:`
    /// identity). The receiving host resolves and reopens *this* session.
    pub(crate) target_session_id: String,
    /// Real CLI session id for `claude --resume` (empty if the backend on the
    /// remote host never started).
    pub(crate) cli_session_id: String,
}

/// What a kind-31989 `spawn_session` command asks for — the *request*, held
/// apart from the command's own transmission identity (its `d`-tag).
///
/// The distinction is the whole point of `idempotency_key`: the d-tag and
/// `spawn_id` are fresh per transmission, so two commands that are the same
/// request look entirely unrelated without a key that says otherwise.
struct SpawnRequest {
    /// Working directory the new session runs in.
    cwd: String,
    /// Backend to launch (defaulted by `decode_session_command` when the command
    /// omits or mis-spells its `backend` tag).
    backend: BackendType,
    /// UUID linking the session back to the sender's placeholder.
    spawn_id: Option<String>,
    /// Explicit session title from the command's `custom_title` tag, when the
    /// spawner set one. Stamped into the new session's `custom_title` so it
    /// shows immediately and no later message overwrites it; `None` lets the
    /// title derive from the first message as before.
    custom_title: Option<String>,
    /// The new session's first `user` message, from the command's `prompt`
    /// tag. Delivered locally the moment the session is materialized, so a
    /// spawner's first message lands even when this host answered too slowly
    /// for the spawner's own delivery to reach the session. `None` leaves the
    /// session idle awaiting a message.
    prompt: Option<String>,
    /// The permission mode the spawner asked the session to start in, from
    /// the command's `permission_mode` tag. `None` — the tag absent, or
    /// naming a mode this build doesn't know — leaves the new session on the
    /// host's own default rather than guessing.
    permission_mode: Option<PermissionMode>,
    /// The request's identity, from the command's `idempotency_key` tag.
    /// Two commands carrying the same key are one spawn asked for twice, so
    /// the second is answered with the session the first produced rather
    /// than materializing another agent in the same worktree. `None` (an
    /// older CLI, or an explicit opt-out) leaves the spawn with no
    /// idempotency, which is how it always behaved.
    idempotency_key: Option<String>,
}

/// A session this host materialized for a spawn command's `idempotency_key`, so
/// a retry carrying the same key can be answered with it instead of creating a
/// second agent in the same worktree.
pub(crate) struct SpawnIdempotencyRecord {
    /// The session the first command with this key produced.
    session: SessionId,
    /// When it was materialized, in unix seconds — the window this record stays
    /// authoritative for is measured from here.
    materialized_at: u64,
}

/// The session an arriving spawn command should be *answered with* rather than
/// duplicated, judged purely on its `idempotency_key` and the clock.
///
/// `None` means go ahead and create: the command carried no key (an older CLI,
/// or `--allow-duplicate`), this host has never materialized one for that key, or
/// the record has aged past [`SPAWN_DEDUPE_WINDOW_SECS`].
///
/// [`SPAWN_DEDUPE_WINDOW_SECS`]: agentium_core::session_events::SPAWN_DEDUPE_WINDOW_SECS
///
/// The window is what keeps a *derived* key — a digest of host+cwd+backend+
/// title+prompt — from refusing the same task tomorrow: spawning "fix the
/// parser" in this worktree again next week is new work, not a duplicate. It is
/// still far longer than the retry loop this guards, where a caller mis-reads a
/// spawn's output and immediately re-runs it.
///
/// Pure over the map and `now` so the windowing is testable without a host; the
/// caller still checks that the named session actually still exists before
/// answering with it.
fn duplicate_spawn_target(
    seen: &HashMap<String, SpawnIdempotencyRecord>,
    idempotency_key: Option<&str>,
    now: u64,
) -> Option<SessionId> {
    let record = seen.get(idempotency_key?)?;
    // `saturating_sub` rather than a subtraction: a record stamped slightly in
    // the future (a clock step) reads as age 0, i.e. still inside the window,
    // which is the safe direction — it dedupes rather than duplicates.
    (now.saturating_sub(record.materialized_at) <= session_events::SPAWN_DEDUPE_WINDOW_SECS)
        .then_some(record.session)
}

/// A kind-31989 session command addressed to this host, decoded from its note by
/// [`decode_session_command`]. The caller ([`Dave::poll_session_command_events`])
/// still owns author-match, dedup, and the side-effecting dispatch.
enum SessionCommand {
    /// Create a fresh session locally (`command = "spawn_session"`).
    Spawn {
        command_id: String,
        request: SpawnRequest,
    },
    /// Reopen + revive + resume an existing session (`command = "resume_session"`),
    /// named by its kind-31988 d-tag.
    Resume {
        command_id: String,
        session_id: String,
    },
}

impl SessionCommand {
    /// The command's stable id (its `d` tag), used for dedup at the call site.
    fn command_id(&self) -> &str {
        match self {
            SessionCommand::Spawn { command_id, .. } => command_id,
            SessionCommand::Resume { command_id, .. } => command_id,
        }
    }
}

/// Decode a kind-31989 command note addressed to `local_hostname`.
///
/// Returns `None` when the note is malformed (no `d` tag), targets a different
/// host (`target_host != local_hostname`), is a resume command missing its
/// `session_id`, or is an unknown command — mirroring the filtering
/// [`Dave::poll_session_command_events`] applies inline. `default_backend` is
/// used when a spawn command omits (or mis-spells) its `backend` tag.
///
/// Pure over the note's tags so the command-parse + target-gate branch is
/// testable without an `AppContext`; the caller still owns author-match, dedup,
/// and the side-effecting dispatch.
fn decode_session_command(
    note: &nostrdb::Note,
    local_hostname: &str,
    default_backend: BackendType,
) -> Option<SessionCommand> {
    let command_id = session_events::get_tag_value(note, "d")?.to_string();
    let target = session_events::get_tag_value(note, "target_host").unwrap_or("");
    if target != local_hostname {
        return None;
    }

    match session_events::get_tag_value(note, "command").unwrap_or("") {
        "spawn_session" => {
            let cwd = session_events::get_tag_value(note, "cwd")
                .unwrap_or("")
                .to_string();
            let backend = session_events::get_tag_value(note, "backend")
                .and_then(BackendType::from_tag_str)
                .unwrap_or(default_backend);
            let spawn_id = session_events::get_tag_value(note, "spawn_id").map(|s| s.to_string());
            let custom_title = session_events::get_tag_value(note, "custom_title")
                .filter(|t| !t.is_empty())
                .map(|s| s.to_string());
            let prompt = session_events::get_tag_value(note, "prompt")
                .filter(|t| !t.is_empty())
                .map(|s| s.to_string());
            // Parsed strictly, via agentium-core's canonical vocabulary: an
            // unknown mode stays `None` so the session keeps the host's default.
            // `permission_mode_from_str` alone would map it to `Default`, quietly
            // downgrading a session the host would otherwise have started in Auto.
            let permission_mode = session_events::get_tag_value(note, "permission_mode")
                .filter(|t| !t.is_empty())
                .and_then(|raw| {
                    let parsed = agentium_core::permission_mode::parse_permission_mode(raw);
                    if parsed.is_none() {
                        tracing::warn!(
                            "spawn command {} names unknown permission mode '{}' — keeping the host default",
                            command_id,
                            raw,
                        );
                    }
                    parsed
                })
                .map(crate::session::permission_mode_from_str);
            let idempotency_key = session_events::get_tag_value(note, "idempotency_key")
                .filter(|k| !k.is_empty())
                .map(|s| s.to_string());
            Some(SessionCommand::Spawn {
                command_id,
                request: SpawnRequest {
                    cwd,
                    backend,
                    spawn_id,
                    custom_title,
                    prompt,
                    permission_mode,
                    idempotency_key,
                },
            })
        }
        "resume_session" => {
            let Some(session_id) = session_events::get_tag_value(note, "session_id") else {
                tracing::warn!("resume command {} missing session_id", command_id);
                return None;
            };
            Some(SessionCommand::Resume {
                command_id,
                session_id: session_id.to_string(),
            })
        }
        other => {
            tracing::debug!("ignoring unknown session command '{}'", other);
            None
        }
    }
}

impl Dave {
    /// Apply one spawn request: materialize the session it asks for, or — when it
    /// repeats a recent `idempotency_key` — answer it with the session an earlier
    /// request already produced.
    ///
    /// Returns the first `user` message to deliver paired with its session, or
    /// `None` when the request carried no prompt *or* was a duplicate — a
    /// duplicate's prompt is the same prompt, already delivered, and re-delivering
    /// it is the duplicated work this path exists to prevent.
    ///
    /// Split out of [`poll_session_command_events`](Self::poll_session_command_events)
    /// so applying the *same request twice* is testable without an `AppContext`,
    /// the way [`decode_session_command`] made the parse + target gate testable.
    /// Nothing here touches the app context: every read and write is Dave's own
    /// state. `now` (unix seconds) is passed in so the dedupe window can be
    /// exercised without sleeping.
    fn apply_spawn_command(
        &mut self,
        request: SpawnRequest,
        now: u64,
    ) -> Option<(SessionId, String)> {
        let SpawnRequest {
            cwd,
            backend,
            spawn_id,
            custom_title,
            prompt,
            permission_mode,
            idempotency_key,
        } = request;

        // Is this the same spawn asked for twice? A retry carries a fresh
        // `spawn_id` and a fresh command d-tag, so the key is the only thing that
        // can tell. The record is only honoured while the session it names still
        // exists — re-spawning after closing one is a genuine new request.
        let duplicate_of =
            duplicate_spawn_target(&self.spawn_idempotency, idempotency_key.as_deref(), now)
                .filter(|sid| self.session_manager.get(*sid).is_some());

        if let Some(existing) = duplicate_of {
            tracing::info!(
                "spawn repeats idempotency key {:?} — answering with existing session {} \
                 instead of creating a second",
                idempotency_key,
                existing,
            );
            // Answer the retry rather than ignoring it: re-stamp *its* spawn_id
            // onto the session we already have and republish, so the caller's
            // `--wait` resolves to this session's ref instead of timing out and
            // tempting yet another retry.
            if let Some(session) = self.session_manager.get_mut(existing) {
                if let Some(spawn_id) = spawn_id {
                    session.spawn_id = Some(spawn_id);
                }
                session.state_dirty = true;
            }
            return None;
        }

        let sid = update::create_session_with_cwd(
            &mut self.session_manager,
            &mut self.directory_picker,
            &mut self.scene,
            self.show_scene,
            self.ai_mode,
            PathBuf::from(cwd),
            &self.hostname,
            backend,
            Model::Default,
        );

        // Store spawn_id so it's echoed in kind-31988 state events, letting the
        // sender match this session to its placeholder. A supplied title lands in
        // `custom_title` (not `title`) so `display_title` shows it at once and
        // `update_title_from_last_message` — which only ever writes `title` —
        // can't clobber it as messages arrive.
        if let Some(session) = self.session_manager.get_mut(sid) {
            if let Some(spawn_id) = spawn_id {
                session.spawn_id = Some(spawn_id);
            }
            if let Some(title) = custom_title {
                session.details.custom_title = Some(title);
            }
            // Set the mode *here*, before the first message is queued below:
            // `dispatch` reads it to build the backend's options, and that
            // dispatch happens later in this same frame. Applying it afterwards —
            // via a `set_permission_mode` command, say — would land after the CLI
            // subprocess had already launched in the default mode.
            if let (Some(mode), Some(agentic)) = (permission_mode, session.agentic.as_mut()) {
                agentic.permission_mode = mode;
            }
        }

        // Remember which session this request produced, so a retry carrying the
        // same key lands in the branch above rather than materializing a sibling.
        if let Some(key) = idempotency_key {
            self.spawn_idempotency.insert(
                key,
                SpawnIdempotencyRecord {
                    session: sid,
                    materialized_at: now,
                },
            );
        }

        prompt.map(|prompt| (sid, prompt))
    }

    /// Poll for kind-31989 session command events.
    ///
    /// When a remote device wants to act on a session on this host, it publishes
    /// a kind-31989 event with `target_host` matching our hostname. We pick it up
    /// here: `spawn_session` creates a new session locally; `resume_session`
    /// reopens (and revives) an existing one via [`reopen_session`](Self::reopen_session).
    ///
    /// Returns the sessions a spawn command asked to start with a first message
    /// (its `prompt` tag): the message is already appended to each session's chat
    /// here, and the caller dispatches it to the backend (it holds the
    /// [`egui::Context`] that dispatch needs).
    pub(crate) fn poll_session_command_events(
        &mut self,
        ctx: &mut AppContext<'_>,
    ) -> Vec<SessionId> {
        let Some(sub) = self.session_command_sub else {
            return Vec::new();
        };
        let Some(account) = self.pns_local_state.as_ref().map(|state| state.account) else {
            return Vec::new();
        };

        let note_keys = ctx.ndb.poll_for_notes(sub, 16);
        if note_keys.is_empty() {
            return Vec::new();
        }

        // Sessions to reopen after the read txn closes — reopen_session needs its
        // own ndb access, so it can't run while `txn` borrows ctx.ndb.
        let mut to_reopen: Vec<String> = Vec::new();

        // First messages carried by spawn commands (their `prompt` tag), paired
        // with the freshly-created session. Appended to chat + dispatched after
        // the read txn closes, since both need `&AppContext`/`&mut self` free of
        // the txn's borrow on `ctx.ndb`.
        let mut first_prompts: Vec<(SessionId, String)> = Vec::new();

        {
            let txn = match Transaction::new(ctx.ndb) {
                Ok(t) => t,
                Err(_) => return Vec::new(),
            };

            for key in note_keys {
                let Ok(note) = ctx.ndb.get_note_by_key(&txn, key) else {
                    continue;
                };
                if *note.pubkey() != *account.bytes() {
                    continue;
                }

                let Some(command) =
                    decode_session_command(&note, &self.hostname, self.model_config.backend)
                else {
                    continue;
                };

                // Dedup: skip already-processed commands. Recorded here, beside
                // its own check, rather than once per match arm — every arm did
                // it first thing anyway, and the arms need `command_id` for
                // their own logging afterwards.
                if self.processed_commands.contains(command.command_id()) {
                    continue;
                }
                self.processed_commands
                    .insert(command.command_id().to_string());

                match command {
                    SessionCommand::Spawn {
                        command_id,
                        request,
                    } => {
                        tracing::info!(
                            "received spawn command {}: cwd={}, backend={:?}, spawn_id={:?}, title={:?}, prompt={}, mode={:?}, key={:?}",
                            command_id,
                            request.cwd,
                            request.backend,
                            request.spawn_id,
                            request.custom_title,
                            request.prompt.is_some(),
                            request.permission_mode,
                            request.idempotency_key,
                        );

                        // Defer any first message until the read txn closes:
                        // appending it builds a kind-1988 event (a fresh ndb read).
                        if let Some(delivery) =
                            self.apply_spawn_command(request, session_events::now_secs())
                        {
                            first_prompts.push(delivery);
                        }
                    }
                    SessionCommand::Resume {
                        command_id,
                        session_id,
                    } => {
                        tracing::info!(
                            "received resume command {}: session_id={}",
                            command_id,
                            session_id,
                        );
                        to_reopen.push(session_id);
                    }
                }
            }
        }

        for selector in to_reopen {
            self.reopen_session(ctx.ndb, account, &selector);
        }

        // Append each spawn command's first message to its new session's chat.
        // `add_user_message_for_session` builds the outbound kind-1988 event and
        // returns whether the backend should be dispatched now (true for a fresh,
        // idle local session). Actual dispatch is the caller's job — it holds the
        // `egui::Context`.
        let mut needs_dispatch = Vec::new();
        for (sid, prompt) in first_prompts {
            if self.add_user_message_for_session(sid, ctx, prompt, Vec::new()) {
                needs_dispatch.push(sid);
            }
        }
        needs_dispatch
    }

    /// Queue a spawn command request. The event is built and published in
    /// update() where AppContext (and thus the secret key) is available.
    /// Also creates a pending placeholder session so the user sees immediate feedback.
    pub(crate) fn queue_spawn_command(
        &mut self,
        target_host: &str,
        cwd: &Path,
        backend: BackendType,
    ) {
        let spawn_id = uuid::Uuid::new_v4().to_string();
        tracing::info!(
            "queuing spawn command {} for {} at {:?}",
            spawn_id,
            target_host,
            cwd
        );
        self.pending_spawn_commands.push(PendingSpawnCommand {
            target_host: target_host.to_string(),
            cwd: cwd.to_path_buf(),
            backend,
            spawn_id: spawn_id.clone(),
        });

        // Create a lightweight pending placeholder for immediate UI feedback. A
        // spawn has no existing session to correlate by, so `resume_target` is
        // None — the revived state is matched by the echoed `spawn_id`.
        self.session_manager.new_pending_placeholder(
            cwd.to_path_buf(),
            target_host.to_string(),
            backend,
            spawn_id,
            None,
        );
        self.active_overlay = DaveOverlay::None;
    }

    /// Queue a kind-31989 resume command for a soft-deleted session that lives on
    /// another host. Mirrors [`queue_spawn_command`](Self::queue_spawn_command)
    /// but carries the target session's identity (`claude_session_id`, its
    /// `agentium:` d-tag) and CLI session id, so the remote host reopens, revives,
    /// and resumes *that* session rather than spawning a fresh one.
    ///
    /// Reuses the spawn path's [`new_pending_placeholder`](crate::session::SessionManager::new_pending_placeholder)
    /// machinery — one pending-chip path, not two: the deleted chip gets an
    /// immediate "Connecting…" affordance instead of sitting unresponsive until
    /// the owning host answers. The placeholder is correlated by the target
    /// session's d-tag (`resume_target`), which the revived kind-31988 always
    /// carries, so [`pending_placeholder_for`](Self::pending_placeholder_for)
    /// upgrades it in place when the state streams back over the shared
    /// subscription (see the note there on why a spawn_id echo is not robust for
    /// resume).
    pub(crate) fn queue_resume_command(&mut self, state: &session_loader::SessionState) {
        let spawn_id = uuid::Uuid::new_v4().to_string();
        let backend = state
            .backend
            .as_deref()
            .and_then(BackendType::from_tag_str)
            .unwrap_or(BackendType::Claude);
        tracing::info!(
            "queuing resume command {} for session {} on host {}",
            spawn_id,
            state.claude_session_id,
            state.hostname,
        );
        self.pending_resume_commands.push(PendingResumeCommand {
            target_host: state.hostname.clone(),
            cwd: PathBuf::from(&state.cwd),
            backend,
            spawn_id: spawn_id.clone(),
            target_session_id: state.claude_session_id.clone(),
            cli_session_id: state.cli_session_id.clone().unwrap_or_default(),
        });

        // Immediate feedback via the same placeholder the spawn path uses, keyed
        // by the target session's d-tag so the revived kind-31988 upgrades it in
        // place regardless of which revision materializes first.
        self.session_manager.new_pending_placeholder(
            PathBuf::from(&state.cwd),
            state.hostname.clone(),
            backend,
            spawn_id,
            Some(state.claude_session_id.clone()),
        );
        self.active_overlay = DaveOverlay::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{test_config, test_dave, test_secret_key};
    use nostrdb::{IngestMetadata, Ndb, Transaction};
    use notedeck::DataPath;
    use tempfile::TempDir;

    /// The kind-31989 command-parse branch on the receiving host: a
    /// `resume_session` command addressed to us decodes to
    /// [`SessionCommand::Resume`] carrying the target session's id (from the
    /// `session_id` tag), a plain `spawn_session` decodes to
    /// [`SessionCommand::Spawn`], and a command for a *different* host is dropped
    /// by the target gate. This is the host dispatch the deleted-chip remote
    /// resume (and `agentium resume`) drive end-to-end — parsed here from a real
    /// ingested event rather than a hand-built note.
    #[tokio::test]
    async fn session_command_parse_respects_target_and_reads_session_id() {
        let sk = test_secret_key();
        let tmp = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();

        // A resume command targeting "host-a" naming session "sess-42", and a
        // plain spawn command for the same host.
        let resume = session_events::ResumeSpawn {
            target_session_id: "sess-42",
            cli_session_id: "cli-xyz",
        };
        let resume_cmd = session_events::build_spawn_command_event(
            "host-a",
            "/tmp/proj",
            "claude",
            &Default::default(),
            "spawn-1",
            Some(&resume),
            &sk,
        )
        .unwrap();
        let spawn_cmd = session_events::build_spawn_command_event(
            "host-a",
            "/work/dir",
            "claude",
            &session_events::SpawnOptions {
                title: Some("Wire the widget"),
                permission_mode: Some("plan"),
                idempotency_key: Some("key-abc"),
                ..Default::default()
            },
            "spawn-2",
            None,
            &sk,
        )
        .unwrap();
        // A spawn naming a mode this build doesn't know — e.g. from a newer peer.
        let bogus_mode_cmd = session_events::build_spawn_command_event(
            "host-a",
            "/work/dir",
            "claude",
            &session_events::SpawnOptions {
                permission_mode: Some("telepathy"),
                ..Default::default()
            },
            "spawn-3",
            None,
            &sk,
        )
        .unwrap();

        let filter = nostrdb::Filter::new().build();
        let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
        for ev in [&resume_cmd, &spawn_cmd, &bogus_mode_cmd] {
            ndb.process_event_with(&ev.to_event_json(), IngestMetadata::new().client(true))
                .unwrap();
        }
        // `wait_for_all_notes` accumulates to the full count; `wait_for_notes` can
        // return after the first note key, before both are queryable by id (a race
        // that made the `get_note_by_id` lookups below intermittently NotFound).
        ndb.wait_for_all_notes(sub, 3).await.unwrap();

        let txn = Transaction::new(&ndb).unwrap();
        let resume_note = ndb.get_note_by_id(&txn, &resume_cmd.note_id).unwrap();
        let spawn_note = ndb.get_note_by_id(&txn, &spawn_cmd.note_id).unwrap();
        let bogus_note = ndb.get_note_by_id(&txn, &bogus_mode_cmd.note_id).unwrap();

        // Resume command addressed to us decodes with the target session id.
        let Some(SessionCommand::Resume { session_id, .. }) =
            decode_session_command(&resume_note, "host-a", BackendType::Claude)
        else {
            panic!("expected a Resume command for this host");
        };
        assert_eq!(session_id, "sess-42", "reads the session_id tag");

        // Spawn command decodes with its cwd / backend / spawn_id / title / mode /
        // idempotency key.
        let Some(SessionCommand::Spawn { request, .. }) =
            decode_session_command(&spawn_note, "host-a", BackendType::Claude)
        else {
            panic!("expected a Spawn command for this host");
        };
        assert_eq!(request.cwd, "/work/dir");
        assert_eq!(request.backend, BackendType::Claude);
        assert_eq!(request.spawn_id.as_deref(), Some("spawn-2"));
        assert_eq!(request.custom_title.as_deref(), Some("Wire the widget"));
        assert_eq!(request.permission_mode, Some(PermissionMode::Plan));
        assert_eq!(
            request.idempotency_key.as_deref(),
            Some("key-abc"),
            "the request identity must survive decode, or nothing can dedupe on it",
        );

        // An unrecognized mode decodes to `None`, so the session is left on the
        // host's default instead of being silently downgraded to Default. That
        // command also carries no idempotency key — an older CLI's spawn — which
        // must decode as absent rather than as an empty-string key that every
        // other keyless spawn would then collide with.
        let Some(SessionCommand::Spawn { request, .. }) =
            decode_session_command(&bogus_note, "host-a", BackendType::Claude)
        else {
            panic!("expected a Spawn command for this host");
        };
        assert_eq!(request.permission_mode, None);
        assert_eq!(request.idempotency_key, None);

        // The target-host gate drops commands meant for another host.
        assert!(
            decode_session_command(&resume_note, "other-host", BackendType::Claude).is_none(),
            "a command for another host must not be processed here",
        );
    }

    /// A spawn request as the CLI would send it: the same `--title`/`--prompt` in
    /// the same worktree, differing only in the per-transmission `spawn_id` that
    /// every invocation mints fresh.
    fn retryable_spawn_request(spawn_id: &str, key: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            cwd: "/work/dir".to_string(),
            backend: BackendType::Claude,
            spawn_id: Some(spawn_id.to_string()),
            custom_title: Some("Fix the parser".to_string()),
            prompt: Some("read crates/foo and fix it".to_string()),
            permission_mode: None,
            idempotency_key: key.map(str::to_string),
        }
    }

    /// The bug, reproduced: apply the *same* spawn request twice — a caller that
    /// mis-read the first spawn's output and re-ran it — and assert exactly one
    /// session exists afterwards, with the prompt delivered exactly once.
    ///
    /// The second half is the control: with no idempotency key (an older CLI, or
    /// `--allow-duplicate`) the very same double-apply still produces two
    /// sessions, so this test is measuring the key and not some incidental
    /// property of `create_session_with_cwd`.
    #[test]
    fn applying_one_spawn_request_twice_creates_one_session() {
        const NOW: u64 = 1_800_000_000;

        let base_dir = TempDir::new().unwrap();
        let data_path = DataPath::new(base_dir.path());
        let mut dave = test_dave(&data_path);
        let before = dave.session_manager.iter().count();

        // First spawn: materializes a session and hands back its first message.
        let first = dave.apply_spawn_command(retryable_spawn_request("spawn-1", Some("k1")), NOW);
        let (created, prompt) = first.expect("a prompted spawn delivers its first message");
        assert_eq!(prompt, "read crates/foo and fix it");
        assert_eq!(dave.session_manager.iter().count(), before + 1);

        // Clear the flag a fresh session is born with, so the assertion below
        // measures what the *retry* did rather than what creation already did.
        dave.session_manager
            .get_mut(created)
            .expect("session materialized")
            .state_dirty = false;

        // The retry: same request, new spawn_id, seconds later.
        let second =
            dave.apply_spawn_command(retryable_spawn_request("spawn-2", Some("k1")), NOW + 3);

        assert_eq!(
            dave.session_manager.iter().count(),
            before + 1,
            "a retried spawn must not materialize a second agent in the same worktree",
        );
        assert!(
            second.is_none(),
            "the retry's prompt was already delivered to the existing session — \
             re-delivering it is the duplicated work being prevented",
        );

        // The retry is *answered*, not ignored: the session now carries the
        // retry's spawn_id and is dirty, so the republished kind-31988 state
        // resolves the retrying caller's `--wait` to this same session.
        let session = dave
            .session_manager
            .get(created)
            .expect("the first spawn's session is still the live one");
        assert_eq!(
            session.spawn_id.as_deref(),
            Some("spawn-2"),
            "the retry's spawn_id must be echoed back, or its --wait times out",
        );
        assert!(
            session.state_dirty,
            "dirty so the answer is actually published"
        );

        // A key whose session is gone is not a duplicate: closing a session and
        // spawning the same task again is a genuine new request, so the stale
        // record must not swallow it.
        assert!(dave.session_manager.delete_session(created));
        let after_delete = dave.session_manager.iter().count();
        let respawn =
            dave.apply_spawn_command(retryable_spawn_request("spawn-5", Some("k1")), NOW + 5);
        assert!(
            respawn.is_some(),
            "re-spawning after closing the session must deliver its prompt",
        );
        assert_eq!(
            dave.session_manager.iter().count(),
            after_delete + 1,
            "a record naming a session that no longer exists must not block a new spawn",
        );

        // Control: identical double-apply, no key, still duplicates.
        let keyless_before = dave.session_manager.iter().count();
        dave.apply_spawn_command(retryable_spawn_request("spawn-3", None), NOW);
        dave.apply_spawn_command(retryable_spawn_request("spawn-4", None), NOW + 3);
        assert_eq!(
            dave.session_manager.iter().count(),
            keyless_before + 2,
            "without a key there is nothing to dedupe on — this is the old behaviour, \
             and its presence here proves the assertions above are measuring the key",
        );
    }

    /// `duplicate_spawn_target` decides, from the key alone, whether an arriving
    /// spawn is a retry to be answered or a new request to be created. Each case
    /// here is a distinct way the host could get that wrong — and creating when
    /// it should have answered is the duplicate-agent bug.
    #[test]
    fn duplicate_spawn_target_answers_only_a_recent_matching_key() {
        const NOW: u64 = 1_800_000_000;
        let window = session_events::SPAWN_DEDUPE_WINDOW_SECS;

        let mut seen = HashMap::new();
        seen.insert(
            "key-abc".to_string(),
            SpawnIdempotencyRecord {
                session: 7,
                materialized_at: NOW - 5,
            },
        );

        // The case the whole mechanism exists for: a retry seconds later.
        assert_eq!(
            duplicate_spawn_target(&seen, Some("key-abc"), NOW),
            Some(7),
            "a retry inside the window must be answered with the existing session",
        );

        // A keyless spawn (older CLI, or an explicit opt-out) never dedupes —
        // there is nothing to match it on, so it must always create.
        assert_eq!(duplicate_spawn_target(&seen, None, NOW), None);

        // A different request is a different key, and must not be swallowed.
        assert_eq!(duplicate_spawn_target(&seen, Some("key-xyz"), NOW), None);

        // The boundary: still inside at exactly the window, outside one second
        // later. Past it, the same derived key is the same *task* asked for
        // again later, which is new work.
        assert_eq!(
            duplicate_spawn_target(&seen, Some("key-abc"), NOW - 5 + window),
            Some(7),
        );
        assert_eq!(
            duplicate_spawn_target(&seen, Some("key-abc"), NOW - 5 + window + 1),
            None,
        );

        // A record stamped in the future (a clock step between the two spawns)
        // must read as still-live rather than wrapping into a huge age and
        // silently duplicating.
        let mut future = HashMap::new();
        future.insert(
            "key-abc".to_string(),
            SpawnIdempotencyRecord {
                session: 7,
                materialized_at: NOW + 3600,
            },
        );
        assert_eq!(
            duplicate_spawn_target(&future, Some("key-abc"), NOW),
            Some(7)
        );
    }
}
