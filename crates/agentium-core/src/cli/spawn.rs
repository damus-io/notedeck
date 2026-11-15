//! What `nd agentium spawn` and `nd agentium spawn` share: where a spawn goes
//! when its flags leave the target out, the duplicate guard that runs before
//! anything is published, and how the result is reported.

use std::time::Duration;

use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use serde::Serialize;

use crate::session_events::SPAWN_DEDUPE_WINDOW_SECS;
use crate::session_loader::{
    load_deleted_session_states_for_author, load_session_states_for_author,
    resolve_session_including_deleted, SessionState,
};

/// Default bound on `spawn --wait`: how long to wait for the target host to
/// answer a spawn command with the new session's kind-31988 state. A host
/// that never answers (none running on the target, or one stuck `pending`)
/// can't hang the caller past this — the spawn command was still published,
/// so a later `list` finds the session if the host was merely slow.
///
/// Sized to real latency, not the happy path: a host processes the kind-31989
/// and fans its state back only on an egui frame, so an idle/backgrounded Dave
/// app (which repaints reactively) is regularly slower than the sub-2s local
/// case — answers around ~20s have been observed. 30s covers that with
/// headroom while still bounding a genuinely dead host.
pub const SPAWN_WAIT_DEFAULT: Duration = Duration::from_secs(30);

/// The variable that overrides [`SPAWN_WAIT_DEFAULT`], in seconds.
pub const SPAWN_WAIT_ENV: &str = "AGENTIUM_SPAWN_WAIT";

/// Resolve the `spawn --wait` bound from the `--wait-timeout <secs>` flag, the
/// [`SPAWN_WAIT_ENV`] value (seconds), else [`SPAWN_WAIT_DEFAULT`]. The flag
/// wins over the env, which wins over the default; a zero or unparseable env
/// value is ignored (falls through to the default) so a stray export can't pin
/// the wait to nothing.
pub fn resolve_spawn_wait(flag_secs: Option<u64>, env_secs: Option<&str>) -> Duration {
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

/// Where the *current* session (`$AGENTIUM_SESSION`) runs, for commands whose
/// target defaults to "here". Every field is `None` when not running inside a
/// session.
#[derive(Default)]
pub struct CurrentSession {
    pub host: Option<String>,
    pub cwd: Option<String>,
    pub backend: Option<String>,
    /// The current session's `agentium:` ref: its resolved state's ref, or the
    /// selector verbatim when the state isn't cached.
    pub session: Option<String>,
    /// The backend profile the current session runs under, if any.
    pub profile: Option<String>,
}

/// Where a process runs, for a current session whose state isn't cached
/// yet: the machine's hostname and the process's working directory.
pub struct Here {
    pub host: Option<String>,
    pub cwd: Option<String>,
}

impl CurrentSession {
    /// The session `selector` (the caller's `$AGENTIUM_SESSION`) names,
    /// resolved like `show` with no selector, live or deleted. `None` for no
    /// selector: outside a session every field must come from a flag, which
    /// isn't an error.
    ///
    /// A selector whose state isn't in this db yet (a cold cache, or a stale
    /// ref) still means we are running inside that session, so its host and
    /// cwd are `here`'s: the host is right because Dave names its host with
    /// the same `gethostname()` and runs agents as local subprocesses.
    pub fn resolve(
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        selector: Option<&str>,
        here: impl FnOnce() -> Here,
    ) -> Self {
        let Some(selector) = selector.filter(|s| !s.is_empty()) else {
            return Self::default();
        };
        let live = load_session_states_for_author(ndb, txn, author);
        let deleted = load_deleted_session_states_for_author(ndb, txn, author);
        match resolve_session_including_deleted(&live, &deleted, selector) {
            Ok(state) => CurrentSession {
                host: Some(state.hostname.clone()),
                cwd: Some(state.cwd.clone()),
                backend: state.backend.clone(),
                session: Some(state.agentium_uri()),
                profile: state.profile.clone(),
            },
            Err(_) => {
                let here = here();
                CurrentSession {
                    session: Some(selector.to_string()),
                    host: here.host,
                    cwd: here.cwd,
                    backend: None,
                    profile: None,
                }
            }
        }
    }
}

/// The flags that pick a spawn's target, each `None` when left out.
pub struct TargetFlags<'a> {
    pub host: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub backend: Option<&'a str>,
    pub profile: Option<&'a str>,
}

