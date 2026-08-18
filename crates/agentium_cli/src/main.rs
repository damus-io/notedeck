//! `agentium` — a CLI to query and control Dave agentic sessions over a running
//! notedeck's embedded relay.
//!
//! A sibling to [`headway_cli`] and [`notebook_cli`], but with the data layer
//! inverted: rather than folding events itself, this CLI drives the
//! platform-neutral [`agentium_core`] engine, which owns its own nostrdb cache
//! and a background relay connect/sync loop that streams this identity's
//! PNS-encrypted session corpus into that cache. `nostrdb_net`'s `relay::sync`
//! module still owns the incidental plumbing — the stored signing key, the cache
//! directory convention, and `login`/`logout`. This file is the command surface:
//! argument parsing, config resolution, and rendering the session list.

use std::env;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_loader::SessionState;
use enostr::Pubkey;
use nostrdb::Transaction;

use nostrdb_net::relay::sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/agentium-cli` on Linux).
const APP: &str = "agentium-cli";

/// Hard cap on the settle wait, so a reachable-but-silent relay can't stall the
/// read: past this we give up on the reconcile and read whatever the cache holds.
const SYNC_MAX: Duration = Duration::from_secs(6);

/// Bound on the post-publish flush (see [`cmd_resume`]). The publish rides the
/// engine loop's FIFO, so this only needs to outlast the loop draining that one
/// command — the initial [`SYNC_MAX`] reconcile already settled the backfill.
const PUBLISH_FLUSH: Duration = Duration::from_secs(2);

/// Bound on `spawn --wait` (see [`cmd_spawn`]): how long to wait for the target
/// host to answer a spawn command with the new session's kind-31988 state. A
/// host that never answers (none running on `target_host`, or one stuck
/// `pending`) can't hang exit past this — the spawn command was still published,
/// so a later `list` finds the session if the host was merely slow.
const SPAWN_WAIT: Duration = Duration::from_secs(8);

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`agentium list | head`) instead of
    // panicking in println! on EPIPE.
    nostrdb_net::relay::sync::reset_sigpipe();
    // Select the rustls CryptoProvider before any wss:// relay handshake; the
    // standalone CLIs never run notedeck's startup init that does this.
    enostr::install_crypto();
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// A parsed command. Session arguments are still raw strings here; they're
/// resolved against the engine's session list once it's synced.
enum Command {
    /// Enumerate this identity's sessions.
    List,
    /// Print git-show-style detail for one resolved session: its kind-31988
    /// state, the run-configs on its host+cwd, its latest usage, and a
    /// conversation summary. The selector is optional — it defaults to
    /// `$AGENTIUM_SESSION` so a running Dave session can just type
    /// `agentium show`.
    Show {
        session: Option<String>,
    },
    /// Print one session's kind-1988 conversation, one entry per message, in the
    /// order [`load_session_messages_for_author`] returns (millisecond
    /// wall-clock via `EventOrder` — *not* the `seq` tag). Named `log` after
    /// `git log`. Like `show`, the selector is optional and defaults to
    /// `$AGENTIUM_SESSION`.
    ///
    /// [`load_session_messages_for_author`]: agentium_core::session_loader::load_session_messages_for_author
    Log {
        session: Option<String>,
        view: MessageView,
    },
    /// Reopen a closed (possibly soft-deleted) session on its host so a new
    /// message drives its backend again. The argument is any session selector
    /// `list` accepts (a d-tag, cli-session id, or `agentium:` word-id).
    Resume {
        session: String,
    },
    /// Send a `user` message to a session's conversation so its running agent
    /// (local or remote) picks it up over relay sync. The selector is required
    /// (unlike `show`/`log`, it can't default to `$AGENTIUM_SESSION` — the first
    /// positional would be ambiguous against the message text), and the message
    /// is the remaining positionals joined with spaces.
    Send {
        session: String,
        text: String,
    },
    /// Publish a kind-31989 spawn command telling a (local or remote) Dave host
    /// to create a fresh session, then optionally block until the host answers
    /// with its kind-31988 state and print the new session's durable `agentium:`
    /// ref. `host`/`cwd`/`backend` default to the *current* session's own state
    /// (`$AGENTIUM_SESSION`) so a bare `agentium spawn` starts a sibling in the
    /// same worktree on the same host. `--title` gives the session an explicit,
    /// sticky title; `--prompt` (which implies `--wait`) delivers a first `user`
    /// message once the session exists.
    Spawn {
        host: Option<String>,
        cwd: Option<String>,
        backend: Option<String>,
        title: Option<String>,
        prompt: Option<String>,
        wait: bool,
    },
    /// Abort a session's in-flight turn on its host — the CLI companion to
    /// pressing Esc in Dave. The selector is required (a specific live session to
    /// interrupt); a tombstoned session has no running backend and is rejected.
    Interrupt {
        session: String,
    },
    Login {
        nsec: String,
    },
    Logout,
}

async fn run() -> Result<()> {
    let cli = match Cli::parse(env::args().skip(1))? {
        Some(cli) => cli,
        None => {
            print_usage();
            return Ok(());
        }
    };

    // `login`/`logout` manage the stored key and touch neither the cache nor a
    // relay, so handle them before any of that machinery spins up.
    match &cli.command {
        Command::Login { nsec } => return nostrdb_net::relay::sync::login(nsec, APP),
        Command::Logout => return nostrdb_net::relay::sync::logout(APP),
        _ => {}
    }

    // Unlike a notebook canvas, agentium's sessions are PNS-encrypted to this
    // identity, so a signing key is required even to *read*: it is both the
    // engine's ndb decryption key and its publish identity. There is no
    // read-only `--author someone-else` path.
    let (secret, self_pk) = cli
        .secret
        .ok_or("need a signing key — run `agentium login <nsec>` (or pass --nsec)")?;

    // Resolve the relay and cache dir, persisting either when it was passed
    // explicitly so later runs reuse it without the flag.
    let relay = resolve_relay(cli.relay)?;
    let db = resolve_db(cli.db)?;

    // The engine owns its own nostrdb cache and a self-driving relay loop. Open
    // it over the cache dir nostrdb_net's relay::sync manages so co-located
    // tools share one cache; the engine takes a clone and drives sync itself.
    // Opening also registers the device key with ndb so its ingest threads can
    // decrypt this identity's inbound kind-1080 PNS envelopes into queryable
    // inner events.
    let ndb = nostrdb_net::relay::sync::open_ndb(db.as_deref(), APP)?;
    let mut engine = Engine::with_ndb(ndb, secret)?;

    // Connect installs the PNS discovery subscription on the engine's `Session` —
    // kind-1080 events authored by this identity's derived PNS pubkey — which
    // streams the whole encrypted session corpus into the cache (where ndb
    // decrypts it) and points publishes at the relay. Best-effort: an unreachable
    // relay just leaves us reading whatever the cache already holds. A `--author`
    // pointing at someone else still can't decrypt *their* private sessions —
    // only they hold that key.
    engine.connect(&relay)?;

    // Let the initial reconcile finish before we read. `wait_for_sync` resolves
    // deterministically once the PNS history backfill has settled — i.e. every
    // reconciled session-state event is queryable — so a single read afterward
    // sees the whole synced batch, not a race with events still streaming in.
    // Bounded by SYNC_MAX so a reachable-but-silent relay can't stall the read;
    // an empty or unreachable relay settles (or times out) fast and we fall
    // through to whatever the cache already holds.
    let _ = tokio::time::timeout(SYNC_MAX, engine.wait_for_sync()).await;

    // `--author` overrides whose sessions we read; it defaults to the signer.
    let read_pk = cli.author.unwrap_or(self_pk);

    let filters = ListFilters {
        host: cli.host,
        status: cli.status,
        cwd: cli.cwd,
        backend: cli.backend,
    };

    match cli.command {
        Command::List => cmd_list(&engine, &read_pk, &filters, cli.list_scope, cli.json)?,
        Command::Show { session } => cmd_show(&engine, &read_pk, session.as_deref(), cli.json)?,
        Command::Log { session, view } if view.follow => {
            cmd_follow(&engine, &read_pk, session.as_deref(), &view, cli.json).await?
        }
        Command::Log { session, view } => {
            cmd_log(&engine, &read_pk, session.as_deref(), &view, cli.json)?
        }
        Command::Resume { session } => cmd_resume(&engine, &read_pk, &session).await?,
        Command::Send { session, text } => {
            cmd_send(&engine, &read_pk, &session, &text, cli.json).await?
        }
        Command::Spawn {
            host,
            cwd,
            backend,
            title,
            prompt,
            wait,
        } => {
            let opts = SpawnOpts {
                host,
                cwd,
                backend,
                title,
                prompt,
                wait,
            };
            cmd_spawn(&engine, &read_pk, &opts, cli.json).await?
        }
        Command::Interrupt { session } => cmd_interrupt(&engine, &read_pk, &session).await?,
        Command::Login { .. } | Command::Logout => unreachable!("handled above"),
    }

    Ok(())
}

/// `agentium resume <session>` — reopen a closed session's backend.
///
/// Resolves the selector across the live *and* tombstoned sets (so a durable
/// `agentium:` ref still resolves after the session was soft-deleted), then
/// publishes a kind-31989 `resume_session` command targeting the session's host.
/// The host reopens the session — reviving its `agentium:` ref, rehydrating its
/// history, and resuming the CLI backend with `claude --resume`.
///
/// Errors early (before publishing) when nothing matches, or when the resolved
/// session has no CLI session id to resume — i.e. its backend never started, so
/// there is nothing for `--resume` to reconstruct.
async fn cmd_resume(engine: &Engine, author: &Pubkey, selector: &str) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author,
        resolve_session_including_deleted,
    };

    // Resolve to the fields the resume command needs, then drop the borrow of the
    // loaded state vectors before we publish.
    let (target_host, cwd, backend, target_sid, cli_sid, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
        let state = resolve_session_including_deleted(&live, &deleted, selector)?;

        let cli = match state.cli_session_id.as_deref() {
            Some(cli) if !cli.is_empty() => cli.to_string(),
            _ => {
                return Err(format!(
                    "{} has no CLI session to resume — its backend never started",
                    state.agentium_uri()
                )
                .into());
            }
        };
        (
            state.hostname.clone(),
            state.cwd.clone(),
            state
                .backend
                .clone()
                .unwrap_or_else(|| "claude".to_string()),
            state.claude_session_id.clone(),
            cli,
            state.agentium_uri(),
        )
    };

    if target_host.is_empty() {
        return Err(format!("{uri} has no recorded host; cannot target a resume").into());
    }

    engine.resume_session(&target_host, &cwd, &backend, &target_sid, &cli_sid)?;

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;

    println!("resume command sent to {target_host} for {uri}");
    Ok(())
}

/// `agentium send <session> <text>` — publish a `user` message to a session.
///
/// Resolves the selector against the **live** session set only, builds a
/// kind-1988 `user` event threaded onto the session's existing conversation, and
/// publishes it through the engine's [`Session`] so the session's running agent
/// (local *or* remote) picks it up over relay sync. Reports the resulting event id.
///
/// Live-only on purpose: a tombstoned (soft-deleted) session has no backend
/// reading its conversation, so a message would just root a stray thread nobody
/// consumes. When the selector matches only a deleted session we redirect to
/// `resume` (the tool for reviving one) rather than send into the void; any
/// other miss surfaces the resolver's own "no session matching" error, so a typo
/// fails loudly instead of silently starting a fresh thread.
///
/// The send runs *after* [`run`]'s bounded sync-settle, so
/// [`Engine::send_message`] threads onto the conversation's real last event (the
/// reconcile has already pulled it) instead of starting a new thread. The
/// post-publish flush mirrors [`cmd_resume`].
async fn cmd_send(
    engine: &Engine,
    author: &Pubkey,
    selector: &str,
    text: &str,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author, resolve_session,
    };

    // Resolve to the session id + URI against the live set, dropping the borrow
    // of the loaded state vectors before we publish. A miss that turns out to be
    // a tombstoned session is redirected to `resume`; any other miss propagates
    // the resolver's error.
    let (session_id, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let state = match resolve_session(&live, selector) {
            Ok(state) => state,
            Err(live_err) => {
                let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
                if let Ok(gone) = resolve_session(&deleted, selector) {
                    return Err(format!(
                        "{} is deleted — reopen it with `agentium resume {selector}` before sending",
                        gone.agentium_uri()
                    )
                    .into());
                }
                return Err(live_err.into());
            }
        };
        (state.claude_session_id.clone(), state.agentium_uri())
    };

    // Build + ingest + publish the kind-1988 `user` message; the returned event
    // carries the durable note id we report.
    let built = engine.send_message(&session_id, text)?;
    let event_id = hex::encode(built.note_id);

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;

    if as_json {
        let obj = serde_json::json!({ "session": uri, "event_id": event_id });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    // A short id prefix reads cleanly at a glance; the full hex is in `--json`.
    let short = &event_id[..event_id.len().min(8)];
    println!("sent to {uri} (event {short}…)");
    Ok(())
}

/// The resolved flags for `agentium spawn`. `host`/`cwd`/`backend` are still the
/// *raw* flags here (each `None` when omitted); [`resolve_spawn_target`] fills the
/// gaps from the current session before the command is built.
struct SpawnOpts {
    host: Option<String>,
    cwd: Option<String>,
    backend: Option<String>,
    /// `--title`: an explicit, sticky session title (rides the command as a
    /// `custom_title` tag). `None` lets the host derive one from the first message.
    title: Option<String>,
    /// `--prompt`: a first `user` message to deliver once the session exists.
    /// Implies `--wait` (you can't send to a session that isn't up yet).
    prompt: Option<String>,
    /// `--wait`: block (bounded by [`SPAWN_WAIT`]) until the host answers with the
    /// new session's kind-31988 state, then print its durable `agentium:` ref.
    wait: bool,
}

impl SpawnOpts {
    /// Whether the spawn should wait for the host's answer: the explicit `--wait`,
    /// or implicitly whenever `--prompt` is set (a first message can't be
    /// delivered to a session that isn't up yet).
    fn effective_wait(&self) -> bool {
        self.wait || self.prompt.is_some()
    }
}

/// The fully-resolved spawn target: which host to create the session on, the cwd
/// it runs in, and the backend to launch. Every field is concrete (the raw
/// [`SpawnOpts`] `Option`s have been defaulted).
struct SpawnTarget {
    host: String,
    cwd: String,
    backend: String,
}

/// Resolve the spawn target, defaulting each omitted flag to the *current*
/// session's own kind-31988 state (`$AGENTIUM_SESSION`) so a bare `agentium
/// spawn` starts a sibling in the same worktree on the same host. `--backend`
/// falls back to `"claude"` when neither a flag nor a current-session backend is
/// available. Errors when `host`/`cwd` can't be determined (not inside a session
/// and no flag) — there is nothing to target.
fn resolve_spawn_target(engine: &Engine, author: &Pubkey, opts: &SpawnOpts) -> Result<SpawnTarget> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author,
        resolve_session_including_deleted,
    };

    // The current session's state, if we're running inside one — resolved the way
    // `cmd_show` does with no selector (the `$AGENTIUM_SESSION` ref). Its host/cwd/
    // backend seed the defaults. Absent (not in a session) just means every field
    // must come from a flag.
    let current = std::env::var("AGENTIUM_SESSION")
        .ok()
        .filter(|s| !s.is_empty());
    let (self_host, self_cwd, self_backend) = match current {
        Some(selector) => {
            let txn = Transaction::new(engine.ndb())?;
            let live = load_session_states_for_author(engine.ndb(), &txn, author);
            let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
            match resolve_session_including_deleted(&live, &deleted, &selector) {
                Ok(state) => (
                    Some(state.hostname.clone()),
                    Some(state.cwd.clone()),
                    state.backend.clone(),
                ),
                // A stale/unknown $AGENTIUM_SESSION isn't fatal — fall back to flags.
                Err(_) => (None, None, None),
            }
        }
        None => (None, None, None),
    };

    merge_spawn_target(
        opts,
        self_host.as_deref(),
        self_cwd.as_deref(),
        self_backend.as_deref(),
    )
}

