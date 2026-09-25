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

mod grep;
mod list;
mod log;
mod term;
mod transcript;

use std::env;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_events::{
    SPAWN_DEDUPE_WINDOW_SECS, SpawnOptions, spawn_idempotency_key,
};
use agentium_core::session_loader::SessionState;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use regex::Regex;

use nostrdb_net::relay::sync::Result;

use grep::{CaseMode, cmd_grep, compile_pattern};
use list::{ListFilters, ListScope, SessionJson, cmd_list};
use log::{cmd_follow, cmd_log};
use term::{
    SGR_BOLD, SGR_NEEDS_INPUT, abbreviate_home, col, now_secs, paint, relative_time, status_style,
};
use transcript::{ColorWhen, MessageView, PagerMode};

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/agentium-cli` on Linux).
const APP: &str = "agentium-cli";

/// Hard cap on the settle wait, so a reachable-but-silent relay can't stall the
/// read: past this we give up on the reconcile and read whatever the cache holds.
const SYNC_MAX: Duration = Duration::from_secs(6);

/// Bound on the settle half of the post-publish flush (see [`flush_publish`]).
const PUBLISH_FLUSH: Duration = Duration::from_secs(2);

/// How long [`flush_publish`] lets the session drain an already-handed-off
/// publish before the process exits out from under it.
///
/// Sized from the failure it fixes rather than from a happy-path round trip: on
/// a two-core Linux box `agentium interrupt` lost its event outright in roughly
/// one run in ten, and 500ms closed that to 0 in 60 runs.
const PUBLISH_DRAIN: Duration = Duration::from_millis(500);

/// Flush a just-published event before the process exits.
///
/// Two waits, because one command has to cross two hand-offs and only the first
/// of them is observable.
///
/// [`Engine::wait_for_sync`] is a FIFO barrier, so once it resolves the loop has
/// dequeued our `Publish` and handed the event to the relay pool. That is *not*
/// the same as the event having been sent: `Session::publish` is
/// fire-and-forget, and a publish for a relay whose socket is still opening sits
/// in the pool's pending map. Exiting there drops it with the process — the CLI
/// prints "sent" and the relay never sees the event. (Measured against the
/// relay's own ndb on a Linux reproducer: `relay_notes=0` on a run the CLI had
/// reported success for.)
///
/// Nothing in the transport reports the second hand-off — there is no publish
/// ack or drained-pending signal on `Session` — so the second wait is a bounded
/// drain rather than a barrier. It should become one: the right fix is a
/// publish-completion barrier upstream in nostrdb_net's `Session`
/// (headway:notedeck/physical-pink-universe), at which point this takes a
/// condition to wait on and [`PUBLISH_DRAIN`] goes away.
async fn flush_publish(engine: &agentium_core::Engine) {
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;
    tokio::time::sleep(PUBLISH_DRAIN).await;
}

/// Default bound on `spawn --wait` (see [`cmd_spawn`]): how long to wait for the
/// target host to answer a spawn command with the new session's kind-31988 state.
/// A host that never answers (none running on `target_host`, or one stuck
/// `pending`) can't hang exit past this — the spawn command was still published,
/// so a later `list` finds the session if the host was merely slow.
///
/// Sized to real latency, not the happy path: a host processes the kind-31989 and
/// fans its state back only on an egui frame, so an idle/backgrounded Dave app
/// (which repaints reactively) is regularly slower than the sub-2s local case —
/// answers around ~20s have been observed. 30s covers that with headroom while
/// still bounding a genuinely dead host. Override per-run with `--wait-timeout`
/// or `$AGENTIUM_SPAWN_WAIT` (see [`resolve_spawn_wait`]).
const SPAWN_WAIT_DEFAULT: Duration = Duration::from_secs(30);

/// How long into a `spawn --wait` to print a one-time "still waiting…" note on
/// stderr, so a human isn't left staring at a silent block while a slow host
/// warms up. Stays on stderr so `--json`/scripted stdout is untouched.
const SPAWN_WAIT_PROGRESS: Duration = Duration::from_secs(8);

/// Resolve the `spawn --wait` bound from the `--wait-timeout <secs>` flag, the
/// `$AGENTIUM_SPAWN_WAIT` env var (seconds), else [`SPAWN_WAIT_DEFAULT`]. The
/// flag wins over the env, which wins over the default; a zero or unparseable env
/// value is ignored (falls through to the default) so a stray export can't pin
/// the wait to nothing. Pure over its inputs so the precedence is unit-testable.
fn resolve_spawn_wait(flag_secs: Option<u64>, env_secs: Option<&str>) -> Duration {
    if let Some(secs) = flag_secs.filter(|s| *s > 0) {
        return Duration::from_secs(secs);
    }
    if let Some(secs) = env_secs
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
    {
        return Duration::from_secs(secs);
    }
    SPAWN_WAIT_DEFAULT
}

/// Validate and normalize a `--permission-mode` value.
///
/// Rejected here rather than on the host: an unknown mode should fail the
/// command visibly, before anything is published, instead of arriving as a tag a
/// host quietly ignores while the session comes up in the wrong mode. The
/// canonical spelling it returns is what rides the event, so aliases
/// (`manual`, `acceptEdits`, `accept-edits`) never reach the wire.
fn parse_mode_flag(value: &str) -> Result<String> {
    agentium_core::permission_mode::parse_permission_mode(value)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "unknown permission mode '{value}' — expected one of: {}",
                agentium_core::permission_mode::PERMISSION_MODES.join(", "),
            )
            .into()
        })
}

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
    /// Search message text across *every* session the `list` filters select,
    /// rather than one resolved session. The point is the single sync: a
    /// per-session `agentium log … | grep` shell loop re-opens the cache and
    /// re-reconciles the relay once per session (seconds each), while this reads
    /// the whole corpus from one settled cache and one read transaction.
    ///
    /// The pattern is a [`Regex`], compiled during parsing (with `-i` folded in)
    /// so a bad pattern fails before any relay work; `view` reuses `log`'s
    /// role/tool/last filters to shape *which messages are searched*.
    Grep {
        pattern: Regex,
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
    /// message once the session exists; `--permission-mode` picks the mode the
    /// session's agent starts in. `--allow-duplicate` opts out of both duplicate
    /// defences (the pre-publish guard and the host's idempotency key).
    Spawn {
        host: Option<String>,
        cwd: Option<String>,
        backend: Option<String>,
        title: Option<String>,
        prompt: Option<String>,
        /// Already normalized to a canonical wire spelling by
        /// [`parse_mode_flag`], so an alias can never reach the command event.
        permission_mode: Option<String>,
        idempotency_key: Option<String>,
        allow_duplicate: bool,
        wait: bool,
        wait_timeout: Option<u64>,
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

impl Command {
    /// Whether the command has to reach the relay, i.e. whether `--no-sync` is a
    /// contradiction for it.
    ///
    /// The read commands fold their answer out of the engine's nostrdb cache,
    /// which `open_ndb` alone makes readable (it registers the device key, so
    /// already-cached kind-1080 envelopes are decrypted regardless of any
    /// connection) — so they can legitimately run offline against whatever the
    /// last sync left behind. Everything else either publishes an event or
    /// streams live ones, and would silently do nothing without a connection.
    /// `login`/`logout` never get here (they return before the engine exists).
    fn needs_relay(&self) -> bool {
        match self {
            Command::List | Command::Show { .. } | Command::Grep { .. } => false,
            // A follow is a live stream, so it needs the connection even though
            // its initial tail is a cache read.
            Command::Log { view, .. } => view.follow,
            Command::Resume { .. }
            | Command::Send { .. }
            | Command::Spawn { .. }
            | Command::Interrupt { .. }
            | Command::Login { .. }
            | Command::Logout => true,
        }
    }
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
    //
    // `--no-sync` skips both the connect and the settle: the cache is already
    // readable without them, and the reconcile is what a read actually spends
    // its time on (seconds, against a fraction of a second of folding), so an
    // offline read is the fast path for repeated queries over a corpus that
    // hasn't moved. Parsing already rejected it for the commands that publish or
    // stream (see `Command::needs_relay`).
    if cli.sync {
        engine.connect(&relay)?;

        // Let the initial reconcile finish before we read. `wait_for_sync`
        // resolves deterministically once the PNS history backfill has settled —
        // i.e. every reconciled session-state event is queryable — so a single
        // read afterward sees the whole synced batch, not a race with events
        // still streaming in. Bounded by SYNC_MAX so a reachable-but-silent relay
        // can't stall the read; an empty or unreachable relay settles (or times
        // out) fast and we fall through to whatever the cache already holds.
        let _ = tokio::time::timeout(SYNC_MAX, engine.wait_for_sync()).await;
    }

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
        Command::Grep { pattern, view } => cmd_grep(
            &engine,
            &read_pk,
            &filters,
            cli.list_scope,
            &pattern,
            &view,
            cli.json,
        )?,
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
            permission_mode,
            idempotency_key,
            allow_duplicate,
            wait,
            wait_timeout,
        } => {
            let opts = SpawnOpts {
                host,
                cwd,
                backend,
                title,
                prompt,
                permission_mode,
                idempotency_key,
                allow_duplicate,
                wait,
                wait_timeout,
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
/// Errors early (before publishing) only when nothing matches the selector. A
/// session whose backend never started (empty `cli_session`) or a legacy event
/// (no `cli_session` tag) still resumes: the host reopens a fresh backend or
/// resumes from the d-tag respectively, mirroring the GUI's click-to-reopen.
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

        // Resolve the `claude --resume` id exactly as the GUI's host-side
        // hydrator does (`hydrate_session_from_state` in notedeck_dave): a
        // non-empty `cli_session` is the real CLI id; an empty one means the
        // backend never started, so the host reopens a *fresh* backend (no
        // `--resume`); an absent tag is a legacy event whose d-tag *is* the CLI
        // id. We never bail — the GUI reopens all three, so the CLI must too.
        //
        // This value is advisory: the host's `reopen_session` re-derives the
        // resume id itself from the session's own state and ignores what the
        // resume command carries. We still resolve it faithfully for
        // forward-compat and so `--json`/logs report a sensible id.
        let cli = match state.cli_session_id.as_deref() {
            Some(cli) if !cli.is_empty() => cli.to_string(),
            Some(_) => String::new(),
            None => state.claude_session_id.clone(),
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

    flush_publish(engine).await;

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
    flush_publish(engine).await;

    if as_json {
        let obj = serde_json::json!({ "session": uri, "event_id": event_id });
        println!("{}", json_line(&obj)?);
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
    /// `--prompt`: the session's first `user` message. It rides the spawn command
    /// (as its `prompt` tag) and the host delivers it when the session comes up,
    /// so delivery is independent of `--wait`. Still implies `--wait` so the
    /// resolved `agentium:` ref gets reported when the host answers in time.
    prompt: Option<String>,
    /// `--permission-mode`: the permission mode the new session's agent starts
    /// in, already normalized to a canonical wire spelling. `None` leaves the
    /// host's own default in place.
    permission_mode: Option<String>,
    /// `--idempotency-key`: the caller's own name for this *request*, overriding
    /// the one derived from the request's fields. Useful when the caller has a
    /// better notion of identity than the fields give — retrying "the spawn for
    /// job 4821" should dedupe even if its prompt was reworded between attempts.
    idempotency_key: Option<String>,
    /// `--allow-duplicate`: deliberately spawn a second session that a duplicate
    /// defence would otherwise refuse. Skips the pre-publish guard *and* omits
    /// the idempotency key from the command, so neither this CLI nor the host
    /// treats the spawn as a retry — one flag for "I mean it", rather than a
    /// guard the host would then silently re-impose.
    allow_duplicate: bool,
    /// `--wait`: block (bounded by [`resolve_spawn_wait`]) until the host answers
    /// with the new session's kind-31988 state, then print its durable `agentium:`
    /// ref.
    wait: bool,
    /// `--wait-timeout <secs>`: override the wait bound for this run (else
    /// `$AGENTIUM_SPAWN_WAIT`, else [`SPAWN_WAIT_DEFAULT`]). `None` leaves the
    /// resolution to the env/default; see [`resolve_spawn_wait`].
    wait_timeout: Option<u64>,
}

impl SpawnOpts {
    /// Whether the spawn should wait for the host's answer: the explicit `--wait`,
    /// or implicitly whenever `--prompt` is set. `--prompt` no longer *needs* the
    /// wait for delivery (the host delivers it off the command), but implying it
    /// means the common prompted spawn still reports the resolved `agentium:` ref
    /// when the host answers promptly.
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

/// A live session an about-to-be-published spawn looks like a retry of.
///
/// Matching is on what `list` can actually see — host, cwd, title, age — because
/// the CLI cannot read the host's own idempotency bookkeeping. That makes this a
/// second line of defence rather than the primary one: it still catches the retry
/// against an *older* host that ignores the `idempotency_key` tag entirely.
///
/// A title is required for a match, and is the whole discriminator. Without one
/// the new session's title would be derived from its first message, so there is
/// nothing on either side to compare and every recent sibling in the worktree
/// would look like a duplicate — including legitimate fan-out. Untitled spawns
/// rely on the host's key instead. In practice the titled path is the one that
/// bites: a handoff always names its session, and its prompt is long enough to
/// live in a file, which is exactly the invocation a caller re-runs verbatim.
fn find_duplicate_spawn<'a>(
    live: &'a [SessionState],
    target: &SpawnTarget,
    title: Option<&str>,
    now: u64,
) -> Option<&'a SessionState> {
    let title = title.filter(|t| !t.is_empty())?;
    live.iter().find(|state| {
        state.hostname == target.host
            && state.cwd == target.cwd
            && state.display_title() == title
            // `saturating_sub`: a session stamped slightly ahead of this clock
            // reads as age 0, i.e. inside the window — the safe direction, since
            // it refuses rather than duplicates.
            && now.saturating_sub(state.created_at) <= SPAWN_DEDUPE_WINDOW_SECS
    })
}

/// The refusal a caught duplicate produces: names the session it would have
/// duplicated, and the flag that overrides.
///
/// Built apart from [`cmd_spawn`] so the wording is unit-testable — the message
/// *is* the feature here. A caller that only sees "refused" learns nothing and
/// reaches for the retry again; one that sees the existing ref can check it and
/// move on.
fn duplicate_spawn_error(existing: &SessionState, title: &str, now: u64) -> String {
    format!(
        "{} is already running \"{}\" in {} on {} (started {}s ago) — not spawning a second \
         agent in the same worktree.\n  follow it:  agentium log {} -f\n  list them:  agentium \
         list --cwd {}\n  really want another: re-run with --allow-duplicate",
        existing.agentium_uri(),
        title,
        existing.cwd,
        existing.hostname,
        now.saturating_sub(existing.created_at),
        existing.agentium_uri(),
        existing.cwd,
    )
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
/// [`resolve_spawn_wait`]. That row is the new session; its `agentium_uri` is the ref we
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

    // Refuse an obvious retry *before* publishing anything. Cheaper and clearer
    // than letting the host dedupe it — the caller gets the existing ref instead
    // of a second `agentium:` ref it has to reconcile — and it is the only defence
    // that works against an older host that ignores the idempotency key.
    if !opts.allow_duplicate {
        let now = agentium_core::session_events::now_secs();
        let txn = Transaction::new(engine.ndb())?;
        let live = load_session_states_for_author(engine.ndb(), &txn, author);
        if let Some(existing) = find_duplicate_spawn(&live, &target, opts.title.as_deref(), now) {
            // `title` is `Some` and non-empty whenever a match was found.
            let title = opts.title.as_deref().unwrap_or_default();
            return Err(duplicate_spawn_error(existing, title, now).into());
        }
    }

    // The request's identity, so a host recognizes a retry that slips past the
    // guard above (an untitled spawn, or one whose earlier session has already
    // aged out of the recent window) as the same spawn rather than a second one.
    // `--allow-duplicate` omits it: opting out of the guard has to opt out of the
    // host's dedupe too, or the host would just re-impose it.
    let idempotency_key = (!opts.allow_duplicate).then(|| {
        opts.idempotency_key.clone().unwrap_or_else(|| {
            spawn_idempotency_key(
                &target.host,
                &target.cwd,
                &target.backend,
                &SpawnOptions {
                    title: opts.title.as_deref(),
                    prompt: opts.prompt.as_deref(),
                    permission_mode: opts.permission_mode.as_deref(),
                    idempotency_key: None,
                },
            )
        })
    });

    // `--wait` (also implied by `--prompt`) blocks to *report* the resolved
    // `agentium:` ref. Delivery no longer needs it — a `--prompt` rides the
    // command and the host delivers it — so a slow host is not a failure here.
    let wait = opts.effective_wait();

    // Install the session-list watch *before* publishing so the host's kind-31988
    // answer can't land between the publish and our first read (only when waiting).
    let mut watch = if wait {
        Some(engine.watch_sessions()?)
    } else {
        None
    };

    // The first message rides the command as a `prompt` tag: the host delivers it
    // when it materializes the session, so it lands even if this CLI stops waiting
    // before the host answers. The permission mode rides it for a sharper reason —
    // the host has to know it *before* it starts the session's backend, which it
    // does the moment it delivers that first message.
    let spawn_id = engine.spawn_session(
        &target.host,
        &target.cwd,
        &target.backend,
        &SpawnOptions {
            title: opts.title.as_deref(),
            prompt: opts.prompt.as_deref(),
            permission_mode: opts.permission_mode.as_deref(),
            idempotency_key: idempotency_key.as_deref(),
        },
    )?;

    flush_publish(engine).await;

    // No `--wait`: only the spawn_id is known — report it and return.
    let Some(watch) = watch.as_mut() else {
        emit_spawn(&target.host, &spawn_id, None, as_json)?;
        return Ok(());
    };

    // Resolve the wait bound: `--wait-timeout` flag, else `$AGENTIUM_SPAWN_WAIT`,
    // else the default. A slow-but-alive host regularly needs more than the old
    // fixed 8s, so this is sized to real latency and tunable per run.
    let wait_bound = resolve_spawn_wait(
        opts.wait_timeout,
        std::env::var("AGENTIUM_SPAWN_WAIT").ok().as_deref(),
    );

    // Wait (bounded) for a kind-31988 state carrying our spawn_id. `check, then
    // wait`: re-read the list, match the spawn_id, else block on the watch. A
    // one-shot timer surfaces a "still waiting…" note on stderr partway through,
    // so a slow host's warmup doesn't look like a silent hang; the note stays off
    // stdout so `--json`/scripted output is untouched.
    let resolved = tokio::time::timeout(wait_bound, async {
        let progress = tokio::time::sleep(SPAWN_WAIT_PROGRESS);
        tokio::pin!(progress);
        let mut noted = false;
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
            tokio::select! {
                // First crossing of the progress threshold: reassure once, then
                // fall back to re-check + wait (the branch is disabled afterwards).
                _ = &mut progress, if !noted => {
                    noted = true;
                    eprintln!(
                        "still waiting for {} to answer (spawn {}…)",
                        target.host,
                        short_id(&spawn_id),
                    );
                }
                // Watch ending (db torn down) resolves the wait with nothing found.
                changed = watch.changed() => {
                    if !changed {
                        return Ok(None);
                    }
                }
            }
        }
    })
    .await;

    let state = match resolved {
        Ok(Ok(Some(state))) => state,
        // The read/loop itself errored — surface it.
        Ok(Err(e)) => return Err(e),
        // Timed out, or the watch ended before an answer arrived. When a `--prompt`
        // rode the command the host will still deliver it whenever it processes the
        // spawn, so a slow host is not a failure: report the spawn_id (no ref yet)
        // and exit cleanly. A bare `--wait` had no other purpose than the ref, so it
        // stays an error.
        Ok(Ok(None)) | Err(_) => {
            let short = short_id(&spawn_id);
            if opts.prompt.is_some() {
                emit_spawn(&target.host, &spawn_id, None, as_json)?;
                return Ok(());
            }
            return Err(format!(
                "no host answered on {} within {}s (spawn {short}…) — the command was \
                 published, so `agentium list` will find the session if the host was just slow \
                 (raise the bound with --wait-timeout <secs> or $AGENTIUM_SPAWN_WAIT)",
                target.host,
                wait_bound.as_secs(),
            )
            .into());
        }
    };

    let uri = state.agentium_uri();
    emit_spawn(&target.host, &spawn_id, Some(&uri), as_json)?;
    Ok(())
}