/// The fully-resolved spawn target: which host to create the session on, the
/// cwd it runs in, and the backend to launch.
pub struct SpawnTarget {
    pub host: String,
    pub cwd: String,
    pub backend: String,
    /// The spawning session's `agentium:` ref, recorded as the new session's
    /// `parent-session`. `None` when not running inside a session.
    pub parent_session: Option<String>,
    /// The backend profile the session runs under ([`spawn_profile`]).
    pub profile: Option<String>,
}

impl SpawnTarget {
    /// Default each omitted flag to the `current` session's own state, so a
    /// bare spawn starts a sibling in the same worktree on the same host.
    /// `backend` falls back to `"claude"`. Errors when `host`/`cwd` can't be
    /// determined — there is nothing to target.
    pub fn resolve(flags: &TargetFlags, current: CurrentSession) -> Result<Self, String> {
        let target = merge_spawn_target(
            flags,
            current.host.as_deref(),
            current.cwd.as_deref(),
            current.backend.as_deref(),
        )?;
        let profile = spawn_profile(flags, current.profile.as_deref())?;
        Ok(SpawnTarget {
            parent_session: current.session,
            profile,
            ..target
        })
    }
}

/// The backend profile a spawn runs under: `--profile`, else the current
/// session's own profile when `--backend` wasn't given, so a bare spawn from a
/// session under `hydra` starts its sibling under `hydra` too rather than on
/// the plain backend's account. An explicit `--backend` without `--profile`
/// asks for that backend plainly. A profile is an overlay on the Claude
/// backend, so `--profile` with any other `--backend` is an error.
fn spawn_profile(flags: &TargetFlags, cur_profile: Option<&str>) -> Result<Option<String>, String> {
    if let Some(profile) = flags.profile {
        if let Some(backend) = flags.backend.filter(|b| *b != "claude") {
            return Err(format!(
                "--profile {profile} runs on the claude backend, not --backend {backend}"
            ));
        }
        return Ok(Some(profile.to_string()));
    }
    if flags.backend.is_some() {
        return Ok(None);
    }
    Ok(cur_profile.filter(|p| !p.is_empty()).map(str::to_string))
}

/// Merge the raw `--host`/`--cwd`/`--backend` flags with the current session's
/// values. A flag wins; otherwise the current-session value fills the gap.
fn merge_spawn_target(
    flags: &TargetFlags,
    cur_host: Option<&str>,
    cur_cwd: Option<&str>,
    cur_backend: Option<&str>,
) -> Result<SpawnTarget, String> {
    let pick = |flag: Option<&str>, cur: Option<&str>| -> Option<String> {
        flag.or(cur).filter(|v| !v.is_empty()).map(str::to_string)
    };

    let host = pick(flags.host, cur_host).ok_or(
        "no host — pass --host (or run inside a session so $AGENTIUM_SESSION supplies it)",
    )?;
    let cwd = pick(flags.cwd, cur_cwd)
        .ok_or("no cwd — pass --cwd (or run inside a session so $AGENTIUM_SESSION supplies it)")?;
    let backend = pick(flags.backend, cur_backend).unwrap_or_else(|| "claude".to_string());

    Ok(SpawnTarget {
        host,
        cwd,
        backend,
        parent_session: None,
        profile: None,
    })
}