/// Merge the raw `--host`/`--cwd`/`--backend` flags with the current session's
/// values (each `cur_*` is `Some` only when we're running inside a session that
/// resolved). A flag wins; otherwise the current-session value fills the gap.
/// `backend` further falls back to `"claude"` when neither is available. Errors
/// when `host`/`cwd` end up empty — there's nothing to target. Pure over its
/// inputs so the defaulting is unit-testable without an ndb.
fn merge_spawn_target(
    opts: &SpawnOpts,
    cur_host: Option<&str>,
    cur_cwd: Option<&str>,
    cur_backend: Option<&str>,
) -> Result<SpawnTarget> {
    let pick = |flag: &Option<String>, cur: Option<&str>| -> Option<String> {
        flag.clone()
            .or_else(|| cur.map(str::to_string))
            .filter(|v| !v.is_empty())
    };

    let host = pick(&opts.host, cur_host).ok_or(
        "no host — pass --host (or run inside a session so $AGENTIUM_SESSION supplies it)",
    )?;
    let cwd = pick(&opts.cwd, cur_cwd)
        .ok_or("no cwd — pass --cwd (or run inside a session so $AGENTIUM_SESSION supplies it)")?;
    let backend = pick(&opts.backend, cur_backend).unwrap_or_else(|| "claude".to_string());

    Ok(SpawnTarget { host, cwd, backend })
}

/// `agentium spawn` — tell a (local or remote) Dave host to create a fresh
/// session, then optionally wait for it and hand it a first prompt.
///
/// Publishes a kind-31989 `spawn_session` command
/// ([`Engine::spawn_session`](agentium_core::Engine::spawn_session)) targeting
/// the resolved host+cwd, carrying `--title` as a `custom_title` override. The
/// host materializes the session and publishes back a kind-31988 state echoing
/// the `spawn_id` we got here.
///
/// Without `--wait` we only know the `spawn_id` (the session doesn't exist yet),
/// so we report just that. With `--wait` (also implied by `--prompt`) we install
/// [`Engine::watch_sessions`](agentium_core::Engine::watch_sessions) *before*
/// publishing — so a fast host answer can't slip through the gap — then re-read
/// the session list on each wake until one carries our `spawn_id`, bounded by
/// [`SPAWN_WAIT`]. That row is the new session; its `agentium_uri` is the ref we
/// print. `--prompt` then delivers a first `user` message via the same send path
/// as [`cmd_send`].
async fn cmd_spawn(
    engine: &Engine,
    author: &Pubkey,
    opts: &SpawnOpts,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::load_session_states_for_author;

    let target = resolve_spawn_target(engine, author, opts)?;

    // `--prompt` can only reach a session that exists, so it implies `--wait`.
    let wait = opts.effective_wait();

    // Install the session-list watch *before* publishing so the host's kind-31988
    // answer can't land between the publish and our first read (only when waiting).
    let mut watch = if wait {
        Some(engine.watch_sessions()?)
    } else {
        None
    };

    let spawn_id = engine.spawn_session(
        &target.host,
        &target.cwd,
        &target.backend,
        opts.title.as_deref(),
    )?;

    // Flush the publish (bounded) so an unreachable relay can't stall exit; the
    // command is ingested locally regardless.
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;

    // No `--wait`: only the spawn_id is known — report it and return.
    let Some(watch) = watch.as_mut() else {
        emit_spawn(&target.host, &spawn_id, None, None, as_json)?;
        return Ok(());
    };

    // Wait (bounded) for a kind-31988 state carrying our spawn_id. `check, then
    // wait`: re-read the list, match the spawn_id, else block on the watch.
    let resolved = tokio::time::timeout(SPAWN_WAIT, async {
        loop {
            {
                let txn = Transaction::new(engine.ndb())?;
                let live = load_session_states_for_author(engine.ndb(), &txn, author);
                if let Some(state) = live
                    .iter()
                    .find(|s| s.spawn_id.as_deref() == Some(&spawn_id))
                {
                    return Ok::<Option<SessionState>, Box<dyn std::error::Error>>(Some(
                        state.clone(),
                    ));
                }
            }
            // Watch ending (db torn down) resolves the wait with nothing found.
            if !watch.changed().await {
                return Ok(None);
            }
        }
    })
    .await;

    let state = match resolved {
        Ok(Ok(Some(state))) => state,
        // The read/loop itself errored — surface it.
        Ok(Err(e)) => return Err(e),
        // Timed out, or the watch ended before an answer arrived.
        Ok(Ok(None)) | Err(_) => {
            let short = short_id(&spawn_id);
            return Err(format!(
                "no host answered on {} within {}s (spawn {short}…) — the command was \
                 published, so `agentium list` will find the session if the host was just slow",
                target.host,
                SPAWN_WAIT.as_secs(),
            )
            .into());
        }
    };

    let uri = state.agentium_uri();

    // `--prompt`: deliver the first user message now that the session is up,
    // reusing the send path. Report its event id alongside the spawn.
    let event_id = match &opts.prompt {
        Some(prompt) => {
            let built = engine.send_message(&state.claude_session_id, prompt)?;
            let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;
            Some(hex::encode(built.note_id))
        }
        None => None,
    };

    emit_spawn(
        &target.host,
        &spawn_id,
        Some(&uri),
        event_id.as_deref(),
        as_json,
    )?;
    Ok(())
}

/// A short (8-char) prefix of an id, for the scannable one-line spawn report.
fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The `spawn --json` object: `{ spawn_id, host, session, event_id }`. `session`
/// is `null` until `--wait` resolves the new session's `agentium:` ref;
/// `event_id` is present only when `--prompt` sent a first message. Built here
/// (rather than inline in [`emit_spawn`]) so the shape is unit-testable.
fn spawn_json(
    host: &str,
    spawn_id: &str,
    session: Option<&str>,
    event_id: Option<&str>,
) -> serde_json::Value {
    let mut obj = serde_json::json!({ "spawn_id": spawn_id, "host": host, "session": session });
    if let Some(event_id) = event_id {
        obj["event_id"] = serde_json::Value::String(event_id.to_string());
    }
    obj
}

/// Report a spawn's result. Plain text mirrors [`cmd_send`]'s style; `--json`
/// emits `{ spawn_id, host, session, event_id }` — `session` is `null` until
/// `--wait` resolves it, and `event_id` is present only when `--prompt` sent a
/// first message. `session` is the sayable `agentium:` ref, `event_id` the hex
/// note id.
fn emit_spawn(
    host: &str,
    spawn_id: &str,
    session: Option<&str>,
    event_id: Option<&str>,
    as_json: bool,
) -> Result<()> {
    if as_json {
        let obj = spawn_json(host, spawn_id, session, event_id);
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    let short = short_id(spawn_id);
    match session {
        None => println!("spawn command sent to {host} (spawn {short}…)"),
        Some(uri) => {
            let mut line = format!("spawned {uri} on {host} (spawn {short}…)");
            if let Some(event_id) = event_id {
                let ev = short_id(event_id);
                line.push_str(&format!(", sent prompt (event {ev}…)"));
            }
            println!("{line}");
        }
    }
    Ok(())
}

/// `agentium interrupt <session>` — abort a session's in-flight turn.
///
/// The CLI companion to pressing Esc in Dave: publishes a kind-1988 interrupt
/// command that the session's host applies by aborting the current turn/tool
/// loop. Resolves the selector against the **live** set — a tombstoned session
/// has no running backend to interrupt, so a match against only the deleted set
/// is reported as such (reopen it with `resume` first); any other miss surfaces
/// the resolver's own "no session matching" error so a typo fails loudly.
///
/// Mirrors [`cmd_send`]'s resolve → engine-verb → bounded post-publish flush,
/// minus the message body and reported event id — an interrupt is fire-and-forget.
async fn cmd_interrupt(engine: &Engine, author: &Pubkey, selector: &str) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author, resolve_session,
    };

    let (session_id, uri) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let state = match resolve_session(&live, selector) {
            Ok(state) => state,
            Err(live_err) => {
                let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
                if let Ok(gone) = resolve_session(&deleted, selector) {
                    return Err(format!(
                        "{} is deleted — nothing is running to interrupt",
                        gone.agentium_uri()
                    )
                    .into());
                }
                return Err(live_err.into());
            }
        };
        (state.claude_session_id.clone(), state.agentium_uri())
    };

    engine.interrupt_session(&session_id)?;

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;

    println!("interrupt sent to {uri}");
    Ok(())
}

/// `agentium show <session>` — git-show-style detail for one resolved session.
///
/// Resolves the selector across the live *and* tombstoned sets (so a durable
/// `agentium:` ref still describes a soft-deleted session), then renders: the
/// session's `agentium:` URI + status, every kind-31988 state field, the
/// run-configs registered on its host+cwd, its latest usage snapshot (from the
/// kind-1989 archive, when present), and a conversation summary (message count
/// plus any pending permission request). With `as_json`, the same detail is a
/// single structured object.
///
/// The `subagent` rollup the card envisions is deferred: subagent lifecycle is
/// tracked live by a stateful stack in `notedeck_dave` (there is no batch
/// JSONL→subagent parser), so it needs its own card rather than a half-build here.
fn cmd_show(engine: &Engine, author: &Pubkey, selector: Option<&str>, as_json: bool) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_messages_for_author,
        load_session_states_for_author, resolve_session_including_deleted,
    };
    use agentium_core::session_reconstructor::latest_session_usage;

    let selector = selector
        .ok_or("no session — pass a selector (see `agentium list`) or set $AGENTIUM_SESSION")?;

    let txn = Transaction::new(engine.ndb())?;
    let live = load_session_states_for_author(engine.ndb(), &txn, author);
    let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
    let state = resolve_session_including_deleted(&live, &deleted, selector)?;

    // Run-configs are keyed by (hostname, cwd); the session's own host+cwd pick
    // the configs that would run *in it* — not this machine's.
    let run_configs = matching_run_configs(engine.ndb(), &txn, author, state);

    // Usage rides the lossless kind-1989 archive; the conversation summary folds
    // the kind-1988 message stream. Both read through the `txn` already open
    // above — calling `engine.session_messages` here instead would open a second
    // read transaction on this thread, which nostrdb refuses (one reader slot per
    // thread), silently yielding an empty conversation.
    let usage = latest_session_usage(engine.ndb(), &txn, &state.claude_session_id);
    let messages =
        load_session_messages_for_author(engine.ndb(), &txn, author, &state.claude_session_id)
            .messages;
    let summary = ConversationSummary::from_messages(&messages);

    if as_json {
        let detail = SessionDetailJson {
            session: SessionJson::new(state),
            run_configs: &run_configs,
            usage: usage.as_ref().map(UsageJson::from),
            conversation: ConversationJson::from(&summary),
        };
        println!("{}", serde_json::to_string_pretty(&detail)?);
        return Ok(());
    }

    let color = std::io::stdout().is_terminal();
    print!(
        "{}",
        render_detail(
            state,
            &run_configs,
            usage.as_ref(),
            &summary,
            now_secs(),
            color
        )
    );
    Ok(())
}