/// Render a single-record `--json` payload as exactly one line.
///
/// `spawn` and `send` each emit one object describing one action, so the
/// line-oriented shell idioms apply to them — and pretty-printing broke every
/// one of those idioms silently. `agentium spawn --json | tail -1 | jq -r
/// .session` (or `head -1`, or `read -r`) yielded an empty string against a
/// multi-line object, which reads as a failed spawn: the caller re-runs, and now
/// there are two agents in the worktree. That is the concrete path a real
/// duplicate took, so the compact form is part of the fix and not a cosmetic
/// change.
///
/// `list`/`show`/`log --json` stay pretty-printed. Those are whole documents
/// (an array, a nested object) that were never line-parseable in either form,
/// so compacting them would trade readability for nothing.
fn json_line(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

/// A short (8-char) prefix of an id, for the scannable one-line spawn report.
fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The `spawn --json` object: `{ spawn_id, host, session }`, emitted on one line
/// by [`json_line`]. `session` is `null`
/// until `--wait` resolves the new session's `agentium:` ref (and stays `null`
/// when a slow host times out the wait — the spawn, and any `--prompt`, is still
/// delivered). Built here (rather than inline in [`emit_spawn`]) so the shape is
/// unit-testable.
fn spawn_json(host: &str, spawn_id: &str, session: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "spawn_id": spawn_id, "host": host, "session": session })
}