/// A live session an about-to-be-published spawn looks like a retry of.
///
/// Matching is on what `list` can actually see — host, cwd, title, age — because
/// the caller cannot read the host's own idempotency bookkeeping. That makes this
/// a second line of defence rather than the primary one: it still catches the
/// retry against an *older* host that ignores the `idempotency_key` tag entirely.
///
/// A title is required for a match, and is the whole discriminator. Without one
/// the new session's title would be derived from its first message, so there is
/// nothing on either side to compare and every recent sibling in the worktree
/// would look like a duplicate — including legitimate fan-out. Untitled spawns
/// rely on the host's key instead.
pub fn find_duplicate_spawn<'a>(
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
/// duplicated, and the flag that overrides. A caller that only sees
/// "refused" learns nothing and reaches for the retry again; one that sees
/// the existing ref can check it and move on.
pub fn duplicate_spawn_error(existing: &SessionState, title: &str, now: u64) -> String {
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

/// The longest stretch of a prompt's last line [`prompt_receipt`] quotes.
const RECEIPT_LAST_LINE_CHARS: usize = 80;

/// The one-line receipt for a delivered prompt: its size and its last line,
/// e.g. `prompt: 1834 bytes, 22 lines, last line: "Previous iteration: …"`.
/// The caller compares the last line with the one they wrote; a mismatch means
/// their shell delivered only part of it. A last line longer than
/// [`RECEIPT_LAST_LINE_CHARS`] keeps its *end*, since the end is what a cut
/// loses.
pub fn prompt_receipt(prompt: &str) -> String {
    let lines = prompt.lines().count();
    let last = prompt.lines().last().unwrap_or_default();
    let chars = last.chars().count();
    let tail = if chars > RECEIPT_LAST_LINE_CHARS {
        let skip = chars - RECEIPT_LAST_LINE_CHARS;
        format!("…{}", last.chars().skip(skip).collect::<String>())
    } else {
        last.to_string()
    };
    format!(
        "prompt: {} bytes, {lines} lines, last line: {tail:?}",
        prompt.len()
    )
}

/// A short (8-char) prefix of an id, for the scannable one-line spawn report.
pub fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// A spawn's result: `--json` is `{ spawn_id, host, session }`, where
/// `session` is the new session's `agentium:` ref, `null` until a wait
/// resolves it (and when a slow host outlasts the wait — the spawn, and any
/// prompt, is still delivered). Handoff pipelines read `.session`, so the
/// field names are a contract.
#[derive(Serialize, Debug, Clone)]
pub struct Spawned {
    pub spawn_id: String,
    pub host: String,
    pub session: Option<String>,
}

impl Spawned {
    /// The one-line text report.
    pub fn render(&self) -> String {
        let short = short_id(&self.spawn_id);
        match &self.session {
            None => format!("spawn command sent to {} (spawn {short}…)\n", self.host),
            Some(uri) => format!("spawned {uri} on {} (spawn {short}…)\n", self.host),
        }
    }

    /// `--ref`'s bare ref, or the error for a host that never answered, so
    /// `ref=$(…)` never silently captures a `spawn_id`.
    pub fn session_ref(&self) -> Result<&str, String> {
        self.session.as_deref().ok_or_else(|| {
            format!(
                "no host answered on {} in time, so there is no ref to print (spawn {}…). \
                 The command was published and any prompt rides it, so check `nd agentium list` \
                 before spawning again",
                self.host,
                short_id(&self.spawn_id),
            )
        })
    }

    /// The error for a bare `--wait` the host never answered: it had no other
    /// purpose than the ref. (A `--prompt` spawn isn't one: the host delivers
    /// the prompt whenever it processes the command.)
    pub fn unanswered(&self, waited: Duration) -> String {
        format!(
            "no host answered on {} within {}s (spawn {}…) — the command was \
             published, so `nd agentium list` will find the session if the host was just slow \
             (raise the bound with --wait-timeout <secs> or ${SPAWN_WAIT_ENV})",
            self.host,
            waited.as_secs(),
            short_id(&self.spawn_id),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live session on host `host` in `/home/u/proj`.
    fn session(host: &str, title: &str, created_at: u64) -> SessionState {
        SessionState {
            claude_session_id: format!("sess-{title}"),
            title: title.to_string(),
            hostname: host.to_string(),
            cwd: "/home/u/proj".to_string(),
            status: "working".to_string(),
            created_at,
            ..Default::default()
        }
    }

    fn flags<'a>(
        host: Option<&'a str>,
        cwd: Option<&'a str>,
        backend: Option<&'a str>,
    ) -> TargetFlags<'a> {
        TargetFlags {
            host,
            cwd,
            backend,
            profile: None,
        }
    }

    fn target(host: &str, cwd: &str) -> SpawnTarget {
        SpawnTarget {
            host: host.to_string(),
            cwd: cwd.to_string(),
            backend: "claude".to_string(),
            parent_session: None,
            profile: None,
        }
    }

    /// The pre-publish guard. Each case is a way it could refuse the wrong thing
    /// or wave through the duplicate it exists to catch.
    #[test]
    fn find_duplicate_spawn_matches_only_a_recent_same_target_same_title() {
        const NOW: u64 = 1_800_000_000;
        let t = target("mbp", "/home/u/proj");
        let live = vec![session("mbp", "Fix the parser", NOW - 30)];

        assert!(find_duplicate_spawn(&live, &t, Some("Fix the parser"), NOW).is_some());
        assert!(find_duplicate_spawn(&live, &t, Some("Wire the widget"), NOW).is_none());
        // No title: nothing to compare, so the guard stands down.
        assert!(find_duplicate_spawn(&live, &t, None, NOW).is_none());
        assert!(find_duplicate_spawn(&live, &t, Some(""), NOW).is_none());
        let untitled = vec![session("mbp", "", NOW - 30)];
        assert!(find_duplicate_spawn(&untitled, &t, Some(""), NOW).is_none());

        // Another host, and another worktree, are not duplicates.
        let other_host = target("studio", "/home/u/proj");
        assert!(find_duplicate_spawn(&live, &other_host, Some("Fix the parser"), NOW).is_none());
        let other_cwd = target("mbp", "/home/u/other");
        assert!(find_duplicate_spawn(&live, &other_cwd, Some("Fix the parser"), NOW).is_none());

        // Past the window the same title is new work; exactly at it, a duplicate.
        let stale = vec![session(
            "mbp",
            "Fix the parser",
            NOW - SPAWN_DEDUPE_WINDOW_SECS - 1,
        )];
        assert!(find_duplicate_spawn(&stale, &t, Some("Fix the parser"), NOW).is_none());
        let edge = vec![session(
            "mbp",
            "Fix the parser",
            NOW - SPAWN_DEDUPE_WINDOW_SECS,
        )];
        assert!(find_duplicate_spawn(&edge, &t, Some("Fix the parser"), NOW).is_some());
    }

    /// A `--title` lands in `custom_title`, so the guard compares the
    /// *displayed* title.
    #[test]
    fn find_duplicate_spawn_compares_the_displayed_title() {
        const NOW: u64 = 1_800_000_000;
        let t = target("mbp", "/home/u/proj");
        let mut renamed = session("mbp", "read crates/foo and fix…", NOW - 30);
        renamed.custom_title = Some("Fix the parser".to_string());
        let live = vec![renamed];
        assert!(find_duplicate_spawn(&live, &t, Some("Fix the parser"), NOW).is_some());
        assert!(find_duplicate_spawn(&live, &t, Some("read crates/foo and fix…"), NOW).is_none());
    }

    /// The refusal names the session and the override.
    #[test]
    fn duplicate_spawn_error_names_the_session_and_the_override() {
        const NOW: u64 = 1_800_000_000;
        let existing = session("mbp", "Fix the parser", NOW - 42);
        let message = duplicate_spawn_error(&existing, "Fix the parser", NOW);
        assert!(message.contains(&existing.agentium_uri()), "{message}");
        assert!(message.contains("/home/u/proj"), "{message}");
        assert!(message.contains("42s ago"), "{message}");
        assert!(message.contains("--allow-duplicate"), "{message}");
    }

    #[test]
    fn resolve_spawn_wait_precedence() {
        assert_eq!(
            resolve_spawn_wait(Some(45), Some("20")),
            Duration::from_secs(45)
        );
        assert_eq!(
            resolve_spawn_wait(None, Some("20")),
            Duration::from_secs(20)
        );
        assert_eq!(resolve_spawn_wait(None, None), SPAWN_WAIT_DEFAULT);
        assert_eq!(
            resolve_spawn_wait(Some(0), Some("20")),
            Duration::from_secs(20)
        );
        assert_eq!(resolve_spawn_wait(None, Some("0")), SPAWN_WAIT_DEFAULT);
        assert_eq!(resolve_spawn_wait(None, Some("nope")), SPAWN_WAIT_DEFAULT);
    }

    #[test]
    fn prompt_receipt_quotes_the_last_line() {
        assert_eq!(
            prompt_receipt("Read the card.\nlast line"),
            r#"prompt: 24 bytes, 2 lines, last line: "last line""#
        );
        let line = format!("{}END", "x".repeat(200));
        let receipt = prompt_receipt(&line);
        assert!(receipt.ends_with(r#"xxEND""#), "{receipt}");
        assert!(receipt.contains("\"…x"), "{receipt}");
    }

    /// A bare spawn from a profile session keeps its profile, `--backend`
    /// alone asks for a plain session, `--profile` wins, and a profile on a
    /// non-claude backend is refused.
    #[test]
    fn spawn_profile_inherits_unless_a_backend_is_named() {
        let bare = flags(None, None, None);
        assert_eq!(
            spawn_profile(&bare, Some("hydra")).unwrap().as_deref(),
            Some("hydra")
        );
        assert_eq!(spawn_profile(&bare, Some("")).unwrap(), None);
        let plain = flags(None, None, Some("claude"));
        assert_eq!(spawn_profile(&plain, Some("hydra")).unwrap(), None);
        let flagged = TargetFlags {
            profile: Some("work"),
            ..flags(None, None, Some("claude"))
        };
        assert_eq!(
            spawn_profile(&flagged, Some("hydra")).unwrap().as_deref(),
            Some("work")
        );
        let codex = TargetFlags {
            profile: Some("hydra"),
            ..flags(None, None, Some("codex"))
        };
        assert!(spawn_profile(&codex, None).is_err());
    }

    #[test]
    fn merge_spawn_target_flags_win_and_current_fills_gaps() {
        let t = merge_spawn_target(
            &flags(Some("flaghost"), Some("/flag"), Some("codex")),
            Some("curhost"),
            Some("/cur"),
            Some("claude"),
        )
        .unwrap();
        assert_eq!((t.host.as_str(), t.cwd.as_str()), ("flaghost", "/flag"));
        assert_eq!(t.backend, "codex");

        let t = merge_spawn_target(
            &flags(None, None, None),
            Some("curhost"),
            Some("/cur"),
            None,
        )
        .unwrap();
        assert_eq!((t.host.as_str(), t.cwd.as_str()), ("curhost", "/cur"));
        assert_eq!(t.backend, "claude");

        assert!(merge_spawn_target(&flags(None, Some("/c"), None), None, None, None).is_err());
        assert!(merge_spawn_target(&flags(Some("h"), None, None), None, None, None).is_err());
        assert!(
            merge_spawn_target(&flags(None, Some("/c"), None), Some(""), None, None).is_err(),
            "an empty current value counts as absent"
        );
    }

    #[test]
    fn spawned_reports_and_json_track_the_wait() {
        let pre = Spawned {
            spawn_id: "spawn-123456789".into(),
            host: "mac".into(),
            session: None,
        };
        let json = serde_json::to_value(&pre).unwrap();
        assert!(json["session"].is_null());
        assert_eq!(json["spawn_id"], "spawn-123456789");
        assert_eq!(
            pre.render(),
            "spawn command sent to mac (spawn spawn-12…)\n"
        );
        let err = pre.session_ref().unwrap_err();
        assert!(err.contains("agentium list"), "{err}");

        let full = Spawned {
            session: Some("agentium:a-b-c".into()),
            ..pre
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap()["session"],
            "agentium:a-b-c"
        );
        assert_eq!(full.session_ref().unwrap(), "agentium:a-b-c");
        assert!(full.render().starts_with("spawned agentium:a-b-c on mac"));
    }
}