/// `agentium log <session>` — print one session's kind-1988 conversation,
/// one entry per message, in order.
///
/// Resolves the selector across the live *and* tombstoned sets (so a deleted
/// session's transcript still reads), then loads its messages with
/// [`load_session_messages_for_author`] — which already orders them by
/// [`EventOrder`] (millisecond wall-clock) and applies the single shared
/// note→message mapping ([`render_conversation_note`]). We render in the order
/// returned; we do **not** re-sort by `seq` (that axis was refactored out of the
/// display order) or reimplement the mapping.
///
/// `--role`/`--last`/`--tools` filter the rendered stream (see [`MessageView`]);
/// `--json` emits each message's structured [`Message::to_json`] view; `--jsonl`
/// short-circuits to the reconstructed claude-code JSONL (a different,
/// `seq`-ordered axis — see below). The whole thing is built into one string and
/// handed to [`emit`], which routes it through a pager (like `git log`) when
/// appropriate.
///
/// [`Message::to_json`]: agentium_core::messages::Message::to_json
///
/// [`load_session_messages_for_author`]: agentium_core::session_loader::load_session_messages_for_author
/// [`EventOrder`]: agentium_core::session_loader::EventOrder
/// [`render_conversation_note`]: agentium_core::session_loader::render_conversation_note
fn cmd_log(
    engine: &Engine,
    author: &Pubkey,
    selector: Option<&str>,
    view: &MessageView,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_messages_for_author,
        load_session_states_for_author, resolve_session_including_deleted,
    };
    use agentium_core::session_reconstructor::reconstruct_jsonl_lines;

    let selector = selector
        .ok_or("no session — pass a selector (see `agentium list`) or set $AGENTIUM_SESSION")?;

    let txn = Transaction::new(engine.ndb())?;
    let live = load_session_states_for_author(engine.ndb(), &txn, author);
    let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
    let state = resolve_session_including_deleted(&live, &deleted, selector)?;

    // Resolve pager/color up front (both are output concerns). `auto` color
    // follows the effective sink: a real tty *or* a color-aware pager — so the
    // default built-in pager (`less -R`) still shows color, and piping raw needs
    // `--color always` to keep it. Bounded by the pager decision so a
    // non-terminal run (a pipe) is plain and unpaged.
    let stdout_tty = std::io::stdout().is_terminal();
    let use_pager = view.pager.enabled(stdout_tty);
    let color = view.color.enabled(stdout_tty || use_pager);

    let output = if view.jsonl {
        // `--jsonl`: emit the reconstructed claude-code JSONL from the lossless
        // kind-1989 archive, raw. This is a *different* ordering axis than the
        // display stream — `reconstruct_jsonl_lines` sorts by `seq` to reproduce
        // the original claude-code line order of the source archive, which is
        // correct here. The function is not author-scoped, but the engine's cache
        // only holds this identity's own PNS-decrypted events (the device key
        // registered at `open_ndb` is the only one whose kind-1080 envelopes ndb
        // can decrypt), so it only ever sees our own kind-1989 events. Display
        // filters and color don't apply.
        let lines = reconstruct_jsonl_lines(engine.ndb(), &txn, &state.claude_session_id)
            .map_err(|e| e.to_string())?;
        join_lines(lines)
    } else {
        // The loader already returns messages in `EventOrder` and applies the
        // shared mapping; `MessageView` only slices/hides, never re-sorts.
        let messages =
            load_session_messages_for_author(engine.ndb(), &txn, author, &state.claude_session_id)
                .messages;
        let selected = view.select(&messages);

        if as_json {
            let rows: Vec<serde_json::Value> = selected.iter().map(|m| m.to_json()).collect();
            let mut json = serde_json::to_string_pretty(&rows)?;
            json.push('\n');
            json
        } else {
            render_messages(&selected, color)
        }
    };

    emit(&output, use_pager)
}

/// `agentium log <session> --follow` — print the current tail, then keep
/// following, appending each new message as it lands until Ctrl-C. The reading
/// *mode* of [`cmd_log`], not a separate command: same selector resolution, same
/// [`EventOrder`]-ordered loader, same shared note→message mapping.
///
/// The loop is the engine's documented *wait, then re-read the snapshot*: an ndb
/// subscription ([`Engine::watch_session`]) wakes on each new kind-1988 event and
/// [`Engine::watch_sessions`] on each kind-31988 state revision; on either wake
/// we re-read the whole ordered conversation and print only the suffix past the
/// highest order already shown (via [`first_after`]) — never a message count, so
/// a slightly-out-of-order live insert can't reprint or misorder. A status
/// change (e.g. `-> needs_input`) is surfaced as a distinct line. The engine's
/// [`Session`] stays connected (as in [`run`]) so new relay envelopes keep firing
/// the watch.
///
/// A live stream can't be paged or reconstructed from the point-in-time archive,
/// so `--pager`/`--jsonl` were already rejected in parsing (see
/// [`MessageView::check_follow`]); `--last`/`--role`/`--tools` shape the initial
/// tail and the streamed messages, and `--json` emits newline-delimited
/// role-tagged objects instead of the rendered text.
///
/// [`EventOrder`]: agentium_core::session_loader::EventOrder
async fn cmd_follow(
    engine: &Engine,
    author: &Pubkey,
    selector: Option<&str>,
    view: &MessageView,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        EventOrder, load_deleted_session_states_for_author, load_session_messages_for_author,
        load_session_states_for_author, resolve_session_including_deleted,
    };

    let selector = selector
        .ok_or("no session — pass a selector (see `agentium list`) or set $AGENTIUM_SESSION")?;

    // Resolve the selector once to the stable session id (its kind-1988 `d` tag)
    // and its starting status; every watch and re-read below keys off that id.
    let (session_id, mut last_status) = {
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
        let state = resolve_session_including_deleted(&live, &deleted, selector)?;
        (state.claude_session_id.clone(), state.status.clone())
    };

    // A live follow never pages, so `auto` color just follows the real stdout
    // tty (there is no color-aware pager to also satisfy, unlike `cmd_log`).
    let color = view.color.enabled(std::io::stdout().is_terminal());

    // Subscribe *before* the initial read so an event that lands in the gap
    // between reading the tail and entering the loop still wakes us: the
    // subscriptions capture from now on, the read captures history, and together
    // they leave no hole. `watch_session` is scoped to this session's kind-1988
    // events; `watch_sessions` wakes on any kind-31988 revision (we re-resolve
    // and compare, so an unrelated session's change prints nothing).
    let mut msg_watch = engine.watch_session(&session_id)?;
    let mut state_watch = engine.watch_sessions()?;

    // Initial tail: print it honoring --last/--role/--tools, but seed the cursor
    // at the whole conversation's max order (not the filtered subset's) so the
    // streamed delta resumes strictly after everything already shown.
    let mut printed_any = false;
    let mut last_order: Option<EventOrder> = {
        let txn = Transaction::new(engine.ndb())?;
        let loaded = load_session_messages_for_author(engine.ndb(), &txn, author, &session_id);
        let selected = view.select(&loaded.messages);
        emit_follow_messages(&selected, color, as_json, &mut printed_any);
        loaded.max_order
    };

    // Follow until Ctrl-C. Ctrl-C breaks cleanly; either watch ending (`false`,
    // e.g. the database was torn down) also ends the loop.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            changed = msg_watch.changed() => {
                if !changed {
                    break;
                }
            }
            changed = state_watch.changed() => {
                if !changed {
                    break;
                }
            }
        }

        // Message delta: re-read the full ordered snapshot, emit only the suffix
        // whose order is past `last_order` (role/tool filtered, but never tailed
        // — `--last` bounds only the initial view), then advance the cursor to
        // the new global max.
        {
            let txn = Transaction::new(engine.ndb())?;
            let loaded = load_session_messages_for_author(engine.ndb(), &txn, author, &session_id);
            let start = first_after(&loaded.orders, last_order);
            let fresh: Vec<&Message> = loaded.messages[start..]
                .iter()
                .filter(|m| view.keep(m))
                .collect();
            emit_follow_messages(&fresh, color, as_json, &mut printed_any);
            if loaded.max_order.is_some() {
                last_order = loaded.max_order;
            }
        }

        // Status transition: re-resolve the session's kind-31988 status and, when
        // it flips, print a distinct line (so `-> needs_input` surfaces mid-follow).
        {
            let txn = Transaction::new(engine.ndb())?;
            let live = load_session_states_for_author(engine.ndb(), &txn, author);
            let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
            if let Ok(state) = resolve_session_including_deleted(&live, &deleted, &session_id)
                && state.status != last_status
            {
                emit_follow_status(&state.status, color, as_json);
                last_status = state.status.clone();
            }
        }
    }

    Ok(())
}

/// Stream a batch of followed messages to stdout, flushed so the follower sees
/// each immediately. Text mode reuses [`render_message`] and mirrors
/// [`render_messages`]' spacing — a blank line *between* entries, none before
/// the first — with `printed_any` carrying that "have we printed yet" state
/// across batches. JSON mode emits one compact role-tagged object per line
/// (newline-delimited, the streaming counterpart to `log --json`'s array),
/// reusing the same [`Message::to_json`] shape.
///
/// [`Message::to_json`]: agentium_core::messages::Message::to_json
fn emit_follow_messages(messages: &[&Message], color: bool, as_json: bool, printed_any: &mut bool) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for m in messages {
        if as_json {
            if let Ok(line) = serde_json::to_string(&m.to_json()) {
                let _ = writeln!(out, "{line}");
            }
        } else {
            // A blank line separates entries (mirroring `render_messages`), but
            // not before the very first one printed across all batches.
            if *printed_any {
                let _ = writeln!(out);
            }
            let _ = write!(out, "{}", render_message(m, color));
        }
        *printed_any = true;
    }
    let _ = out.flush();
}

/// Print a session's new status as a distinct, colored line while following (or
/// a `{"event":"status",…}` object under `--json`), so a mid-follow transition
/// like `-> needs_input` stands out from the message stream.
fn emit_follow_status(status: &str, color: bool, as_json: bool) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if as_json {
        let obj = serde_json::json!({ "event": "status", "status": status });
        if let Ok(line) = serde_json::to_string(&obj) {
            let _ = writeln!(out, "{line}");
        }
    } else {
        let (glyph, label, sgr) = status_style(status);
        let line = paint(color, sgr, &format!("── {glyph} {label} ──"));
        let _ = writeln!(out, "\n{line}");
    }
    let _ = out.flush();
}

/// Join JSONL lines into a single newline-terminated block (empty stays empty),
/// so the whole archive rides the same [`emit`] path as the rendered transcript.
fn join_lines(lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Write `output` to stdout, or through a pager when `use_pager`.
///
/// The pager command comes from `$AGENTIUM_PAGER`, then `$PAGER`, else the
/// built-in default `less -R` (`-R` so the rendered ANSI color survives —
/// answering "keep color when it's long"). If the pager can't be spawned (not
/// installed, empty command), we fall back to printing plainly rather than
/// failing. A broken pipe (the user quit the pager early) is ignored.
fn emit(output: &str, use_pager: bool) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if !use_pager {
        print!("{output}");
        return Ok(());
    }

    let pager = env::var("AGENTIUM_PAGER")
        .ok()
        .or_else(|| env::var("PAGER").ok())
        .unwrap_or_else(|| "less -R".to_string());
    let mut parts = pager.split_whitespace();
    let Some(program) = parts.next() else {
        print!("{output}");
        return Ok(());
    };

    let child = Command::new(program)
        .args(parts)
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        // No usable pager (e.g. `less` absent) — degrade to a plain print.
        Err(_) => {
            print!("{output}");
            return Ok(());
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // Ignore the write result: a pager the user quits early closes the pipe,
        // and that EPIPE is expected, not an error worth surfacing.
        let _ = stdin.write_all(output.as_bytes());
    }
    let _ = child.wait();
    Ok(())
}

/// The run-configs registered for a session's host+cwd — the ones that would
/// run *inside* it. [`load_run_configs_from_ndb`] buckets configs by cwd for a
/// given hostname, so we load for the session's host and take its cwd's bucket
/// (empty when none are configured there).
///
/// [`load_run_configs_from_ndb`]: agentium_core::session_loader::load_run_configs_from_ndb
fn matching_run_configs(
    ndb: &nostrdb::Ndb,
    txn: &Transaction,
    author: &Pubkey,
    state: &SessionState,
) -> Vec<agentium_core::config::RunConfig> {
    use agentium_core::session_loader::load_run_configs_from_ndb;
    let mut by_cwd = load_run_configs_from_ndb(ndb, txn, author, &state.hostname);
    by_cwd
        .remove(&std::path::PathBuf::from(&state.cwd))
        .unwrap_or_default()
}

/// A folded read of a session's kind-1988 conversation for the detail view: how
/// many messages it holds, and the tool of any still-unanswered permission
/// request. Owned (not borrowing the message vec) so it can be rendered and
/// serialized after the transaction is dropped.
struct ConversationSummary {
    message_count: usize,
    /// The tool named by the latest *unresponded* permission request, if the
    /// session is waiting on a decision.
    pending_permission: Option<String>,
}

impl ConversationSummary {
    /// Fold the reconstructed message list into a summary. A permission request
    /// is pending when its reconstructed [`response`] is `None`; the newest such
    /// request is the one a human would act on, so we scan newest-first.
    ///
    /// [`response`]: agentium_core::messages::PermissionRequest::response
    fn from_messages(messages: &[agentium_core::messages::Message]) -> Self {
        use agentium_core::messages::Message;
        let pending_permission = messages.iter().rev().find_map(|m| match m {
            Message::PermissionRequest(p) if p.response.is_none() => Some(p.tool_name.clone()),
            _ => None,
        });
        ConversationSummary {
            message_count: messages.len(),
            pending_permission,
        }
    }
}

/// The `--json` view of a session: every [`SessionState`] field, plus the
/// rendered `agentium:word-word-word` URI the terminal rows show but the raw
/// struct omits (it carries only the underlying `claude_session_id`). Flattened
/// so the extra field sits alongside the state, not nested under it.
#[derive(serde::Serialize)]
struct SessionJson<'a> {
    #[serde(flatten)]
    state: &'a SessionState,
    /// The sayable reference (`agentium_core::SessionState::agentium_uri`) an
    /// external agent quotes without re-encoding the word-id itself.
    agentium_uri: String,
}

impl<'a> SessionJson<'a> {
    fn new(state: &'a SessionState) -> Self {
        SessionJson {
            state,
            agentium_uri: state.agentium_uri(),
        }
    }
}

/// The `show --json` object: the session state (with its URI), the run-configs
/// on its host+cwd, its latest usage (absent when the archive holds no
/// completed turn), and a conversation summary. Mirrors the fields the plain
/// text view renders, structured for machine consumers.
#[derive(serde::Serialize)]
struct SessionDetailJson<'a> {
    session: SessionJson<'a>,
    /// `RunConfig` serializes its id/name/command (its `updated_at` is
    /// `#[serde(skip)]`), so the slice needs no wrapper.
    run_configs: &'a [agentium_core::config::RunConfig],
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<UsageJson>,
    conversation: ConversationJson,
}