/// Report a spawn's result. Plain text mirrors [`cmd_send`]'s style; `--json`
/// emits `{ spawn_id, host, session }` — `session` is the sayable `agentium:` ref,
/// `null` until `--wait` resolves it. Any `--prompt` is delivered by the host off
/// the spawn command, so it is not reported here.
fn emit_spawn(host: &str, spawn_id: &str, session: Option<&str>, as_json: bool) -> Result<()> {
    if as_json {
        let obj = spawn_json(host, spawn_id, session);
        println!("{}", json_line(&obj)?);
        return Ok(());
    }

    let short = short_id(spawn_id);
    match session {
        None => println!("spawn command sent to {host} (spawn {short}…)"),
        Some(uri) => println!("spawned {uri} on {host} (spawn {short}…)"),
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
    flush_publish(engine).await;

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

/// Read a `--prompt-file` value into the prompt text: `-` reads stdin (so a
/// handoff can heredoc a multi-line prompt with no shell-escaping), anything else
/// is a file path. Trailing whitespace is trimmed so a heredoc's closing newline
/// doesn't ride along.
fn read_prompt_source(path: &str) -> Result<String> {
    use std::io::Read;
    let mut text = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(path)?;
    }
    Ok(text.trim_end().to_string())
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
    /// Whether to reconcile with the relay before reading (`--no-sync` clears
    /// it). Off, the read folds whatever the cache already holds — which is the
    /// whole of a corpus that hasn't moved since the last run, and skips the
    /// seconds a reconcile costs. Parsing rejects it for commands that publish
    /// or stream (see [`Command::needs_relay`]).
    sync: bool,
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
        let mut no_sync = false;
        // `grep`'s case mode. Folded into the compiled pattern below rather than
        // carried separately, so nothing downstream has to remember it.
        let mut case = CaseMode::Smart;
        // `log` transcript flags. `show_tools` defaults off, so the rendered
        // transcript is the human conversation; `--tools` opts tool traffic back
        // in and `--no-tools` re-asserts the default. Color/pager default to
        // `Auto` (tty detection), like `git log`.
        let mut roles: Vec<String> = Vec::new();
        let mut last = None;
        let mut show_tools = false;
        let mut jsonl = false;
        let mut color = ColorWhen::Auto;
        let mut pager = PagerMode::Auto;
        let mut follow = false;
        // `spawn` flags. `--title`/`--prompt` are values; `--wait` is a switch
        // (also implied by `--prompt`, resolved in `cmd_spawn`). `--prompt-file`
        // is the escaping-free alternative to `--prompt`: a path (or `-` for
        // stdin) whose contents become the prompt, resolved once the loop ends.
        let mut title = None;
        let mut prompt = None;
        let mut prompt_file = None;
        let mut permission_mode = None;
        let mut idempotency_key = None;
        let mut allow_duplicate = false;
        let mut wait = false;
        let mut wait_timeout = None;
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
                "--no-sync" => no_sync = true,
                "-i" | "--ignore-case" => case = CaseMode::Insensitive,
                "-s" | "--case-sensitive" => case = CaseMode::Sensitive,
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
                "--prompt-file" => prompt_file = Some(value("--prompt-file")?),
                "--permission-mode" => {
                    permission_mode = Some(parse_mode_flag(&value("--permission-mode")?)?)
                }
                "--idempotency-key" => idempotency_key = Some(value("--idempotency-key")?),
                "--allow-duplicate" => allow_duplicate = true,
                "--wait" => wait = true,
                "--wait-timeout" => {
                    wait_timeout = Some(
                        value("--wait-timeout")?
                            .parse::<u64>()
                            .map_err(|_| "--wait-timeout needs a non-negative integer (seconds)")?,
                    )
                }
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
        // Fold `--prompt-file` into `prompt`: the two name the same thing (the
        // first message), so passing both is a contradiction. A file value of `-`
        // means stdin, which is what lets a handoff pipe a multi-line prompt in
        // via a heredoc without shell-escaping.
        let prompt = match (prompt, prompt_file) {
            (Some(_), Some(_)) => {
                return Err("pass either --prompt or --prompt-file, not both".into());
            }
            (Some(text), None) => Some(text),
            (None, Some(path)) => Some(read_prompt_source(&path)?),
            (None, None) => None,
        };

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
                permission_mode,
                idempotency_key,
                allow_duplicate,
                wait,
                wait_timeout,
            }
        } else {
            parse_command(name, rest, view, case)?
        };

        // `--no-sync` is a read-only shortcut; a command that publishes or
        // streams can't honor it. Rejected here, before any engine or relay work
        // spins up, for the same reason `check_follow` is.
        if no_sync && command.needs_relay() {
            return Err(
                "--no-sync only applies to cache reads (list/show/log/grep); a command that \
                 publishes — or `log --follow`, which streams — needs the relay"
                    .into(),
            );
        }

        // `login`/`logout` manage the stored key themselves, so don't parse (and
        // potentially reject on) whatever key is currently configured.
        // `parse_nsec` hands back a `nostrdb_net::Pubkey`; the rest of the CLI
        // (and the `agentium_core` engine) speaks `nostrdb_net::Pubkey`. Both are
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
        let sync = !no_sync;

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
            sync,
            command,
        }))
    }
}

