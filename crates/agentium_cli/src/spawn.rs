//! `agentium spawn` — tell a Dave host to create a fresh session, then
//! optionally wait for its `agentium:` ref.

use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_events::{
    SPAWN_DEDUPE_WINDOW_SECS, SpawnOptions, spawn_idempotency_key,
};
use agentium_core::session_loader::SessionState;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};

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

/// The resolved flags for `agentium spawn`. `host`/`cwd`/`backend` are still the
/// *raw* flags here (each `None` when omitted); [`resolve_spawn_target`] fills the
/// gaps from the current session before the command is built.
pub(crate) struct SpawnOpts {
    pub(crate) host: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) backend: Option<String>,
    /// `--title`: an explicit, sticky session title (rides the command as a
    /// `custom_title` tag). `None` lets the host derive one from the first message.
    pub(crate) title: Option<String>,
    /// `--prompt`: the session's first `user` message. It rides the spawn command
    /// (as its `prompt` tag) and the host delivers it when the session comes up,
    /// so delivery is independent of `--wait`. Still implies `--wait` so the
    /// resolved `agentium:` ref gets reported when the host answers in time.
    pub(crate) prompt: Option<String>,
    /// `--permission-mode`: the permission mode the new session's agent starts
    /// in, already normalized to a canonical wire spelling. `None` leaves the
    /// host's own default in place.
    pub(crate) permission_mode: Option<String>,
    /// `--issue-url`: the issue the new session works (e.g. a
    /// `headway:<board>/<word-id>` card), already checked to be a URI. It rides
    /// the command and the host copies it onto the session's state. `None`
    /// links no issue.
    pub(crate) issue_url: Option<String>,
    /// `--idempotency-key`: the caller's own name for this *request*, overriding
    /// the one derived from the request's fields. Useful when the caller has a
    /// better notion of identity than the fields give — retrying "the spawn for
    /// job 4821" should dedupe even if its prompt was reworded between attempts.
    pub(crate) idempotency_key: Option<String>,
    /// `--allow-duplicate`: deliberately spawn a second session that a duplicate
    /// defence would otherwise refuse. Skips the pre-publish guard *and* omits
    /// the idempotency key from the command, so neither this CLI nor the host
    /// treats the spawn as a retry — one flag for "I mean it", rather than a
    /// guard the host would then silently re-impose.
    pub(crate) allow_duplicate: bool,
    /// `--wait`: block (bounded by [`resolve_spawn_wait`]) until the host answers
    /// with the new session's kind-31988 state, then print its durable `agentium:`
    /// ref.
    pub(crate) wait: bool,
    /// `--wait-timeout <secs>`: override the wait bound for this run (else
    /// `$AGENTIUM_SPAWN_WAIT`, else [`SPAWN_WAIT_DEFAULT`]). `None` leaves the
    /// resolution to the env/default; see [`resolve_spawn_wait`].
    pub(crate) wait_timeout: Option<u64>,
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

/// Where the *current* session (`$AGENTIUM_SESSION`) runs, for commands whose
/// target defaults to "here". Every field is `None` when not running inside a
/// session; see [`current_session`] for when the ref doesn't resolve.
#[derive(Default)]
pub(crate) struct CurrentSession {
    pub(crate) host: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) backend: Option<String>,
}

impl CurrentSession {
    /// This process's own host and working directory, for when we know we're
    /// inside a session but can't read its state.
    ///
    /// The host is right because Dave names its host with the same
    /// `gethostname()` and runs agents as local subprocesses. The cwd is the
    /// agent's current directory, which is the session's unless the agent has
    /// `cd`'d away from it. The backend is unknown and is left to fall back.
    fn here() -> Self {
        CurrentSession {
            host: Some(gethostname::gethostname().to_string_lossy().into_owned()),
            cwd: std::env::current_dir()
                .ok()
                .map(|d| d.to_string_lossy().into_owned()),
            backend: None,
        }
    }
}