/// The `--json` shape of a [`UsageInfo`] snapshot. `UsageInfo` isn't itself
/// `Serialize`, and we add the derived `context_tokens` (the figure the desktop
/// context bar shows) so consumers don't have to re-sum the buckets.
///
/// [`UsageInfo`]: agentium_core::messages::UsageInfo
#[derive(serde::Serialize)]
struct UsageJson {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    output_tokens: u64,
    context_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    num_turns: u32,
}

impl From<&agentium_core::messages::UsageInfo> for UsageJson {
    fn from(u: &agentium_core::messages::UsageInfo) -> Self {
        UsageJson {
            input_tokens: u.input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            output_tokens: u.output_tokens,
            context_tokens: u.context_tokens(),
            cost_usd: u.cost_usd,
            num_turns: u.num_turns,
        }
    }
}

/// The `--json` shape of a [`ConversationSummary`].
#[derive(serde::Serialize)]
struct ConversationJson {
    message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_permission: Option<String>,
}

impl From<&ConversationSummary> for ConversationJson {
    fn from(s: &ConversationSummary) -> Self {
        ConversationJson {
            message_count: s.message_count,
            pending_permission: s.pending_permission.clone(),
        }
    }
}

/// Width of the label column in the detail view, sized to the longest label
/// (`cli session`, `run configs`), so values align in a second column.
const DETAIL_LABEL_W: usize = 11;

/// Render one `field: value` line of the detail body, indented under its
/// section and padded to [`DETAIL_LABEL_W`] so values line up.
fn field(label: &str, value: &str) -> String {
    format!("  {label:<DETAIL_LABEL_W$}  {value}\n")
}

/// Render the full `agentium show` detail block for a session as plain text.
///
/// A header line (`agentium:` URI + colored status) and the display title, then
/// the kind-31988 state fields, the host+cwd run-configs, the usage snapshot
/// (omitted entirely when `None`), and the conversation summary. Returns an
/// owned `String` (rather than printing) so the layout is unit-testable; ANSI
/// color is applied only when `color` (stdout is a tty).
fn render_detail(
    s: &SessionState,
    run_configs: &[agentium_core::config::RunConfig],
    usage: Option<&agentium_core::messages::UsageInfo>,
    summary: &ConversationSummary,
    now: u64,
    color: bool,
) -> String {
    let (glyph, label, sgr) = status_style(&s.status);
    let mut out = String::new();

    // Header: the sayable ref + status, then the human title.
    out.push_str(&format!(
        "{}  {}\n",
        paint(color, "90", &s.agentium_uri()),
        paint(color, sgr, &format!("{glyph} {label}")),
    ));
    let title = match s.display_title() {
        "" => "(untitled)",
        t => t,
    };
    out.push_str(&format!("{}\n\n", paint(color, SGR_BOLD, title)));

    // kind-31988 state fields. A dash stands in for an absent optional tag.
    let dash = |v: Option<&str>| v.filter(|t| !t.is_empty()).unwrap_or("-").to_string();
    out.push_str(&field("session", &s.claude_session_id));
    out.push_str(&field("cli session", &dash(s.cli_session_id.as_deref())));
    out.push_str(&field("spawn id", &dash(s.spawn_id.as_deref())));
    out.push_str(&field(
        "host",
        if s.hostname.is_empty() {
            "(unknown host)"
        } else {
            &s.hostname
        },
    ));
    out.push_str(&field("cwd", &abbreviate_home(&s.cwd, &s.home_dir)));
    out.push_str(&field("home", &dash(Some(s.home_dir.as_str()))));
    out.push_str(&field("backend", &dash(s.backend.as_deref())));
    out.push_str(&field("perm mode", &dash(s.permission_mode.as_deref())));
    if let Some(ind) = s.indicator.as_deref().filter(|i| !i.is_empty()) {
        out.push_str(&field("indicator", ind));
    }
    out.push_str(&field(
        "created",
        &format!("{} ({})", relative_time(now, s.created_at), s.created_at),
    ));

    // Run-configs on the session's host+cwd.
    out.push('\n');
    out.push_str(&paint(color, SGR_BOLD, "run configs (host+cwd)"));
    out.push('\n');
    if run_configs.is_empty() {
        out.push_str("  none\n");
    } else {
        for rc in run_configs {
            out.push_str(&format!("  {}  {}\n", col(&rc.name, 16), rc.command));
        }
    }

    // Usage snapshot — only when the archive held a completed turn.
    if let Some(u) = usage {
        out.push('\n');
        out.push_str(&paint(color, SGR_BOLD, "usage"));
        out.push('\n');
        out.push_str(&field(
            "context",
            &format!(
                "{} tokens  (in {} · cache +{} ·{})",
                u.context_tokens(),
                u.input_tokens,
                u.cache_creation_input_tokens,
                u.cache_read_input_tokens,
            ),
        ));
        out.push_str(&field("output", &format!("{} tokens", u.output_tokens)));
        out.push_str(&field("turns", &u.num_turns.to_string()));
        if let Some(cost) = u.cost_usd {
            out.push_str(&field("cost", &format!("${cost:.4}")));
        }
    }

    // Conversation summary.
    out.push('\n');
    out.push_str(&paint(color, SGR_BOLD, "conversation"));
    out.push('\n');
    out.push_str(&format!("  {} messages\n", summary.message_count));
    if let Some(tool) = &summary.pending_permission {
        let note = format!("needs input: {tool}");
        out.push_str(&format!("  {}\n", paint(color, SGR_NEEDS_INPUT, &note)));
    }

    out
}

// ---------------------------------------------------------------------------
// `agentium messages` — transcript filtering, rendering, and JSON view
// ---------------------------------------------------------------------------

use agentium_core::messages::{Message, PermissionResponseType, SubagentStatus};
use agentium_core::tools::{ToolCall, ToolResponse, ToolResponses};

/// The transcript-shaping flags for `agentium log`: which roles to keep,
/// whether to fold tool-call/result noise, and how many trailing messages to
/// show. `jsonl` selects the reconstructed-archive path instead (see
/// [`cmd_log`]); it's carried here so the whole command's shape lives in
/// one value.
struct MessageView {
    /// `--role <r>[,<r>…]`: keep only messages whose canonical role token (see
    /// [`message_role`]) matches one of these, case-insensitively. Accumulates
    /// across repeated flags and comma-separated lists; empty keeps all roles.
    roles: Vec<String>,
    /// `--last N` (`-n N`): after role/tool filtering, keep only the trailing `N`.
    last: Option<usize>,
    /// `--tools`/`--no-tools`: when `false`, drop `tool_call`/`tool_result`
    /// messages so the human turns read cleanly. Defaults to `true` (show).
    show_tools: bool,
    /// `--jsonl`: emit reconstructed claude-code JSONL instead of the rendered
    /// transcript. Mutually exclusive in effect with the filters above.
    jsonl: bool,
    /// `--color <when>`: whether to ANSI-color the rendered transcript.
    color: ColorWhen,
    /// `--pager`/`--no-pager`: whether to route output through a pager, like
    /// `git log`.
    pager: PagerMode,
    /// `--follow`/`-f`: after printing the current tail, keep streaming each new
    /// message as it lands (`git log`'s transcript, followed like `tail -f`)
    /// until Ctrl-C. A live stream can't be paged or reconstructed as a
    /// point-in-time archive, so it conflicts with `--pager`/`--jsonl` (see
    /// [`MessageView::check_follow`]).
    follow: bool,
}

/// When to ANSI-color the rendered transcript (`--color`). `Auto` follows the
/// effective sink (a tty or a color-aware pager); `Always`/`Never` force it —
/// `Always` is how you keep color when piping into your own `less -R`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    /// Parse the `--color` value; anything else is an error naming the choices.
    fn parse(s: &str) -> Result<ColorWhen> {
        match s {
            "auto" => Ok(ColorWhen::Auto),
            "always" => Ok(ColorWhen::Always),
            "never" => Ok(ColorWhen::Never),
            other => Err(format!("--color must be auto|always|never, got '{other}'").into()),
        }
    }

    /// Resolve to on/off. `sink_supports_color` is whether the effective output
    /// (tty or color-aware pager) can render ANSI — the `Auto` signal.
    fn enabled(self, sink_supports_color: bool) -> bool {
        match self {
            ColorWhen::Auto => sink_supports_color,
            ColorWhen::Always => true,
            ColorWhen::Never => false,
        }
    }
}

/// Whether to page `log` output (`--pager`/`--no-pager`). `Auto` pages only when
/// stdout is a tty (so a pipe stays unpaged); the flags force it either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PagerMode {
    Auto,
    Always,
    Never,
}

impl PagerMode {
    /// Resolve to on/off given whether stdout is a terminal.
    fn enabled(self, stdout_tty: bool) -> bool {
        match self {
            PagerMode::Auto => stdout_tty,
            PagerMode::Always => true,
            PagerMode::Never => false,
        }
    }
}

impl MessageView {
    /// Whether a single message survives the role/tool filters — the per-message
    /// predicate shared by [`select`](MessageView::select) (which then also
    /// tails with `--last`) and the live `--follow` stream (which filters each
    /// new message the same way but never tails it).
    fn keep(&self, m: &Message) -> bool {
        let role_ok = self.roles.is_empty()
            || self
                .roles
                .iter()
                .any(|r| message_role(m).eq_ignore_ascii_case(r));
        role_ok && (self.show_tools || !is_tool_message(m))
    }

    /// Apply the role/tool/last filters to an ordered message slice, returning
    /// borrowed references in the same order. Order of operations: role filter,
    /// then tool fold, then the `--last` tail — so `--last N` counts the
    /// messages actually shown, not ones a filter already dropped.
    fn select<'a>(&self, messages: &'a [Message]) -> Vec<&'a Message> {
        let mut kept: Vec<&Message> = messages.iter().filter(|m| self.keep(m)).collect();
        if let Some(n) = self.last {
            let start = kept.len().saturating_sub(n);
            kept.drain(..start);
        }
        kept
    }

    /// Reject the flag combinations that a live `--follow` can't honor. A live
    /// stream can't be paged (there is no end to page over), and the `--jsonl`
    /// reconstruction is a point-in-time dump of a finished archive, not a
    /// stream — so both conflict with `--follow`. Returns the reason on
    /// conflict; a no-op when `--follow` isn't set.
    fn check_follow(&self) -> Result<()> {
        if !self.follow {
            return Ok(());
        }
        if self.jsonl {
            return Err(
                "cannot --follow --jsonl: the reconstructed source archive is a \
                 point-in-time dump, not a live stream"
                    .into(),
            );
        }
        if self.pager == PagerMode::Always {
            return Err("cannot page a live follow: --follow and --pager conflict".into());
        }
        Ok(())
    }
}

/// Index of the first item whose order key is strictly greater than `last`
/// (`0` when `last` is `None`, i.e. nothing printed yet — take everything).
///
/// The input keys must be ascending (as [`load_session_messages_for_author`]
/// returns them); this then partitions them into "already printed" / "new" by
/// the order key *itself*, never by a message count. That is the out-of-order
/// guard the live follower needs: a message that arrives late and sorts before
/// the previous max is simply not in the `> last` suffix, so it can never shove
/// an already-printed message back into the tail (which a count-based cursor
/// would do). Live callers key off [`EventOrder`]; the generic bound keeps the
/// selection unit-testable with plain integers.
///
/// [`load_session_messages_for_author`]: agentium_core::session_loader::load_session_messages_for_author
/// [`EventOrder`]: agentium_core::session_loader::EventOrder
fn first_after<T: Ord + Copy>(orders: &[T], last: Option<T>) -> usize {
    match last {
        None => 0,
        Some(last) => orders.partition_point(|o| *o <= last),
    }
}

/// The canonical role token for a message — the axis `--role` filters on and the
/// `role` tag the `--json` view carries. Collapses each [`Message`] variant to
/// the kind-1988 role vocabulary a reader would type.
fn message_role(m: &Message) -> &'static str {
    match m {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolCalls(_) => "tool_call",
        Message::ToolResponse(_) => "tool_result",
        Message::PermissionRequest(_) => "permission_request",
        Message::CompactionComplete(_) => "compaction",
        Message::Subagent(_) => "subagent",
        Message::System(_) => "system",
        Message::Error(_) => "error",
        Message::TodoUpdate(_) => "todo",
    }
}

/// Whether a message is tool-call/result noise that `--no-tools` folds away.
fn is_tool_message(m: &Message) -> bool {
    matches!(m, Message::ToolCalls(_) | Message::ToolResponse(_))
}

/// Terminal presentation for a message role: a human label and an SGR color.
/// Mirrors [`message_role`]'s vocabulary; used to head each rendered entry.
fn role_style(m: &Message) -> (&'static str, &'static str) {
    match m {
        Message::User(_) => ("user", "36"),
        Message::Assistant(_) => ("assistant", "32"),
        Message::ToolCalls(_) => ("tool", "35"),
        Message::ToolResponse(_) => ("result", "90"),
        Message::PermissionRequest(_) => ("permission", SGR_NEEDS_INPUT),
        Message::CompactionComplete(_) => ("compaction", "90"),
        Message::Subagent(_) => ("subagent", "34"),
        Message::System(_) => ("system", "90"),
        Message::Error(_) => ("error", "31"),
        Message::TodoUpdate(_) => ("todo", "90"),
    }
}

/// Render a filtered message stream as plain text, one entry per message
/// separated by a blank line. Returns an owned `String` (like [`render_detail`])
/// so the layout is unit-testable; ANSI color is applied only via [`paint`] when
/// `color` (stdout is a tty).
fn render_messages(messages: &[&Message], color: bool) -> String {
    let mut out = String::new();
    for (i, m) in messages.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_message(m, color));
    }
    out
}