fn parse_command(
    name: &str,
    rest: &[String],
    view: MessageView,
    case: CaseMode,
) -> Result<Command> {
    Ok(match name {
        "list" => Command::List,
        "show" => Command::Show {
            session: optional_session(rest),
        },
        "log" => Command::Log {
            session: optional_session(rest),
            view,
        },
        "grep" => Command::Grep {
            pattern: compile_pattern(&arg(rest, 0, name)?, case)?,
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
                      Tool call/result noise is folded away by default; pass
                      --tools to include it. Filter/shape with --role/--last;
                      --json
                      emits structured message objects, --jsonl the reconstructed
                      claude-code JSONL from the source archive. --follow/-f keeps
                      streaming new messages (and status changes) until Ctrl-C.
    grep <pattern>    Search message text across every session the list filters
                      select (--host/--cwd/--status/--backend/--deleted/--all),
                      printing each matching line under its session's agentium:
                      ref. <pattern> is a regex; -i folds case. One sync and one
                      cache read covers every session, so it is flat in session
                      count where a per-session `log | grep` loop is not. The
                      log filters --role/--tools/--last narrow what is
                      searched (tool noise is folded away by default); --json
                      groups matches under each session.
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
                      --prompt <text> rides the command so the host delivers it as
                      the session's first message (delivery no longer depends on
                      --wait, so a slow host still gets the prompt);
                      --permission-mode picks the mode its agent starts in. --json emits
                      {{ spawn_id, host, session }} on one line. A spawn that looks
                      like a retry of a recent one (same host+cwd+title) is refused,
                      naming the session it would have duplicated; the host also
                      answers a retried spawn with the session it already made, so
                      re-running after a timeout is safe. --allow-duplicate opts out.
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
    --no-sync         Skip the relay reconcile and read the local cache as it
                      stands — the fast path for repeated reads (list/show/log/
                      grep). Rejected for commands that publish or stream.

  list filters (case-insensitive):
    --host <h>        Only sessions whose host contains <h>
    --status <s>      Only sessions with exactly this status
                      (idle|working|needs_input|error|done|pending)
    --cwd <c>         Only sessions whose working dir contains <c>
    --backend <b>     Only sessions whose backend contains <b>
    --deleted         Show only soft-deleted (tombstoned) sessions
    --all             Show live and deleted sessions together

  grep options (also uses the list filters above to pick sessions, and the log
  options below to pick which messages are searched):
                      Case is smart by default: an all-lowercase pattern matches
                      case-insensitively, one carrying an uppercase letter
                      matches exactly.
    -i, --ignore-case Force a case-insensitive match
    -s, --case-sensitive
                      Force a case-sensitive match

  log options:
    --role <r[,r…]>   Only messages with these roles, comma-separated and/or
                      repeatable (user|assistant|tool_call|tool_result|
                      permission_request|subagent|system|error|compaction|todo)
    -n, --last <n>    Only the last <n> messages (after other filters)
    --tools           Also show tool_call/tool_result messages
    --no-tools        Fold away tool_call/tool_result noise (default)
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
    --wait-timeout <secs>
                      Override the --wait bound for this run (default 30s, else
                      $AGENTIUM_SPAWN_WAIT). A still-waiting note hits stderr
                      after 8s so a slow host doesn't look like a hang.
    --prompt <text>   The session's first user message. It rides the spawn command
                      and the host delivers it when the session comes up, so it
                      lands even if a slow host outlasts --wait (which --prompt
                      still implies, only to report the ref)
    --prompt-file <p> Like --prompt, but read the message from file <p> (or stdin
                      when <p> is `-`) — pass a long/multi-line prompt with no
                      shell-escaping. Mutually exclusive with --prompt.
    --permission-mode <m>
                      The mode the new session's agent starts in, rather than the
                      host's default: default (aka manual) | plan | accept_edits |
                      auto | bypass. Asking for it in --prompt does NOT work — the
                      backend has already started by the time it reads that
                      message. bypass does no safety checking at all.
    --idempotency-key <k>
                      Name this *request*, so a retry of it is recognized as the
                      same spawn. Defaults to a digest of the request itself
                      (host+cwd+backend+title+prompt+mode), which already makes an
                      identical re-run safe; pass your own when you have a better
                      notion of identity (a job id, say).
    --allow-duplicate Really spawn a second session the duplicate guard would
                      refuse. Also drops the idempotency key, so the host doesn't
                      re-impose the dedupe.

    -h, --help        Print this help",
        DEFAULT_RELAY = nostrdb_net::relay::sync::DEFAULT_RELAY,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::tests::session;
    use crate::transcript::tests::view_all;
    use agentium_core::config::RunConfig;
    use agentium_core::messages::UsageInfo;

    #[test]
    fn show_selector_prefers_explicit_arg() {
        // An explicit positional is used verbatim (the $AGENTIUM_SESSION
        // fallback only applies when none is given — exercised end-to-end, not
        // here, to avoid mutating process env in a shared test binary).
        match parse_command(
            "show",
            &["agentium:a-b-c".to_string()],
            view_all(),
            CaseMode::Smart,
        )
        .unwrap()
        {
            Command::Show { session } => assert_eq!(session.as_deref(), Some("agentium:a-b-c")),
            _ => panic!("expected Show"),
        }
    }

    #[test]
    fn log_command_carries_selector_and_view() {
        let view = MessageView {
            roles: vec!["assistant".into()],
            last: Some(5),
            show_tools: true,
            jsonl: true,
            ..view_all()
        };
        match parse_command(
            "log",
            &["agentium:a-b-c".to_string()],
            view,
            CaseMode::Smart,
        )
        .unwrap()
        {
            Command::Log { session, view } => {
                assert_eq!(session.as_deref(), Some("agentium:a-b-c"));
                assert_eq!(view.roles, vec!["assistant".to_string()]);
                assert_eq!(view.last, Some(5));
                assert!(view.show_tools);
                assert!(view.jsonl);
            }
            _ => panic!("expected Log"),
        }
    }

    /// The compiled regex behind `grep <pattern>` under `case`.
    fn grep_pattern(pattern: &str, case: CaseMode) -> Regex {
        let rest = [pattern.to_string()];
        match parse_command("grep", &rest, view_all(), case).unwrap() {
            Command::Grep { pattern, .. } => pattern,
            _ => panic!("expected Grep"),
        }
    }

    #[test]
    fn grep_is_smart_case_by_default() {
        // An all-lowercase pattern reads as "I don't care": the `hyrule` that
        // sent jb55 looking, against the `Hyrule` the transcripts actually
        // spell.
        let loose = grep_pattern("hyrule", CaseMode::Smart);
        assert!(loose.is_match("Hyrule Field"));
        assert!(loose.is_match("hyrule"));

        // Spelling the case *is* the ask, so it's honoured exactly.
        let exact = grep_pattern("Term.*ux", CaseMode::Smart);
        assert!(exact.is_match("Terminal ux"));
        assert!(!exact.is_match("terminal ux"));
    }

    #[test]
    fn grep_case_flags_override_the_smart_default() {
        // `-i` loosens a pattern smart-case would have pinned...
        assert!(grep_pattern("Term.*ux", CaseMode::Insensitive).is_match("terminal ux"));
        // ...and `-s` pins one it would have loosened.
        assert!(!grep_pattern("hyrule", CaseMode::Sensitive).is_match("Hyrule"));
    }

    #[test]
    fn grep_rejects_a_missing_or_unparseable_pattern() {
        assert!(parse_command("grep", &[], view_all(), CaseMode::Smart).is_err());
        let bad = ["[unclosed".to_string()];
        assert!(parse_command("grep", &bad, view_all(), CaseMode::Smart).is_err());
    }

    #[test]
    fn grep_carries_the_message_view() {
        // `--role`/`--tools`/`--last` shape *what is searched*, so the same view
        // `log` builds rides the grep command. Both axes carry a non-default
        // value, so carriage is what's actually being asserted.
        let view = MessageView {
            roles: vec!["assistant".into()],
            show_tools: true,
            ..view_all()
        };
        match parse_command("grep", &["x".to_string()], view, CaseMode::Smart).unwrap() {
            Command::Grep { view, .. } => {
                assert_eq!(view.roles, vec!["assistant".to_string()]);
                assert!(view.show_tools);
            }
            _ => panic!("expected Grep"),
        }
    }

    /// The parsed `MessageView` behind a `log`/`grep` argv — the flag defaults
    /// as a real invocation sees them, not as a test literal builds them.
    fn parsed_view(argv: &[&str]) -> MessageView {
        match parse_cli(argv).expect("parses").expect("a command").command {
            Command::Log { view, .. } | Command::Grep { view, .. } => view,
            _ => panic!("expected Log or Grep"),
        }
    }

    #[test]
    fn show_tools_defaults_off_and_tools_opts_back_in() {
        // A bare `log`/`grep` reads as the human conversation.
        assert!(!parsed_view(&["--nsec", TEST_NSEC, "log", "agentium:a-b-c"]).show_tools);
        assert!(!parsed_view(&["--nsec", TEST_NSEC, "grep", "x"]).show_tools);
        // `--tools` opts the tool traffic back in; `--no-tools` says the default
        // out loud; last flag wins either way.
        assert!(parsed_view(&["--nsec", TEST_NSEC, "--tools", "log", "agentium:a-b-c"]).show_tools);
        assert!(
            !parsed_view(&["--nsec", TEST_NSEC, "--no-tools", "log", "agentium:a-b-c"]).show_tools
        );
        assert!(
            !parsed_view(&[
                "--nsec",
                TEST_NSEC,
                "--tools",
                "--no-tools",
                "log",
                "agentium:a-b-c"
            ])
            .show_tools
        );
    }

    #[test]
    fn no_sync_is_accepted_for_reads_and_refused_for_the_rest() {
        // Reads fold out of the cache, so they can run without a reconcile.
        for cmd in [
            vec!["--nsec", TEST_NSEC, "--no-sync", "list"],
            vec!["--nsec", TEST_NSEC, "--no-sync", "show", "agentium:a-b-c"],
            vec!["--nsec", TEST_NSEC, "--no-sync", "log", "agentium:a-b-c"],
            vec!["--nsec", TEST_NSEC, "--no-sync", "grep", "x"],
        ] {
            let cli = parse_cli(&cmd).expect("parses").expect("a command");
            assert!(!cli.sync, "--no-sync clears the reconcile for {cmd:?}");
        }
        // Publishing and streaming commands would silently do nothing offline.
        for cmd in [
            vec![
                "--nsec",
                TEST_NSEC,
                "--no-sync",
                "send",
                "agentium:a-b-c",
                "hi",
            ],
            vec![
                "--nsec",
                TEST_NSEC,
                "--no-sync",
                "interrupt",
                "agentium:a-b-c",
            ],
            vec!["--nsec", TEST_NSEC, "--no-sync", "resume", "agentium:a-b-c"],
            vec!["--nsec", TEST_NSEC, "--no-sync", "spawn"],
            vec![
                "--nsec",
                TEST_NSEC,
                "--no-sync",
                "-f",
                "log",
                "agentium:a-b-c",
            ],
        ] {
            assert!(
                parse_cli(&cmd).is_err(),
                "--no-sync must be refused for {cmd:?}"
            );
        }
        // Without the flag, every command still reconciles.
        let cli = parse_cli(&["--nsec", TEST_NSEC, "list"])
            .expect("parses")
            .expect("a command");
        assert!(cli.sync);
    }

    #[test]
    fn send_requires_session_and_joins_text() {
        // `send <sel> hey there` → the first positional is the selector, the rest
        // join into the message with single spaces (no quoting needed).
        let rest = ["agentium:a-b-c", "hey", "there"].map(String::from);
        match parse_command("send", &rest, view_all(), CaseMode::Smart).unwrap() {
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
        assert!(parse_command("send", &[], view_all(), CaseMode::Smart).is_err());
        // A selector but no message words → the empty-message error.
        let one = ["agentium:a-b-c"].map(String::from);
        assert!(parse_command("send", &one, view_all(), CaseMode::Smart).is_err());
        // A selector plus a whitespace-only quoted arg is also rejected.
        let blank = ["agentium:a-b-c", "   "].map(String::from);
        assert!(parse_command("send", &blank, view_all(), CaseMode::Smart).is_err());
    }

    #[test]
    fn interrupt_requires_session() {
        // `interrupt <sel>` carries just the selector; trailing words are ignored.
        let rest = ["agentium:a-b-c", "extra"].map(String::from);
        match parse_command("interrupt", &rest, view_all(), CaseMode::Smart).unwrap() {
            Command::Interrupt { session } => assert_eq!(session, "agentium:a-b-c"),
            _ => panic!("expected Interrupt"),
        }
        // No selector → the missing-argument error (no $AGENTIUM_SESSION default).
        assert!(parse_command("interrupt", &[], view_all(), CaseMode::Smart).is_err());
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

    // -- `agentium spawn`: flag capture, target defaulting, wait/json shape -----

    /// `[7u8; 32]` as an nsec — passed to `Cli::parse` so key resolution is
    /// deterministic (overrides any stored/env key on the test machine).
    const TEST_NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";

    /// Parse a full arg vector (without the program name) through [`Cli::parse`].
    fn parse_cli(args: &[&str]) -> Result<Option<Cli>> {
        Cli::parse(args.iter().map(|s| s.to_string()))
    }

    /// A [`SpawnOpts`] with the target flags set and no title/prompt/mode/wait.
    fn spawn_opts(host: Option<&str>, cwd: Option<&str>, backend: Option<&str>) -> SpawnOpts {
        SpawnOpts {
            host: host.map(str::to_string),
            cwd: cwd.map(str::to_string),
            backend: backend.map(str::to_string),
            title: None,
            prompt: None,
            permission_mode: None,
            idempotency_key: None,
            allow_duplicate: false,
            wait: false,
            wait_timeout: None,
        }
    }

    /// The pre-publish guard. Each case is a way it could refuse the wrong thing
    /// or wave through the duplicate it exists to catch.
    #[test]
    fn find_duplicate_spawn_matches_only_a_recent_same_target_same_title() {
        const NOW: u64 = 1_800_000_000;
        let target = SpawnTarget {
            host: "mbp".to_string(),
            cwd: "/home/u/proj".to_string(),
            backend: "claude".to_string(),
        };
        // `session()` puts every row in /home/u/proj, which is the target's cwd.
        let live = vec![session("mbp", "Fix the parser", "working", NOW - 30)];

        // The duplicate this exists to catch: same host, cwd and title, 30s ago.
        assert!(
            find_duplicate_spawn(&live, &target, Some("Fix the parser"), NOW).is_some(),
            "a titled re-run against a live session must be refused",
        );

        // Different title — a second, genuinely different task in this worktree.
        assert!(find_duplicate_spawn(&live, &target, Some("Wire the widget"), NOW).is_none());

        // No title: nothing to compare, so the guard must stand down rather than
        // refuse every recent sibling in the worktree.
        assert!(find_duplicate_spawn(&live, &target, None, NOW).is_none());
        assert!(find_duplicate_spawn(&live, &target, Some(""), NOW).is_none());

        // ...and "no title" must not degenerate into "the empty title", which
        // would match a session that happens to have one and refuse an untitled
        // spawn on a coincidence.
        let untitled = vec![session("mbp", "", "working", NOW - 30)];
        assert_eq!(untitled[0].display_title(), "");
        assert!(find_duplicate_spawn(&untitled, &target, None, NOW).is_none());
        assert!(find_duplicate_spawn(&untitled, &target, Some(""), NOW).is_none());

        // Another host, and another worktree, are not duplicates.
        let other_host = SpawnTarget {
            host: "studio".to_string(),
            cwd: target.cwd.clone(),
            backend: target.backend.clone(),
        };
        assert!(find_duplicate_spawn(&live, &other_host, Some("Fix the parser"), NOW).is_none());
        let other_cwd = SpawnTarget {
            host: target.host.clone(),
            cwd: "/home/u/other".to_string(),
            backend: target.backend.clone(),
        };
        assert!(find_duplicate_spawn(&live, &other_cwd, Some("Fix the parser"), NOW).is_none());

        // Past the window the same title is the same *task* asked for again
        // later, which is new work.
        let stale = vec![session(
            "mbp",
            "Fix the parser",
            "working",
            NOW - SPAWN_DEDUPE_WINDOW_SECS - 1,
        )];
        assert!(find_duplicate_spawn(&stale, &target, Some("Fix the parser"), NOW).is_none());
        // ...but exactly at the window it is still a duplicate.
        let edge = vec![session(
            "mbp",
            "Fix the parser",
            "working",
            NOW - SPAWN_DEDUPE_WINDOW_SECS,
        )];
        assert!(find_duplicate_spawn(&edge, &target, Some("Fix the parser"), NOW).is_some());
    }

    /// A `--title` lands in `custom_title`, so the guard has to compare against
    /// the *displayed* title — not the derived one, which churns with the first
    /// message and would never match what the caller passed.
    #[test]
    fn find_duplicate_spawn_compares_the_displayed_title() {
        const NOW: u64 = 1_800_000_000;
        let target = SpawnTarget {
            host: "mbp".to_string(),
            cwd: "/home/u/proj".to_string(),
            backend: "claude".to_string(),
        };
        let mut renamed = session("mbp", "read crates/foo and fix…", "working", NOW - 30);
        renamed.custom_title = Some("Fix the parser".to_string());
        let live = vec![renamed];

        assert!(
            find_duplicate_spawn(&live, &target, Some("Fix the parser"), NOW).is_some(),
            "the sticky title the spawner set is what a retry would pass again",
        );
        assert!(
            find_duplicate_spawn(&live, &target, Some("read crates/foo and fix…"), NOW).is_none()
        );
    }

    /// The refusal has to leave the caller somewhere to go, or they reach for the
    /// retry again — which is how the duplicate happened in the first place.
    #[test]
    fn duplicate_spawn_error_names_the_session_and_the_override() {
        const NOW: u64 = 1_800_000_000;
        let existing = session("mbp", "Fix the parser", "working", NOW - 42);
        let message = duplicate_spawn_error(&existing, "Fix the parser", NOW);

        assert!(
            message.contains(&existing.agentium_uri()),
            "must name the session it would have duplicated: {message}",
        );
        assert!(message.contains("Fix the parser"), "message: {message}");
        assert!(message.contains("/home/u/proj"), "message: {message}");
        assert!(message.contains("42s ago"), "message: {message}");
        assert!(
            message.contains("--allow-duplicate"),
            "must name the override, or a caller who really wants two is stuck: {message}",
        );
    }

    #[test]
    fn spawn_captures_duplicate_flags() {
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "--idempotency-key",
            "job-4821",
            "--allow-duplicate",
            "spawn",
        ])
        .unwrap()
        .unwrap();
        match cli.command {
            Command::Spawn {
                idempotency_key,
                allow_duplicate,
                ..
            } => {
                assert_eq!(idempotency_key.as_deref(), Some("job-4821"));
                assert!(allow_duplicate);
            }
            _ => panic!("expected Spawn"),
        }

        // Neither flag set is the default: derive the key, and keep the guard on.
        let bare = parse_cli(&["--nsec", TEST_NSEC, "spawn"]).unwrap().unwrap();
        match bare.command {
            Command::Spawn {
                idempotency_key,
                allow_duplicate,
                ..
            } => {
                assert_eq!(idempotency_key, None);
                assert!(!allow_duplicate);
            }
            _ => panic!("expected Spawn"),
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
            "--permission-mode",
            "plan",
            "--wait",
            "--wait-timeout",
            "45",
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
                permission_mode,
                wait,
                wait_timeout,
                ..
            } => {
                assert_eq!(host.as_deref(), Some("mac"));
                assert_eq!(cwd.as_deref(), Some("/x/y"));
                assert_eq!(backend.as_deref(), Some("codex"));
                assert_eq!(title.as_deref(), Some("My Task"));
                assert_eq!(prompt, None);
                assert_eq!(permission_mode.as_deref(), Some("plan"));
                assert!(wait);
                assert_eq!(wait_timeout, Some(45));
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
                permission_mode,
                wait,
                wait_timeout,
                ..
            } => {
                assert!(host.is_none() && cwd.is_none() && backend.is_none());
                assert!(title.is_none() && prompt.is_none() && !wait);
                // No mode asked for → nothing rides the command, and the host
                // keeps whatever default it already applies to a new session.
                assert!(permission_mode.is_none());
                assert!(wait_timeout.is_none());
            }
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn spawn_permission_mode_normalizes_aliases() {
        // Whatever spelling a human reaches for, the canonical wire string is
        // what's captured — so an alias can never reach the command event.
        for (typed, canonical) in [
            ("plan", "plan"),
            ("manual", "default"),
            ("acceptEdits", "accept_edits"),
            ("accept-edits", "accept_edits"),
            ("bypassPermissions", "bypass"),
        ] {
            let cli = parse_cli(&["--nsec", TEST_NSEC, "--permission-mode", typed, "spawn"])
                .unwrap()
                .unwrap();
            match cli.command {
                Command::Spawn {
                    permission_mode, ..
                } => assert_eq!(
                    permission_mode.as_deref(),
                    Some(canonical),
                    "'{typed}' should normalize to '{canonical}'"
                ),
                _ => panic!("expected Spawn"),
            }
        }
    }

    #[test]
    fn spawn_rejects_an_unknown_permission_mode() {
        // Rejected at parse time, before anything is published: a bad mode that
        // reached the wire would be dropped by the host and the session would
        // come up in the default mode with no sign anything went wrong.
        let Err(err) = parse_cli(&["--nsec", TEST_NSEC, "--permission-mode", "yolo", "spawn"])
        else {
            panic!("an unknown mode must be rejected");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("yolo"),
            "error should name the bad mode: {msg}"
        );
        // ...and say what the valid ones are.
        assert!(msg.contains("plan"), "error should list the modes: {msg}");
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
    fn spawn_prompt_file_is_read_and_trimmed() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "do the thing\nwith newlines\n\n").unwrap();
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "--prompt-file",
            f.path().to_str().unwrap(),
            "spawn",
        ])
        .unwrap()
        .unwrap();
        match cli.command {
            // The file's trailing newlines are trimmed; interior ones survive.
            Command::Spawn { prompt, .. } => {
                assert_eq!(prompt.as_deref(), Some("do the thing\nwith newlines"))
            }
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn spawn_wait_timeout_rejects_non_integer() {
        let err = match parse_cli(&["--nsec", TEST_NSEC, "--wait-timeout", "soon", "spawn"]) {
            Err(e) => e,
            Ok(_) => panic!("--wait-timeout should reject a non-integer"),
        };
        assert!(err.to_string().contains("--wait-timeout"), "{err}");
    }

    #[test]
    fn resolve_spawn_wait_precedence() {
        // Flag wins over env wins over default.
        assert_eq!(
            resolve_spawn_wait(Some(45), Some("20")),
            Duration::from_secs(45),
        );
        assert_eq!(
            resolve_spawn_wait(None, Some("20")),
            Duration::from_secs(20),
        );
        assert_eq!(resolve_spawn_wait(None, None), SPAWN_WAIT_DEFAULT);
        // A zero or garbage value at either level falls through, never pinning the
        // wait to nothing.
        assert_eq!(
            resolve_spawn_wait(Some(0), Some("20")),
            Duration::from_secs(20)
        );
        assert_eq!(resolve_spawn_wait(None, Some("0")), SPAWN_WAIT_DEFAULT);
        assert_eq!(resolve_spawn_wait(None, Some("nope")), SPAWN_WAIT_DEFAULT);
    }

    #[test]
    fn spawn_prompt_and_prompt_file_conflict() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "from file").unwrap();
        let res = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "--prompt",
            "inline",
            "--prompt-file",
            f.path().to_str().unwrap(),
            "spawn",
        ]);
        let err = match res {
            Err(e) => e,
            Ok(_) => panic!("--prompt + --prompt-file should conflict"),
        };
        assert!(err.to_string().contains("not both"), "{err}");
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
    fn spawn_json_shape_tracks_wait() {
        // Pre-wait (or a slow-host timeout): session is null.
        let pre = spawn_json("mac", "spawn-1", None);
        assert_eq!(pre["spawn_id"], "spawn-1");
        assert_eq!(pre["host"], "mac");
        assert!(pre["session"].is_null());

        // Resolved: the session ref is filled in.
        let full = spawn_json("mac", "spawn-1", Some("agentium:a-b-c"));
        assert_eq!(full["session"], "agentium:a-b-c");
        // Delivery moved to the host, so there is no event_id key either way.
        assert!(full.get("event_id").is_none());
    }

    /// The property `| jq -r .session` pipelines depend on: one record, one line.
    /// A pretty-printed object silently defeats `tail -1`/`head -1`/`read -r`,
    /// which reads as a failed spawn and invites the retry that duplicates it.
    #[test]
    fn json_line_emits_exactly_one_line() {
        let rendered = json_line(&spawn_json("mac", "spawn-1", Some("agentium:a-b-c")))
            .expect("render spawn json");

        assert_eq!(rendered.lines().count(), 1, "rendered: {rendered:?}");
        assert!(!rendered.contains('\n'), "rendered: {rendered:?}");
        // Still the same object, just on one line.
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("one-line output is still valid JSON");
        assert_eq!(parsed["session"], "agentium:a-b-c");
        assert_eq!(parsed["spawn_id"], "spawn-1");
    }

    #[test]
    fn short_id_prefixes_to_eight() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }
}