/// Look up the current session's host/cwd/backend from its kind-31988 state,
/// resolved the way `cmd_show` does with no selector (the `$AGENTIUM_SESSION`
/// ref, live or deleted). Outside a session every field is `None` and must come
/// from a flag, which isn't an error.
///
/// Inside a session whose state isn't in this CLI's cache yet, the fields
/// come from [`CurrentSession::here`] instead. That happens on a cold cache,
/// whose first sync is cut off at `SYNC_MAX` before it reaches this session.
pub(crate) fn current_session(engine: &Engine, author: &Pubkey) -> Result<CurrentSession> {
    use agentium_core::session_loader::{
        load_deleted_session_states_for_author, load_session_states_for_author,
        resolve_session_including_deleted,
    };

    let Some(selector) = std::env::var("AGENTIUM_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Ok(CurrentSession::default());
    };
    let txn = Transaction::new(engine.ndb())?;
    let live = load_session_states_for_author(engine.ndb(), &txn, author);
    let deleted = load_deleted_session_states_for_author(engine.ndb(), &txn, author);
    Ok(
        match resolve_session_including_deleted(&live, &deleted, &selector) {
            Ok(state) => CurrentSession {
                host: Some(state.hostname.clone()),
                cwd: Some(state.cwd.clone()),
                backend: state.backend.clone(),
            },
            // Not cached yet, or a stale ref. Either way we are still running
            // inside that session, so its host and cwd are this machine's.
            Err(_) => CurrentSession::here(),
        },
    )
}

/// Resolve the spawn target, defaulting each omitted flag to the
/// [`current_session`]'s own state so a bare `agentium spawn` starts a sibling
/// in the same worktree on the same host. `--backend` falls back to `"claude"`
/// when neither a flag nor a current-session backend is available. Errors when
/// `host`/`cwd` can't be determined (not inside a session and no flag) — there
/// is nothing to target.
fn resolve_spawn_target(engine: &Engine, author: &Pubkey, opts: &SpawnOpts) -> Result<SpawnTarget> {
    let current = current_session(engine, author)?;
    merge_spawn_target(
        opts,
        current.host.as_deref(),
        current.cwd.as_deref(),
        current.backend.as_deref(),
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
/// as [`cmd_send`](crate::send::cmd_send).
pub(crate) async fn cmd_spawn(
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
    let request = SpawnOptions {
        title: opts.title.as_deref(),
        prompt: opts.prompt.as_deref(),
        permission_mode: opts.permission_mode.as_deref(),
        issue_url: opts.issue_url.as_deref(),
        idempotency_key: None,
    };
    let idempotency_key = (!opts.allow_duplicate).then(|| {
        opts.idempotency_key.clone().unwrap_or_else(|| {
            spawn_idempotency_key(&target.host, &target.cwd, &target.backend, &request)
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
            idempotency_key: idempotency_key.as_deref(),
            ..request
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

/// Report a spawn's result. Plain text mirrors [`cmd_send`](crate::send::cmd_send)'s style; `--json`
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::tests::session;

    /// A [`SpawnOpts`] with the target flags set and no title/prompt/mode/wait.
    fn spawn_opts(host: Option<&str>, cwd: Option<&str>, backend: Option<&str>) -> SpawnOpts {
        SpawnOpts {
            host: host.map(str::to_string),
            cwd: cwd.map(str::to_string),
            backend: backend.map(str::to_string),
            title: None,
            prompt: None,
            permission_mode: None,
            issue_url: None,
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

    /// An unresolvable `$AGENTIUM_SESSION` still targets this machine, so a bare
    /// spawn on a cold cache doesn't fail with "no host".
    #[test]
    fn here_targets_this_host_and_cwd() {
        let here = CurrentSession::here();
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(here.host.as_deref(), Some(host.as_str()));
        assert_eq!(here.cwd.as_deref(), Some(&*cwd.to_string_lossy()));
        assert_eq!(here.backend, None);

        let t = merge_spawn_target(
            &spawn_opts(None, None, None),
            here.host.as_deref(),
            here.cwd.as_deref(),
            here.backend.as_deref(),
        )
        .unwrap();
        assert_eq!(t.host, host);
        assert_eq!(t.backend, "claude");
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
