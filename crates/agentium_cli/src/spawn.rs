//! `agentium spawn` — tell a Dave host to create a fresh session, then
//! optionally wait for its `agentium:` ref. Where a spawn goes, the duplicate
//! guard and the report are [`agentium_core::cli::spawn`], shared with
//! `nd agentium spawn`.

use std::time::Duration;

use agentium_core::Engine;
use agentium_core::cli::spawn::{
    CurrentSession, Here, SPAWN_WAIT_ENV, SpawnTarget, Spawned, TargetFlags, duplicate_spawn_error,
    find_duplicate_spawn, prompt_receipt, resolve_spawn_wait, short_id,
};
use agentium_core::session_events::{SessionRelation, SpawnOptions, spawn_idempotency_key};
use agentium_core::session_loader::SessionState;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};

/// How long into a `spawn --wait` to print a one-time "still waiting…" note on
/// stderr, so a human isn't left staring at a silent block while a slow host
/// warms up. Stays on stderr so `--json`/scripted stdout is untouched.
const SPAWN_WAIT_PROGRESS: Duration = Duration::from_secs(8);

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
    /// `--model`: the model the new session's agent starts on, as the backend
    /// names it (an alias or a full id). Rides the command as a `model` tag;
    /// `None` leaves the backend's default.
    pub(crate) model: Option<String>,
    /// `--issue-url`: the issue the new session works (e.g. a
    /// `headway:<board>/<word-id>` card), already checked to be a URI. It rides
    /// the command and the host copies it onto the session's state. `None`
    /// links no issue.
    pub(crate) issue_url: Option<String>,
    /// `--profile`: the backend profile (kind-31992 slug) the new session runs
    /// under, already checked to be a valid slug. `None` inherits the current
    /// session's profile unless `--backend` was given; see [`spawn_profile`].
    pub(crate) profile: Option<String>,
    /// `--child` / `--successor`: how the new session relates to the session
    /// running this spawn, recorded beside its `parent-session` on the command
    /// and the new session's state. The default records no relation.
    pub(crate) relation: SessionRelation,
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
    /// `--ref`: print only the new session's `agentium:` ref on stdout, so a
    /// caller captures it with `ref=$(agentium spawn … --ref)` and no `jq`.
    /// Implies `--wait`, since the ref is what it waits for.
    pub(crate) print_ref: bool,
}

impl SpawnOpts {
    /// Whether the spawn should wait for the host's answer: the explicit `--wait`
    /// or `--ref`, or implicitly whenever `--prompt` is set. `--prompt` no longer *needs* the
    /// wait for delivery (the host delivers it off the command), but implying it
    /// means the common prompted spawn still reports the resolved `agentium:` ref
    /// when the host answers promptly.
    fn effective_wait(&self) -> bool {
        self.wait || self.print_ref || self.prompt.is_some()
    }

    /// How [`emit_spawn`] reports the result: `--ref`, else `--json`, else text.
    fn output(&self, as_json: bool) -> SpawnOutput {
        if self.print_ref {
            return SpawnOutput::Ref;
        }
        if as_json {
            return SpawnOutput::Json;
        }
        SpawnOutput::Text
    }
}

/// The current session (`$AGENTIUM_SESSION`), for commands whose target
/// defaults to "here" (see [`CurrentSession::resolve`]). A ref this cache
/// doesn't hold yet — a cold cache's first sync is cut off at `SYNC_MAX`
/// before it reaches the session — targets this process's host and cwd.
pub(crate) fn current_session(engine: &Engine, author: &Pubkey) -> Result<CurrentSession> {
    let selector = std::env::var("AGENTIUM_SESSION").ok();
    let txn = Transaction::new(engine.ndb())?;
    Ok(CurrentSession::resolve(
        engine.ndb(),
        &txn,
        author,
        selector.as_deref(),
        here,
    ))
}

/// This process's own host and working directory.
fn here() -> Here {
    Here {
        host: Some(gethostname::gethostname().to_string_lossy().into_owned()),
        cwd: std::env::current_dir()
            .ok()
            .map(|d| d.to_string_lossy().into_owned()),
    }
}