/// Render a single message: a colored role header line, then its body indented
/// two spaces. Every [`Message`] variant is handled so the transcript never
/// silently drops an entry.
fn render_message(m: &Message, color: bool) -> String {
    let (label, sgr) = role_style(m);
    let body = match m {
        Message::User(u) => {
            let mut b = u.text.clone();
            if !u.images.is_empty() {
                if !b.is_empty() {
                    b.push('\n');
                }
                b.push_str(&format!("[{} image(s)]", u.images.len()));
            }
            b
        }
        Message::Assistant(a) => a.text().to_string(),
        Message::ToolCalls(calls) => calls
            .iter()
            .map(render_tool_call)
            .collect::<Vec<_>>()
            .join("\n"),
        Message::ToolResponse(tr) => render_tool_response(tr),
        Message::PermissionRequest(p) => {
            format!("{}  [{}]", p.tool_name, decision_label(p.response))
        }
        Message::CompactionComplete(c) => format!("{} tokens before compaction", c.pre_tokens),
        Message::Subagent(s) => format!(
            "{}: {}  [{}]",
            s.subagent_type,
            s.description,
            subagent_status_label(s.status)
        ),
        Message::System(s) => s.clone(),
        Message::Error(e) => e.clone(),
        Message::TodoUpdate(v) => todo_summary(v),
    };

    let mut out = paint(color, sgr, label);
    out.push('\n');
    for line in body.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One line for a tool call: its registry name plus a one-line summary of the
/// arguments JSON (whitespace-flattened and truncated).
fn render_tool_call(call: &ToolCall) -> String {
    let name = call.calls().tool_name();
    let args = one_line(&call.calls().arguments(), 100);
    if args.is_empty() {
        name.to_string()
    } else {
        format!("{name}  {args}")
    }
}

/// One line for a tool response, keyed on the underlying [`ToolResponses`]
/// variant. The loader's message stream only ever builds `ExecutedTool`, but the
/// other variants are handled so a directly-rendered `ToolResponse` never
/// silently vanishes.
fn render_tool_response(tr: &ToolResponse) -> String {
    match tr.responses() {
        ToolResponses::ExecutedTool(e) => {
            if e.summary.is_empty() {
                e.tool_name.clone()
            } else {
                format!("{}: {}", e.tool_name, one_line(&e.summary, 200))
            }
        }
        ToolResponses::Error(msg) => format!("error: {}", one_line(msg, 200)),
        ToolResponses::Query(q) => format!("query: {} notes", q.notes.len()),
        ToolResponses::PresentNotes(n) => format!("present: {n} notes"),
    }
}

/// The bracketed decision shown on a permission request row.
fn decision_label(response: Option<PermissionResponseType>) -> &'static str {
    match response {
        Some(PermissionResponseType::Allowed) => "allowed",
        Some(PermissionResponseType::Denied) => "denied",
        None => "pending",
    }
}

/// A subagent's status as a lowercase word.
fn subagent_status_label(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Running => "running",
        SubagentStatus::Completed => "completed",
        SubagentStatus::Failed => "failed",
    }
}

/// A one-line summary of a `TodoWrite` payload: the number of items, plus the
/// content of the in-progress one when the payload has the expected shape.
/// Falls back to a flattened one-line dump for anything unexpected.
fn todo_summary(v: &serde_json::Value) -> String {
    let Some(todos) = v.get("todos").and_then(|t| t.as_array()) else {
        return one_line(&v.to_string(), 120);
    };
    let active = todos
        .iter()
        .find(|t| t.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
        .and_then(|t| t.get("content").and_then(|c| c.as_str()));
    match active {
        Some(content) => format!("{} item(s), doing: {}", todos.len(), one_line(content, 80)),
        None => format!("{} item(s)", todos.len()),
    }
}

/// Flatten `s` to a single line (runs of whitespace, including newlines,
/// collapse to one space) and truncate to `max` display chars with an ellipsis.
/// Keeps a summary scannable on one row regardless of the source's line breaks.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    col_ellipsis(&flat, max)
}

/// Truncate to `max` chars with a trailing `…` when cut; unlike [`col`], does
/// not pad (transcript bodies aren't columnar).
fn col_ellipsis(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut t: String = chars[..max.saturating_sub(1)].iter().collect();
    t.push('…');
    t
}

/// Which sessions `list` shows. Tombstoned sessions are hidden by default so the
/// list stays clean; `--deleted`/`--all` surface them so a soft-deleted session
/// (and the durable `agentium:` ref that quotes it) is still discoverable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListScope {
    /// Live sessions only — the default.
    Live,
    /// Only tombstoned (deleted) sessions.
    Deleted,
    /// Live and tombstoned sessions together.
    All,
}

impl ListScope {
    /// Select the scope from the `--all`/`--deleted` flags. `--all` (live +
    /// deleted) wins over `--deleted` (deleted only); neither leaves the default
    /// live-only list.
    fn from_flags(all: bool, deleted: bool) -> ListScope {
        match (all, deleted) {
            (true, _) => ListScope::All,
            (false, true) => ListScope::Deleted,
            (false, false) => ListScope::Live,
        }
    }
}

/// `agentium list` — enumerate this identity's sessions, newest first, grouped
/// by host.
///
/// Reads the kind-31988 session-state set from the engine's synced cache (scoped
/// to `author`), drops rows that don't match `filters`, and renders one row per
/// session: a colored status glyph + label, the title, the working directory,
/// backend, permission mode, and how long ago it last updated. With `as_json`,
/// each session is emitted as a [`SessionJson`] (the state plus its `agentium:`
/// URI). Status colors are written only when stdout is a terminal.
fn cmd_list(
    engine: &Engine,
    author: &Pubkey,
    filters: &ListFilters,
    scope: ListScope,
    as_json: bool,
) -> Result<()> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author,
    };

    let txn = Transaction::new(engine.ndb())?;
    let mut sessions = match scope {
        ListScope::Live => load_session_states_for_author(engine.ndb(), &txn, author),
        ListScope::Deleted => load_deleted_session_states_for_author(engine.ndb(), &txn, author),
        ListScope::All => {
            let mut v = load_session_states_for_author(engine.ndb(), &txn, author);
            v.extend(load_deleted_session_states_for_author(
                engine.ndb(),
                &txn,
                author,
            ));
            v
        }
    };
    sessions.retain(|s| filters.matches(s));

    if as_json {
        // The full SessionState set plus its rendered `agentium:` URI,
        // machine-readable. External agents (e.g. the agentium Claude skill)
        // quote their own session ref into a headway done-comment straight from
        // this field rather than reimplementing the word-id encoding.
        let view: Vec<SessionJson> = sessions.iter().map(SessionJson::new).collect();
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }

    let color = std::io::stdout().is_terminal();
    let now = now_secs();

    // Surface sessions waiting on the user up front — the one status a human
    // scanning the list most needs to act on.
    let waiting = sessions
        .iter()
        .filter(|s| s.status == "needs_input")
        .count();
    if waiting > 0 {
        let note = format!("{waiting} session(s) need input");
        println!("{}\n", paint(color, SGR_NEEDS_INPUT, &note));
    }

    // Size the leading `agentium:` column to the longest ref in the list so
    // every URI renders in full — a truncated ref can't be copied into
    // `show`/`send`, which is the whole point of leading with it.
    let sref_width = sessions
        .iter()
        .map(|s| {
            agentium_core::wordid::session_ref(&s.claude_session_id)
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0);

    for (host, group) in group_by_host(sessions) {
        println!("{}", paint(color, SGR_BOLD, &host));
        for s in group {
            println!("{}", session_row(&s, now, color, sref_width));
        }
    }

    Ok(())
}

/// Amber — the status color for a session waiting on the user, and the color of
/// the summary line that surfaces them.
const SGR_NEEDS_INPUT: &str = "33";
/// Bold, for the per-host group headers.
const SGR_BOLD: &str = "1";

/// Render one session as a padded, aligned row (indented under its host header).
///
/// Leads with the session's sayable `agentium:word-word-word` reference — the
/// selector a human copies into `show`/`send`/etc. — then the status, title,
/// working dir, backend, permission mode, and last-updated age. `sref_width` is
/// the column width for that leading reference; the caller sizes it to the
/// longest ref in the list so the full, copyable URI is never truncated.
fn session_row(s: &SessionState, now: u64, color: bool, sref_width: usize) -> String {
    let (glyph, label, sgr) = status_style(&s.status);
    let sref = agentium_core::wordid::session_ref(&s.claude_session_id);
    let sref_col = paint(color, "90", &col(&sref, sref_width));
    let status_col = paint(color, sgr, &format!("{glyph} {}", col(&label, 11)));
    let title = col(s.display_title(), 30);
    let cwd = col(&abbreviate_home(&s.cwd, &s.home_dir), 26);
    let backend = col(s.backend.as_deref().unwrap_or("-"), 8);
    let mode = col(s.permission_mode.as_deref().unwrap_or("-"), 12);
    format!(
        "  {sref_col}  {status_col}  {title}  {}  {backend}  {mode}  {}",
        paint(color, "90", &cwd),
        paint(color, "90", &relative_time(now, s.created_at)),
    )
}

/// Terminal presentation for a status string: a glyph, a human label, and an SGR
/// color. Mirrors [`AgentStatus`] — which lives in the egui-side notedeck_dave
/// crate (its `color()` returns an `egui::Color32`), so it can't be reused from a
/// terminal CLI. An unknown status shows its raw token, uncolored.
///
/// [`AgentStatus`]: https://docs.rs/notedeck_dave
fn status_style(status: &str) -> (&'static str, String, &'static str) {
    match status {
        "idle" => ("○", "Idle".into(), "90"),
        "working" => ("●", "Working".into(), "32"),
        "needs_input" => ("◆", "Needs Input".into(), SGR_NEEDS_INPUT),
        "error" => ("✖", "Error".into(), "31"),
        "done" => ("✓", "Done".into(), "34"),
        "pending" => ("◌", "Pending".into(), "36"),
        "deleted" => ("⊘", "Deleted".into(), "90"),
        other => ("?", other.to_string(), "0"),
    }
}

/// Replace a leading home directory with `~`, matching how the desktop shows
/// working directories.
fn abbreviate_home(cwd: &str, home: &str) -> String {
    match cwd.strip_prefix(home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

/// A coarse "2h ago" for an event timestamp, relative to `now` (both Unix secs).
fn relative_time(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

/// Truncate `s` to `width` display chars (appending `…` when cut) and left-pad
/// to `width` so columns align. ANSI color must be applied *after* this, or the
/// invisible escape bytes would throw the padding off.
fn col(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > width {
        let mut t: String = chars[..width.saturating_sub(1)].iter().collect();
        t.push('…');
        return t;
    }
    format!("{s:<width$}")
}

/// Wrap `s` in an SGR color when `enabled` (stdout is a tty), else return it
/// plain. `sgr` is the numeric code(s), e.g. `"32"` or `"33"`.
fn paint(enabled: bool, sgr: &str, s: &str) -> String {
    if enabled {
        format!("\x1b[{sgr}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Group sessions by host, ordering hosts by their most recent activity and
/// sessions within a host newest-first — mirroring how the desktop groups the
/// scene by host then cwd.
fn group_by_host(sessions: Vec<SessionState>) -> Vec<(String, Vec<SessionState>)> {
    let mut groups: Vec<(String, Vec<SessionState>)> = Vec::new();
    for s in sessions {
        let host = if s.hostname.is_empty() {
            "(unknown host)".to_string()
        } else {
            s.hostname.clone()
        };
        match groups.iter_mut().find(|(h, _)| *h == host) {
            Some((_, v)) => v.push(s),
            None => groups.push((host, vec![s])),
        }
    }
    // Newest-first within each host, then hosts by their newest session.
    for (_, v) in &mut groups {
        v.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.first().map_or(0, |s| s.created_at)));
    groups
}

/// The current Unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `list` row filters, all optional and case-insensitive. `status` matches the
/// raw status token exactly; `host`, `cwd`, and `backend` match as substrings.
struct ListFilters {
    host: Option<String>,
    status: Option<String>,
    cwd: Option<String>,
    backend: Option<String>,
}

impl ListFilters {
    fn matches(&self, s: &SessionState) -> bool {
        let contains = |hay: &str, needle: &Option<String>| {
            needle
                .as_ref()
                .is_none_or(|n| hay.to_lowercase().contains(&n.to_lowercase()))
        };
        let eq = |hay: &str, needle: &Option<String>| {
            needle.as_ref().is_none_or(|n| hay.eq_ignore_ascii_case(n))
        };
        contains(&s.hostname, &self.host)
            && eq(&s.status, &self.status)
            && contains(&s.cwd, &self.cwd)
            && contains(s.backend.as_deref().unwrap_or(""), &self.backend)
    }
}

/// Resolve the relay URL, preferring `--relay`, then `$AGENTIUM_RELAY`, then the
/// stored config, then the built-in default. Passing `--relay` also persists it
/// as the sticky default for later runs, so the flag is only needed once — the
/// relay is a single connection endpoint, not a per-operation target, so
/// remembering it can't race a concurrent run the way a stateful
/// current-selection would.
fn resolve_relay(flag: Option<String>) -> Result<String> {
    if let Some(url) = flag {
        nostrdb_net::relay::sync::write_config(APP, "relay", &url)?;
        return Ok(url);
    }
    Ok(env::var("AGENTIUM_RELAY")
        .ok()
        .or_else(|| nostrdb_net::relay::sync::read_config(APP, "relay"))
        .unwrap_or_else(|| nostrdb_net::relay::sync::DEFAULT_RELAY.to_string()))
}

/// Resolve the nostrdb cache dir. Precedence: `--db > stored config > default`
/// (`open_ndb`'s `<data-dir>/agentium-cli`). Passing `--db` persists it, same as
/// `--relay`; `None` lets `open_ndb` pick the default.
fn resolve_db(flag: Option<String>) -> Result<Option<String>> {
    if let Some(path) = flag {
        nostrdb_net::relay::sync::write_config(APP, "db", &path)?;
        return Ok(Some(path));
    }
    Ok(nostrdb_net::relay::sync::read_config(APP, "db"))
}

// ---------------------------------------------------------------------------
// argument parsing
// ---------------------------------------------------------------------------

struct Cli {
    secret: Option<([u8; 32], Pubkey)>,
    author: Option<Pubkey>,
    /// Raw `--relay`/`--db` flags, if given; resolved (and persisted) by
    /// [`resolve_relay`]/[`resolve_db`] against env vars and stored config.
    relay: Option<String>,
    db: Option<String>,
    json: bool,
    /// `list` row filters (`--host`/`--status`/`--cwd`/`--backend`).
    host: Option<String>,
    status: Option<String>,
    cwd: Option<String>,
    backend: Option<String>,
    /// Which sessions `list` shows (`--deleted`/`--all`); [`ListScope::Live`] by default.
    list_scope: ListScope,
    command: Command,
}

impl Cli {
    /// Parse args (without the program name). Returns `Ok(None)` when usage
    /// should be printed (no command, `-h`/`--help`).
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>> {
        // Precedence: `--nsec` overrides the `AGENTIUM_NSEC` env var, which
        // overrides the key stored by `login`. `--relay`/`--db` are captured raw
        // here and resolved against env/stored config in `run` (see
        // `resolve_relay`/`resolve_db`).
        let mut nsec = env::var("AGENTIUM_NSEC")
            .ok()
            .or_else(|| nostrdb_net::relay::sync::stored_nsec(APP));
        let mut relay = None;
        let mut db = None;
        let mut author = None;
        let mut json = false;
        let mut host = None;
        let mut status = None;
        let mut cwd = None;
        let mut backend = None;
        let mut deleted = false;
        let mut all = false;
        // `log` transcript flags. `show_tools` defaults on; `--no-tools`
        // folds tool noise and `--tools` re-asserts the default. Color/pager
        // default to `Auto` (tty detection), like `git log`.
        let mut roles: Vec<String> = Vec::new();
        let mut last = None;
        let mut show_tools = true;
        let mut jsonl = false;
        let mut color = ColorWhen::Auto;
        let mut pager = PagerMode::Auto;
        let mut follow = false;
        // `spawn` flags. `--title`/`--prompt` are values; `--wait` is a switch
        // (also implied by `--prompt`, resolved in `cmd_spawn`).
        let mut title = None;
        let mut prompt = None;
        let mut wait = false;
        let mut positionals: Vec<String> = Vec::new();

        let mut args = args;
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value").into())
                    as Result<String>
            };
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--nsec" => nsec = Some(value("--nsec")?),
                "--relay" => relay = Some(value("--relay")?),
                "--db" => db = Some(value("--db")?),
                "--author" => author = Some(Pubkey::parse(&value("--author")?)?),
                "--json" => json = true,
                "--host" => host = Some(value("--host")?),
                "--status" => status = Some(value("--status")?),
                "--cwd" => cwd = Some(value("--cwd")?),
                "--backend" => backend = Some(value("--backend")?),
                "--deleted" => deleted = true,
                "--all" => all = true,
                // Accumulate roles across repeated `--role` flags and
                // comma-separated lists, so `--role user,assistant` and
                // `--role user --role assistant` both select multiple roles.
                "--role" => roles.extend(
                    value("--role")?
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                ),
                "--last" | "-n" => {
                    last = Some(
                        value(arg.as_str())?
                            .parse::<usize>()
                            .map_err(|_| "--last needs a non-negative integer")?,
                    )
                }
                "--tools" => show_tools = true,
                "--no-tools" => show_tools = false,
                "--jsonl" => jsonl = true,
                "--color" => color = ColorWhen::parse(&value("--color")?)?,
                "--pager" => pager = PagerMode::Always,
                "--no-pager" => pager = PagerMode::Never,
                "--follow" | "-f" => follow = true,
                "--title" => title = Some(value("--title")?),
                "--prompt" => prompt = Some(value("--prompt")?),
                "--wait" => wait = true,
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        let Some((name, rest)) = positionals.split_first() else {
            return Ok(None);
        };
        let view = MessageView {
            roles,
            last,
            show_tools,
            jsonl,
            color,
            pager,
            follow,
        };
        // A live follow can't be paged or reconstructed from the archive, so
        // reject those combinations here (before any relay work spins up).
        view.check_follow()?;
        // `spawn` reuses the shared `--host`/`--cwd`/`--backend` flags as its
        // target (they double as `list` filters), plus its own
        // `--title`/`--prompt`/`--wait`, so it's assembled here where those flags
        // live rather than threading them all through `parse_command`.
        let command = if name == "spawn" {
            Command::Spawn {
                host: host.clone(),
                cwd: cwd.clone(),
                backend: backend.clone(),
                title,
                prompt,
                wait,
            }
        } else {
            parse_command(name, rest, view)?
        };

        // `login`/`logout` manage the stored key themselves, so don't parse (and
        // potentially reject on) whatever key is currently configured.
        // `parse_nsec` hands back a `nostrdb_net::Pubkey`; the rest of the CLI
        // (and the `agentium_core` engine) speaks `enostr::Pubkey`. Both are
        // `[u8; 32]` newtypes, so bridge at this boundary and keep everything
        // downstream in enostr terms.
        let secret = match (&command, nsec) {
            (Command::Login { .. } | Command::Logout, _) => None,
            (_, Some(nsec)) => {
                let (sk, pk) = nostrdb_net::relay::sync::parse_nsec(&nsec)?;
                Some((sk, Pubkey::new(*pk.bytes())))
            }
            (_, None) => None,
        };

        let list_scope = ListScope::from_flags(all, deleted);

        Ok(Some(Cli {
            secret,
            author,
            relay,
            db,
            json,
            host,
            status,
            cwd,
            backend,
            list_scope,
            command,
        }))
    }
}

fn parse_command(name: &str, rest: &[String], view: MessageView) -> Result<Command> {
    Ok(match name {
        "list" => Command::List,
        "show" => Command::Show {
            session: optional_session(rest),
        },
        "log" => Command::Log {
            session: optional_session(rest),
            view,
        },
        "resume" => Command::Resume {
            session: arg(rest, 0, name)?,
        },
        "send" => Command::Send {
            session: arg(rest, 0, name)?,
            text: join_message(&rest[1..])?,
        },
        "interrupt" => Command::Interrupt {
            session: arg(rest, 0, name)?,
        },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
        other => return Err(format!("unknown command '{other}' (try `agentium --help`)").into()),
    })
}

/// The optional session selector for `show`/`log`: the first positional if
/// present, else `$AGENTIUM_SESSION` (the `agentium:` ref a running Dave session
/// exports) so the command with no argument targets the current session. An
/// empty env var is treated as unset; the command errors if neither is present.
fn optional_session(rest: &[String]) -> Option<String> {
    rest.first()
        .cloned()
        .or_else(|| env::var("AGENTIUM_SESSION").ok().filter(|s| !s.is_empty()))
}

/// The `idx`th positional argument to a command, or an error naming the command.
fn arg(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    rest.get(idx)
        .cloned()
        .ok_or_else(|| format!("`{cmd}` is missing an argument").into())
}

/// Join `send`'s trailing positionals into one message body, separated by single
/// spaces, so `agentium send <sel> hey there` needs no quoting. Errors when the
/// result is empty or whitespace-only — an empty `user` message is never
/// something to publish.
fn join_message(words: &[String]) -> Result<String> {
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Err("`send` needs a non-empty message".into());
    }
    Ok(text)
}

fn print_usage() {
    eprintln!(
        "\
agentium — query and control Dave agentic sessions over a running notedeck's relay

USAGE:
    agentium [OPTIONS] <COMMAND>

COMMANDS:
    list              List this identity's sessions, newest first, grouped by
                      host. Filter with --host/--status/--cwd/--backend; --json
                      emits the raw session set. Deleted sessions are hidden
                      unless --deleted/--all is passed.
    show [session]    Show one session's detail: its state, the run-configs on
                      its host+cwd, its latest usage, and a conversation summary
                      (message count + any pending permission). Takes any
                      selector `list` accepts; defaults to $AGENTIUM_SESSION so a
                      running Dave session can just run `agentium show`. --json
                      emits the structured detail object.
    log [session]     Print one session's full conversation, one entry per
                      message, in order (millisecond wall-clock, not seq). Takes
                      any selector `list` accepts; defaults to $AGENTIUM_SESSION.
                      Filter/shape with --role/--last/--tools/--no-tools; --json
                      emits structured message objects, --jsonl the reconstructed
                      claude-code JSONL from the source archive. --follow/-f keeps
                      streaming new messages (and status changes) until Ctrl-C.
    resume <session>  Reopen a closed (even soft-deleted) session on its host so
                      a new message drives its backend again. Takes any selector
                      `list` accepts (d-tag, cli-session id, or agentium: ref);
                      revives the session's agentium: reference in place.
    send <session> <text…>
                      Send a user message to a live session so its running agent
                      (local or remote) picks it up over relay sync, then report
                      the resulting event id. The selector is required (no
                      $AGENTIUM_SESSION default — it would be ambiguous against
                      the text); the message is the remaining words joined with
                      spaces (quote to preserve exact spacing). Reopen a deleted
                      session with `resume` first. --json emits the event id.
    spawn             Tell a (local or remote) Dave host to create a fresh
                      session, then print its new agentium: ref. --host/--cwd/
                      --backend pick the target; omitted, they default to the
                      current session ($AGENTIUM_SESSION) so a bare `spawn`
                      starts a sibling in the same worktree. --title sets a
                      sticky session title; --wait blocks until the host answers;
                      --prompt <text> (implies --wait) sends a first message once
                      the session is up. --json emits {{ spawn_id, host, session,
                      event_id }}.
    interrupt <session>
                      Abort a live session's in-flight turn on its host — the CLI
                      companion to pressing Esc in Dave. Takes any selector `list`
                      accepts; a deleted session has nothing running to interrupt.
    login <nsec>      Store a signing key for later runs
    logout            Forget the stored signing key

OPTIONS:
    --nsec <nsec>     Signing key for this run. Normally unnecessary — run
                      `agentium login` once and it's reused. ($AGENTIUM_NSEC,
                      if set, takes precedence over the stored key.)
    --author <pk>     Identity whose sessions to read (defaults to the signer).
                      Note: sessions are PNS-encrypted to their owner, so a
                      pubkey other than yours lists nothing decryptable.
    --relay <url>     Relay URL. Passing it also remembers it as the default for
                      later runs. (Precedence: --relay > $AGENTIUM_RELAY > stored
                      > {DEFAULT_RELAY})
    --db <path>       nostrdb cache dir (remembered like --relay)
                      [default: <data-dir>/agentium-cli]
    --json            Machine-readable output

  list filters (case-insensitive):
    --host <h>        Only sessions whose host contains <h>
    --status <s>      Only sessions with exactly this status
                      (idle|working|needs_input|error|done|pending)
    --cwd <c>         Only sessions whose working dir contains <c>
    --backend <b>     Only sessions whose backend contains <b>
    --deleted         Show only soft-deleted (tombstoned) sessions
    --all             Show live and deleted sessions together

  log options:
    --role <r[,r…]>   Only messages with these roles, comma-separated and/or
                      repeatable (user|assistant|tool_call|tool_result|
                      permission_request|subagent|system|error|compaction|todo)
    -n, --last <n>    Only the last <n> messages (after other filters)
    --tools           Show tool_call/tool_result messages (default)
    --no-tools        Fold away tool_call/tool_result noise
    --jsonl           Emit reconstructed claude-code JSONL (source archive)
    --color <when>    auto (default) | always | never. `always` keeps color
                      when piping into your own pager (e.g. `less -SR`).
    --pager           Force paging even when stdout isn't a terminal
    --no-pager        Never page; write straight to stdout
                      (pager command: $AGENTIUM_PAGER, $PAGER, else `less -R`)
    -f, --follow      After the tail, stream new messages as they land (like
                      `tail -f`) until Ctrl-C. Also surfaces status changes.
                      Conflicts with --pager/--jsonl (can't page/reconstruct a
                      live stream); with --json, streams newline-delimited objects.

  spawn options (also uses --host/--cwd/--backend above as the target):
    --title <text>    Explicit, sticky session title (else it derives from — and
                      churns with — the first message)
    --wait            Block (bounded) until the host answers with the new
                      session's state, then print its agentium: ref
    --prompt <text>   Deliver <text> as the session's first user message once
                      it's up (implies --wait; also reports the message event id)

    -h, --help        Print this help",
        DEFAULT_RELAY = nostrdb_net::relay::sync::DEFAULT_RELAY,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentium_core::config::RunConfig;
    use agentium_core::messages::UsageInfo;

    /// A SessionState with sensible defaults, overriding the fields the tests
    /// care about. (End-to-end coverage over a real relay lives in a separate
    /// card; these exercise the pure rendering/filtering logic.)
    fn session(host: &str, title: &str, status: &str, created_at: u64) -> SessionState {
        SessionState {
            claude_session_id: format!("{host}-{title}"),
            title: title.to_string(),
            custom_title: None,
            cwd: "/home/u/proj".to_string(),
            status: status.to_string(),
            indicator: None,
            hostname: host.to_string(),
            home_dir: "/home/u".to_string(),
            backend: Some("claude".to_string()),
            permission_mode: Some("default".to_string()),
            created_at,
            cli_session_id: None,
            spawn_id: None,
        }
    }

    #[test]
    fn col_pads_and_truncates() {
        assert_eq!(col("hi", 5), "hi   ");
        assert_eq!(col("exactly", 7), "exactly");
        // longer than width: cut to width-1 chars plus an ellipsis
        assert_eq!(col("toolongword", 5), "tool…");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(100, 100), "0s ago");
        assert_eq!(relative_time(100, 90), "10s ago");
        assert_eq!(relative_time(60, 0), "1m ago");
        assert_eq!(relative_time(3600, 0), "1h ago");
        assert_eq!(relative_time(90_000, 0), "1d ago");
        // 'then' in the future clamps rather than underflows.
        assert_eq!(relative_time(0, 100), "0s ago");
    }

    #[test]
    fn abbreviate_home_replaces_prefix() {
        assert_eq!(abbreviate_home("/home/u/proj", "/home/u"), "~/proj");
        assert_eq!(abbreviate_home("/other/x", "/home/u"), "/other/x");
        assert_eq!(abbreviate_home("/home/u/proj", ""), "/home/u/proj");
    }

    #[test]
    fn display_title_prefers_nonempty_custom() {
        let mut s = session("h", "derived", "idle", 0);
        assert_eq!(s.display_title(), "derived");
        s.custom_title = Some("Custom".into());
        assert_eq!(s.display_title(), "Custom");
        s.custom_title = Some(String::new());
        assert_eq!(s.display_title(), "derived");
    }

    #[test]
    fn status_style_known_and_unknown() {
        let (g, l, c) = status_style("needs_input");
        assert_eq!((g, l.as_str(), c), ("◆", "Needs Input", SGR_NEEDS_INPUT));
        // A tombstoned session gets its own muted glyph rather than the "?" fallback.
        let (g, l, _) = status_style("deleted");
        assert_eq!((g, l.as_str()), ("⊘", "Deleted"));
        let (g, l, c) = status_style("weird");
        assert_eq!((g, l.as_str(), c), ("?", "weird", "0"));
    }

    #[test]
    fn list_scope_from_flags() {
        assert_eq!(ListScope::from_flags(false, false), ListScope::Live);
        assert_eq!(ListScope::from_flags(false, true), ListScope::Deleted);
        assert_eq!(ListScope::from_flags(true, false), ListScope::All);
        // --all wins over --deleted.
        assert_eq!(ListScope::from_flags(true, true), ListScope::All);
    }

    #[test]
    fn paint_gates_on_flag() {
        assert_eq!(paint(false, "32", "x"), "x");
        assert_eq!(paint(true, "32", "x"), "\x1b[32mx\x1b[0m");
    }

    #[test]
    fn group_by_host_orders_by_recency() {
        let sessions = vec![
            session("mac", "old", "idle", 100),
            session("linux", "newest", "working", 300),
            session("mac", "new", "idle", 200),
        ];
        let groups = group_by_host(sessions);
        // linux first: it holds the single newest session (300).
        assert_eq!(groups[0].0, "linux");
        assert_eq!(groups[1].0, "mac");
        // within mac, newest-first.
        let mac: Vec<_> = groups[1].1.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(mac, vec!["new", "old"]);
    }

    #[test]
    fn group_by_host_buckets_empty_hostname() {
        let groups = group_by_host(vec![session("", "x", "idle", 1)]);
        assert_eq!(groups[0].0, "(unknown host)");
    }

    #[test]
    fn filters_match_case_insensitively() {
        let s = session("MacBook", "t", "working", 0);
        let f = |host, status, cwd, backend| ListFilters {
            host,
            status,
            cwd,
            backend,
        };
        // host is a case-insensitive substring.
        assert!(f(Some("mac".into()), None, None, None).matches(&s));
        assert!(!f(Some("linux".into()), None, None, None).matches(&s));
        // status is an exact (case-insensitive) token, not a substring.
        assert!(f(None, Some("WORKING".into()), None, None).matches(&s));
        assert!(!f(None, Some("work".into()), None, None).matches(&s));
        // backend is a substring; empty filters match everything.
        assert!(f(None, None, None, Some("clau".into())).matches(&s));
        assert!(f(None, None, None, None).matches(&s));
    }

    #[test]
    fn session_row_plain_is_uncolored_and_complete() {
        let s = session("mac", "Hello", "working", 0);
        let sref = agentium_core::wordid::session_ref(&s.claude_session_id);
        let row = session_row(&s, 60, false, sref.chars().count());
        assert!(!row.contains('\x1b'), "no ANSI when color=false: {row:?}");
        assert!(row.contains("agentium:"), "row leads with the sayable ref");
        assert!(
            row.contains(&sref) && !row.contains('…'),
            "the full, copyable ref renders untruncated: {row:?}"
        );
        assert!(row.contains("● Working"));
        assert!(row.contains("Hello"));
        assert!(row.contains("~/proj")); // cwd home-abbreviated
        assert!(row.contains("claude"));
        assert!(row.contains("default"));
        assert!(row.contains("1m ago")); // 60s since created_at 0
    }

    #[test]
    fn show_selector_prefers_explicit_arg() {
        // An explicit positional is used verbatim (the $AGENTIUM_SESSION
        // fallback only applies when none is given — exercised end-to-end, not
        // here, to avoid mutating process env in a shared test binary).
        match parse_command("show", &["agentium:a-b-c".to_string()], view_all()).unwrap() {
            Command::Show { session } => assert_eq!(session.as_deref(), Some("agentium:a-b-c")),
            _ => panic!("expected Show"),
        }
    }

    #[test]
    fn log_command_carries_selector_and_view() {
        let view = MessageView {
            roles: vec!["assistant".into()],
            last: Some(5),
            show_tools: false,
            jsonl: true,
            ..view_all()
        };
        match parse_command("log", &["agentium:a-b-c".to_string()], view).unwrap() {
            Command::Log { session, view } => {
                assert_eq!(session.as_deref(), Some("agentium:a-b-c"));
                assert_eq!(view.roles, vec!["assistant".to_string()]);
                assert_eq!(view.last, Some(5));
                assert!(!view.show_tools);
                assert!(view.jsonl);
            }
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn send_requires_session_and_joins_text() {
        // `send <sel> hey there` → the first positional is the selector, the rest
        // join into the message with single spaces (no quoting needed).
        let rest = ["agentium:a-b-c", "hey", "there"].map(String::from);
        match parse_command("send", &rest, view_all()).unwrap() {
            Command::Send { session, text } => {
                assert_eq!(session, "agentium:a-b-c");
                assert_eq!(text, "hey there");
            }
            _ => panic!("expected Send"),
        }
    }

    #[test]
    fn send_missing_session_and_empty_text_are_errors() {
        // No positionals at all → the missing-selector error (session is required;
        // there is no $AGENTIUM_SESSION default for `send`).
        assert!(parse_command("send", &[], view_all()).is_err());
        // A selector but no message words → the empty-message error.
        let one = ["agentium:a-b-c"].map(String::from);
        assert!(parse_command("send", &one, view_all()).is_err());
        // A selector plus a whitespace-only quoted arg is also rejected.
        let blank = ["agentium:a-b-c", "   "].map(String::from);
        assert!(parse_command("send", &blank, view_all()).is_err());
    }

    #[test]
    fn interrupt_requires_session() {
        // `interrupt <sel>` carries just the selector; trailing words are ignored.
        let rest = ["agentium:a-b-c", "extra"].map(String::from);
        match parse_command("interrupt", &rest, view_all()).unwrap() {
            Command::Interrupt { session } => assert_eq!(session, "agentium:a-b-c"),
            _ => panic!("expected Interrupt"),
        }
        // No selector → the missing-argument error (no $AGENTIUM_SESSION default).
        assert!(parse_command("interrupt", &[], view_all()).is_err());
    }

    #[test]
    fn join_message_joins_and_rejects_empty() {
        assert_eq!(
            join_message(&["hey".into(), "there".into()]).unwrap(),
            "hey there"
        );
        assert_eq!(join_message(&["solo".into()]).unwrap(), "solo");
        assert!(join_message(&[]).is_err());
        assert!(join_message(&["  ".into()]).is_err());
    }

    fn usage(input: u64, cc: u64, cr: u64, out: u64, turns: u32, cost: Option<f64>) -> UsageInfo {
        UsageInfo {
            input_tokens: input,
            cache_creation_input_tokens: cc,
            cache_read_input_tokens: cr,
            output_tokens: out,
            cost_usd: cost,
            num_turns: turns,
        }
    }

    #[test]
    fn render_detail_plain_has_every_section() {
        let s = session("mac", "My Session", "working", 0);
        let configs = vec![RunConfig::new("build".into(), "cargo build".into())];
        let u = usage(100, 10, 20, 50, 3, Some(0.25));
        let summary = ConversationSummary {
            message_count: 7,
            pending_permission: Some("Bash".into()),
        };
        let out = render_detail(&s, &configs, Some(&u), &summary, 60, false);

        assert!(!out.contains('\x1b'), "no ANSI when color=false: {out:?}");
        // header + state fields
        assert!(out.contains("agentium:"));
        assert!(out.contains("● Working"));
        assert!(out.contains("My Session"));
        assert!(out.contains("mac-My Session")); // claude_session_id
        assert!(out.contains("~/proj")); // cwd home-abbreviated
        assert!(out.contains("1m ago (0)")); // created relative + raw ts
        // run configs
        assert!(out.contains("run configs"));
        assert!(out.contains("cargo build"));
        // usage: context = input + both cache buckets
        assert!(out.contains("usage"));
        assert!(out.contains("130 tokens"));
        assert!(out.contains("$0.2500"));
        assert!(out.contains("3")); // turns
        // conversation summary + pending permission
        assert!(out.contains("7 messages"));
        assert!(out.contains("needs input: Bash"));
    }

    #[test]
    fn render_detail_omits_usage_section_when_absent() {
        let s = session("mac", "t", "idle", 0);
        let summary = ConversationSummary {
            message_count: 0,
            pending_permission: None,
        };
        let out = render_detail(&s, &[], None, &summary, 0, false);
        assert!(
            !out.contains("usage"),
            "usage section hidden when None: {out:?}"
        );
        assert!(out.contains("run configs"));
        assert!(out.contains("  none")); // no configs registered
        assert!(out.contains("0 messages"));
        assert!(!out.contains("needs input"));
    }

    #[test]
    fn conversation_summary_reports_latest_pending_permission() {
        use agentium_core::messages::{Message, PermissionRequest, PermissionResponseType};
        use serde_json::Value;
        use uuid::Uuid;

        let responded = PermissionRequest::new(
            Uuid::nil(),
            "Read".into(),
            Value::Null,
            None,
            Some(PermissionResponseType::Allowed),
            None,
        );
        let pending =
            PermissionRequest::new(Uuid::nil(), "Bash".into(), Value::Null, None, None, None);
        let msgs = vec![
            Message::User("hi".into()),
            Message::PermissionRequest(responded),
            Message::PermissionRequest(pending),
        ];
        let summary = ConversationSummary::from_messages(&msgs);
        assert_eq!(summary.message_count, 3);
        // The unresponded request wins over the earlier responded one.
        assert_eq!(summary.pending_permission.as_deref(), Some("Bash"));
    }

    #[test]
    fn conversation_summary_no_pending_when_all_responded() {
        use agentium_core::messages::{Message, PermissionRequest, PermissionResponseType};
        use serde_json::Value;
        use uuid::Uuid;

        let responded = PermissionRequest::new(
            Uuid::nil(),
            "Read".into(),
            Value::Null,
            None,
            Some(PermissionResponseType::Denied),
            None,
        );
        let summary = ConversationSummary::from_messages(&[Message::PermissionRequest(responded)]);
        assert!(summary.pending_permission.is_none());
    }

    // -- `agentium messages`: transcript rendering, filtering, JSON view -------

    use agentium_core::messages::{
        AssistantMessage, CompactionInfo, ExecutedTool, Message, PermissionRequest,
        PermissionResponseType, SubagentInfo, SubagentStatus,
    };
    use agentium_core::tools::{QueryCall, ToolCall, ToolCalls, ToolResponse};

    /// A `tool_result` message carrying an [`ExecutedTool`], as the loader builds.
    fn tool_result(name: &str, summary: &str) -> Message {
        Message::ToolResponse(ToolResponse::executed_tool(ExecutedTool {
            tool_name: name.into(),
            summary: summary.into(),
            output: None,
            parent_task_id: None,
            file_update: None,
        }))
    }

    /// The default (show-everything) view: no role filter, no tail, tools shown,
    /// auto color/pager.
    fn view_all() -> MessageView {
        MessageView {
            roles: vec![],
            last: None,
            show_tools: true,
            jsonl: false,
            color: ColorWhen::Auto,
            pager: PagerMode::Auto,
            follow: false,
        }
    }

    #[test]
    fn message_role_tokens_cover_every_variant() {
        assert_eq!(message_role(&Message::User("x".into())), "user");
        assert_eq!(
            message_role(&Message::Assistant(AssistantMessage::from_text("x".into()))),
            "assistant"
        );
        assert_eq!(message_role(&tool_result("Bash", "ok")), "tool_result");
        assert_eq!(message_role(&Message::System("x".into())), "system");
        assert_eq!(message_role(&Message::Error("x".into())), "error");
        assert_eq!(
            message_role(&Message::CompactionComplete(CompactionInfo {
                pre_tokens: 1
            })),
            "compaction"
        );
        assert_eq!(
            message_role(&Message::TodoUpdate(serde_json::json!({}))),
            "todo"
        );
    }

    #[test]
    fn select_filters_by_role_case_insensitively() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi".into())),
            Message::User("again".into()),
        ];
        let view = MessageView {
            roles: vec!["USER".into()],
            ..view_all()
        };
        let kept = view.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|m| message_role(m) == "user"));
    }

    #[test]
    fn select_keeps_any_of_multiple_roles() {
        let msgs = vec![
            Message::User("hello".into()),
            Message::Assistant(AssistantMessage::from_text("hi".into())),
            Message::System("boot".into()),
        ];
        // `--role user,assistant` (or repeated flags) keeps both, drops system.
        let view = MessageView {
            roles: vec!["user".into(), "assistant".into()],
            ..view_all()
        };
        let kept = view.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(
            kept.iter()
                .all(|m| matches!(message_role(m), "user" | "assistant"))
        );
    }

    #[test]
    fn select_folds_tool_noise_with_no_tools() {
        let msgs = vec![
            Message::User("hello".into()),
            tool_result("Bash", "exit 0"),
            Message::Assistant(AssistantMessage::from_text("done".into())),
        ];
        // Default keeps the tool_result; --no-tools drops it.
        assert_eq!(view_all().select(&msgs).len(), 3);
        let folded = MessageView {
            show_tools: false,
            ..view_all()
        };
        let kept = folded.select(&msgs);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|m| !is_tool_message(m)));
    }

    #[test]
    fn select_last_tails_after_other_filters() {
        let msgs = vec![
            Message::User("1".into()),
            tool_result("Bash", "ok"),
            Message::User("2".into()),
            Message::User("3".into()),
        ];
        // --no-tools leaves 3 user messages; --last 2 keeps the final two.
        let view = MessageView {
            last: Some(2),
            show_tools: false,
            ..view_all()
        };
        let kept = view.select(&msgs);
        let texts: Vec<&str> = kept
            .iter()
            .filter_map(|m| match m {
                Message::User(u) => Some(u.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["2", "3"]);
    }

    #[test]
    fn render_messages_plain_handles_every_variant() {
        let subagent = SubagentInfo {
            task_id: "t1".into(),
            description: "explore the tree".into(),
            subagent_type: "Explore".into(),
            status: SubagentStatus::Completed,
            output: String::new(),
            max_output_size: 0,
            tool_results: vec![],
            background: false,
        };
        let msgs = vec![
            Message::System("session started".into()),
            Message::User("hi dave".into()),
            Message::Assistant(AssistantMessage::from_text("**hello**".into())),
            Message::ToolCalls(vec![ToolCall::new(
                "id1".into(),
                ToolCalls::Query(QueryCall::default()),
            )]),
            tool_result("Bash", "exit 0"),
            Message::PermissionRequest(PermissionRequest::new(
                uuid::Uuid::nil(),
                "Write".into(),
                serde_json::Value::Null,
                None,
                Some(PermissionResponseType::Denied),
                None,
            )),
            Message::CompactionComplete(CompactionInfo { pre_tokens: 12000 }),
            Message::Subagent(subagent),
            Message::TodoUpdate(serde_json::json!({
                "todos": [
                    {"content": "first", "status": "completed"},
                    {"content": "second", "status": "in_progress"},
                ]
            })),
            Message::Error("kaboom".into()),
        ];
        let refs: Vec<&Message> = msgs.iter().collect();
        let out = render_messages(&refs, false);

        assert!(!out.contains('\x1b'), "no ANSI when color=false: {out:?}");
        // Each role header renders.
        for label in [
            "system",
            "user",
            "assistant",
            "tool",
            "result",
            "permission",
            "compaction",
            "subagent",
            "todo",
            "error",
        ] {
            assert!(out.contains(label), "missing {label:?} header in:\n{out}");
        }
        // Bodies render (indented) — assistant markdown kept raw.
        assert!(out.contains("  hi dave"));
        assert!(out.contains("  **hello**"));
        assert!(out.contains("Write  [denied]"));
        assert!(out.contains("12000 tokens before compaction"));
        assert!(out.contains("Explore: explore the tree  [completed]"));
        assert!(out.contains("2 item(s), doing: second"));
        assert!(out.contains("  kaboom"));
    }

    #[test]
    fn render_message_colored_wraps_only_the_header() {
        let out = render_message(&Message::User("body".into()), true);
        // The header is colored; the indented body is not repainted.
        assert!(out.starts_with("\x1b[36muser\x1b[0m\n"));
        assert!(out.contains("\n  body\n"));
    }

    #[test]
    fn color_when_resolves_against_sink() {
        // auto follows the sink; always/never override it.
        assert!(ColorWhen::Auto.enabled(true));
        assert!(!ColorWhen::Auto.enabled(false));
        assert!(ColorWhen::Always.enabled(false));
        assert!(!ColorWhen::Never.enabled(true));
        assert_eq!(ColorWhen::parse("always").unwrap(), ColorWhen::Always);
        assert!(ColorWhen::parse("technicolor").is_err());
    }

    #[test]
    fn pager_mode_resolves_against_tty() {
        // auto pages only for a tty; the flags force it either way.
        assert!(PagerMode::Auto.enabled(true));
        assert!(!PagerMode::Auto.enabled(false));
        assert!(PagerMode::Always.enabled(false));
        assert!(!PagerMode::Never.enabled(true));
    }

    #[test]
    fn first_after_selects_the_strictly_greater_suffix() {
        let orders = [10u64, 20, 30];
        // Nothing printed yet: take everything.
        assert_eq!(first_after(&orders, None), 0);
        // Past a middle key: only the strictly-greater tail.
        assert_eq!(first_after(&orders, Some(10)), 1);
        assert_eq!(first_after(&orders, Some(20)), 2);
        // Past the max: nothing new.
        assert_eq!(first_after(&orders, Some(30)), 3);
        // A cursor between keys still splits by value, not position.
        assert_eq!(first_after(&orders, Some(15)), 1);
    }

    #[test]
    fn first_after_guards_out_of_order_inserts() {
        // We followed up to the max (30) of [10, 20, 30]. A later re-read shows a
        // late-arriving event (15) that sorts *before* that max, plus a genuinely
        // newer one (40). Selecting by the order key past the previous max yields
        // only the suffix (40) — the out-of-order 15 is neither re-emitted nor
        // does it drag an already-printed message (10/20/30) back into view, the
        // corruption a count-based cursor would cause.
        let reread = [10u64, 15, 20, 30, 40];
        let start = first_after(&reread, Some(30));
        assert_eq!(&reread[start..], &[40]);
    }

    /// A `MessageView` with only `--follow` and one conflicting flag set,
    /// otherwise default — for exercising [`MessageView::check_follow`].
    fn follow_view(jsonl: bool, pager: PagerMode) -> MessageView {
        MessageView {
            follow: true,
            jsonl,
            pager,
            ..view_all()
        }
    }

    #[test]
    fn check_follow_rejects_paging_and_jsonl() {
        // A plain follow is fine, as is follow with a non-forced pager mode.
        assert!(follow_view(false, PagerMode::Auto).check_follow().is_ok());
        assert!(follow_view(false, PagerMode::Never).check_follow().is_ok());
        // --follow --jsonl and --follow --pager both conflict.
        assert!(follow_view(true, PagerMode::Auto).check_follow().is_err());
        assert!(
            follow_view(false, PagerMode::Always)
                .check_follow()
                .is_err()
        );
        // Without --follow the checks are a no-op even with those flags set.
        let not_following = MessageView {
            follow: false,
            jsonl: true,
            pager: PagerMode::Always,
            ..view_all()
        };
        assert!(not_following.check_follow().is_ok());
    }

    #[test]
    fn keep_matches_role_and_tool_filters() {
        let user = Message::User("hi".into());
        let tool = tool_result("Bash", "ok");
        // Empty role filter keeps everything; tools shown by default.
        assert!(view_all().keep(&user));
        assert!(view_all().keep(&tool));
        // --no-tools drops tool messages but keeps others.
        let no_tools = MessageView {
            show_tools: false,
            ..view_all()
        };
        assert!(no_tools.keep(&user));
        assert!(!no_tools.keep(&tool));
        // A role filter keeps only the named role (case-insensitively).
        let only_user = MessageView {
            roles: vec!["USER".into()],
            ..view_all()
        };
        assert!(only_user.keep(&user));
        assert!(!only_user.keep(&tool));
    }

    #[test]
    fn join_lines_terminates_and_keeps_empty() {
        assert_eq!(join_lines(vec![]), "");
        assert_eq!(join_lines(vec!["a".into(), "b".into()]), "a\nb\n");
    }

    #[test]
    fn one_line_flattens_whitespace_and_truncates() {
        assert_eq!(one_line("a\n  b\tc", 100), "a b c");
        assert_eq!(one_line("abcdef", 4), "abc…");
        assert_eq!(one_line("abc", 3), "abc");
    }

    #[test]
    fn todo_summary_reports_count_and_active() {
        let none_active = serde_json::json!({"todos": [{"content": "x", "status": "pending"}]});
        assert_eq!(todo_summary(&none_active), "1 item(s)");
        let no_shape = serde_json::json!({"unexpected": true});
        assert!(todo_summary(&no_shape).contains("unexpected"));
    }

    #[test]
    fn message_to_json_is_tagged_by_role() {
        // The CLI's `--json` output is `Message::to_json` per message; check the
        // role-tagged shape the integration test relies on.
        let v = Message::User("hi".into()).to_json();
        assert_eq!(v["role"], "user");
        assert_eq!(v["text"], "hi");
        // A text-only user message omits the images field.
        assert!(v.get("images").is_none());

        let v = Message::PermissionRequest(PermissionRequest::new(
            uuid::Uuid::nil(),
            "Bash".into(),
            serde_json::Value::Null,
            None,
            Some(PermissionResponseType::Allowed),
            None,
        ))
        .to_json();
        assert_eq!(v["role"], "permission_request");
        assert_eq!(v["tool"], "Bash");
        assert_eq!(v["decision"], "allowed");

        let v = tool_result("Read", "154 lines").to_json();
        assert_eq!(v["role"], "tool_result");
        assert_eq!(v["tool"], "Read");
        assert_eq!(v["summary"], "154 lines");
    }

    // -- `agentium spawn`: flag capture, target defaulting, wait/json shape -----

    /// `[7u8; 32]` as an nsec — passed to `Cli::parse` so key resolution is
    /// deterministic (overrides any stored/env key on the test machine).
    const TEST_NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";

    /// Parse a full arg vector (without the program name) through [`Cli::parse`].
    fn parse_cli(args: &[&str]) -> Result<Option<Cli>> {
        Cli::parse(args.iter().map(|s| s.to_string()))
    }

    /// A [`SpawnOpts`] with the target flags set and no title/prompt/wait.
    fn spawn_opts(host: Option<&str>, cwd: Option<&str>, backend: Option<&str>) -> SpawnOpts {
        SpawnOpts {
            host: host.map(str::to_string),
            cwd: cwd.map(str::to_string),
            backend: backend.map(str::to_string),
            title: None,
            prompt: None,
            wait: false,
        }
    }

    #[test]
    fn spawn_captures_flags() {
        // The shared --host/--cwd/--backend plus spawn's own --title/--wait land
        // on the Spawn variant; --json is the global flag.
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "--json",
            "--host",
            "mac",
            "--cwd",
            "/x/y",
            "--backend",
            "codex",
            "--title",
            "My Task",
            "--wait",
            "spawn",
        ])
        .unwrap()
        .unwrap();
        assert!(cli.json);
        match cli.command {
            Command::Spawn {
                host,
                cwd,
                backend,
                title,
                prompt,
                wait,
            } => {
                assert_eq!(host.as_deref(), Some("mac"));
                assert_eq!(cwd.as_deref(), Some("/x/y"));
                assert_eq!(backend.as_deref(), Some("codex"));
                assert_eq!(title.as_deref(), Some("My Task"));
                assert_eq!(prompt, None);
                assert!(wait);
            }
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn spawn_with_no_flags_defaults_everything() {
        // A bare `spawn` captures all-None target flags (defaulted later from the
        // current session) and no title/prompt/wait.
        let cli = parse_cli(&["--nsec", TEST_NSEC, "spawn"]).unwrap().unwrap();
        match cli.command {
            Command::Spawn {
                host,
                cwd,
                backend,
                title,
                prompt,
                wait,
            } => {
                assert!(host.is_none() && cwd.is_none() && backend.is_none());
                assert!(title.is_none() && prompt.is_none() && !wait);
            }
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn spawn_prompt_is_captured() {
        let cli = parse_cli(&["--nsec", TEST_NSEC, "--prompt", "do the thing", "spawn"])
            .unwrap()
            .unwrap();
        match cli.command {
            Command::Spawn { prompt, .. } => assert_eq!(prompt.as_deref(), Some("do the thing")),
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn prompt_implies_wait() {
        // --wait alone waits; --prompt alone also waits (can't send to a session
        // that isn't up); neither means fire-and-forget.
        let mut opts = spawn_opts(Some("h"), Some("/c"), None);
        assert!(!opts.effective_wait());
        opts.wait = true;
        assert!(opts.effective_wait());
        opts.wait = false;
        opts.prompt = Some("hi".into());
        assert!(opts.effective_wait());
    }

    #[test]
    fn merge_spawn_target_flag_wins_over_current() {
        let opts = spawn_opts(Some("flaghost"), Some("/flag"), Some("codex"));
        let t = merge_spawn_target(&opts, Some("curhost"), Some("/cur"), Some("claude")).unwrap();
        assert_eq!(t.host, "flaghost");
        assert_eq!(t.cwd, "/flag");
        assert_eq!(t.backend, "codex");
    }

    #[test]
    fn merge_spawn_target_current_fills_gaps() {
        // No flags → default to the current session's own host/cwd/backend.
        let opts = spawn_opts(None, None, None);
        let t = merge_spawn_target(&opts, Some("curhost"), Some("/cur"), Some("codex")).unwrap();
        assert_eq!(t.host, "curhost");
        assert_eq!(t.cwd, "/cur");
        assert_eq!(t.backend, "codex");
    }

    #[test]
    fn merge_spawn_target_backend_falls_back_to_claude() {
        // Neither a --backend flag nor a current-session backend → "claude".
        let t =
            merge_spawn_target(&spawn_opts(Some("h"), Some("/c"), None), None, None, None).unwrap();
        assert_eq!(t.backend, "claude");
    }

    #[test]
    fn merge_spawn_target_missing_host_or_cwd_errors() {
        // Not inside a session and no flag → nothing to target.
        assert!(merge_spawn_target(&spawn_opts(None, Some("/c"), None), None, None, None).is_err());
        assert!(merge_spawn_target(&spawn_opts(Some("h"), None, None), None, None, None).is_err());
        // An empty current value counts as absent (a session with no recorded
        // host can't seed the default).
        assert!(
            merge_spawn_target(
                &spawn_opts(None, Some("/c"), None),
                Some(""),
                Some("/c"),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn spawn_json_shape_tracks_wait_and_prompt() {
        // Pre-wait: session is null, no event_id key.
        let pre = spawn_json("mac", "spawn-1", None, None);
        assert_eq!(pre["spawn_id"], "spawn-1");
        assert_eq!(pre["host"], "mac");
        assert!(pre["session"].is_null());
        assert!(pre.get("event_id").is_none());

        // Resolved with --prompt: session ref + event_id both present.
        let full = spawn_json("mac", "spawn-1", Some("agentium:a-b-c"), Some("deadbeef"));
        assert_eq!(full["session"], "agentium:a-b-c");
        assert_eq!(full["event_id"], "deadbeef");
    }

    #[test]
    fn short_id_prefixes_to_eight() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }
}