/// Resolve the spawn target, defaulting each omitted flag to the
/// [`current_session`]'s own state so a bare `agentium spawn` starts a sibling
/// in the same worktree on the same host.
fn resolve_spawn_target(engine: &Engine, author: &Pubkey, opts: &SpawnOpts) -> Result<SpawnTarget> {
    let current = current_session(engine, author)?;
    let flags = TargetFlags {
        host: opts.host.as_deref(),
        cwd: opts.cwd.as_deref(),
        backend: opts.backend.as_deref(),
        profile: opts.profile.as_deref(),
    };
    Ok(SpawnTarget::resolve(&flags, current)?)
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
    let output = opts.output(as_json);

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
        model: opts.model.as_deref(),
        issue_url: opts.issue_url.as_deref(),
        idempotency_key: None,
        parent_session: target.parent_session.as_deref(),
        relation: opts.relation,
        profile: target.profile.as_deref(),
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
    // before the host answers. The permission mode and model ride it for a sharper
    // reason — the host has to know them *before* it starts the session's backend,
    // which it does the moment it delivers that first message.
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

    // Say what prompt rode the command, on stderr so stdout stays scriptable.
    // A prompt the caller's shell cut short (a heredoc inside `$(...)` on bash
    // 3.2 ends at the first stray `)`) shows up here as the wrong last line.
    if let Some(prompt) = opts.prompt.as_deref() {
        eprintln!("{}", prompt_receipt(prompt));
    }

    // No `--wait`: only the spawn_id is known — report it and return.
    let Some(watch) = watch.as_mut() else {
        emit_spawn(&target.host, &spawn_id, None, output)?;
        return Ok(());
    };

    // Resolve the wait bound: `--wait-timeout` flag, else `$AGENTIUM_SPAWN_WAIT`,
    // else the default. A slow-but-alive host regularly needs more than the old
    // fixed 8s, so this is sized to real latency and tunable per run.
    let wait_bound = resolve_spawn_wait(
        opts.wait_timeout,
        std::env::var(SPAWN_WAIT_ENV).ok().as_deref(),
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
            if opts.prompt.is_some() {
                emit_spawn(&target.host, &spawn_id, None, output)?;
                return Ok(());
            }
            let spawned = spawned(&target.host, &spawn_id, None);
            return Err(spawned.unanswered(wait_bound).into());
        }
    };

    let uri = state.agentium_uri();
    emit_spawn(&target.host, &spawn_id, Some(&uri), output)?;
    Ok(())
}

/// How a spawn's result is printed: the one-line text report, the `--json`
/// object, or `--ref`'s bare `agentium:` ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpawnOutput {
    Text,
    Json,
    Ref,
}

/// The [`Spawned`] report for a spawn and the ref it resolved, if any.
fn spawned(host: &str, spawn_id: &str, session: Option<&str>) -> Spawned {
    Spawned {
        spawn_id: spawn_id.to_string(),
        host: host.to_string(),
        session: session.map(str::to_string),
    }
}

/// Report a spawn's result: [`Spawned::render`]'s line, `--json`'s one-line
/// object, or `--ref`'s bare `agentium:` ref — which fails with no ref (a slow
/// host outlasted the wait), so `ref=$(…)` never silently captures a
/// `spawn_id`. Any `--prompt` is delivered by the host off the spawn command,
/// so it is not reported here.
fn emit_spawn(
    host: &str,
    spawn_id: &str,
    session: Option<&str>,
    output: SpawnOutput,
) -> Result<()> {
    let spawned = spawned(host, spawn_id, session);
    match output {
        SpawnOutput::Json => println!("{}", json_line(&serde_json::to_value(&spawned)?)?),
        SpawnOutput::Ref => println!("{}", spawned.session_ref()?),
        SpawnOutput::Text => print!("{}", spawned.render()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--prompt` and `--ref` imply `--wait`; neither means fire-and-forget.
    #[test]
    fn prompt_and_ref_imply_wait_and_ref_picks_the_output() {
        let mut opts = SpawnOpts {
            host: Some("h".into()),
            cwd: Some("/c".into()),
            backend: None,
            title: None,
            prompt: None,
            permission_mode: None,
            model: None,
            issue_url: None,
            profile: None,
            relation: SessionRelation::None,
            idempotency_key: None,
            allow_duplicate: false,
            wait: false,
            wait_timeout: None,
            print_ref: false,
        };
        assert!(!opts.effective_wait());
        assert_eq!(opts.output(false), SpawnOutput::Text);
        assert_eq!(opts.output(true), SpawnOutput::Json);
        opts.prompt = Some("hi".into());
        assert!(opts.effective_wait());
        opts.prompt = None;
        opts.print_ref = true;
        assert!(opts.effective_wait());
        assert_eq!(opts.output(false), SpawnOutput::Ref);
    }

    /// `--ref` with no resolved ref fails rather than printing a `spawn_id`.
    #[test]
    fn ref_output_without_a_ref_is_an_error() {
        let err = emit_spawn("mac", "spawn-123456789", None, SpawnOutput::Ref)
            .expect_err("no ref to print");
        let msg = err.to_string();
        assert!(msg.contains("spawn spawn-12"), "{msg}");
        assert!(msg.contains("agentium list"), "{msg}");
    }

    /// The property `| jq -r .session` pipelines depend on: one record, one line.
    #[test]
    fn json_line_emits_exactly_one_line() {
        let value =
            serde_json::to_value(spawned("mac", "spawn-1", Some("agentium:a-b-c"))).unwrap();
        let rendered = json_line(&value).expect("render spawn json");
        assert_eq!(rendered.lines().count(), 1, "rendered: {rendered:?}");
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["session"], "agentium:a-b-c");
        assert_eq!(parsed["spawn_id"], "spawn-1");
    }

    /// An unresolvable `$AGENTIUM_SESSION` still targets this machine.
    #[test]
    fn here_targets_this_host_and_cwd() {
        let here = here();
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        assert_eq!(here.host.as_deref(), Some(host.as_str()));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(here.cwd.as_deref(), Some(&*cwd.to_string_lossy()));
    }
}
