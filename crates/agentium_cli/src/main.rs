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

use agentium_core::session_loader::SessionState;
use agentium_core::{Engine, Transport};
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

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`agentium list | head`) instead of
    // panicking in println! on EPIPE.
    nostrdb_net::relay::sync::reset_sigpipe();
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
    /// Reopen a closed (possibly soft-deleted) session on its host so a new
    /// message drives its backend again. The argument is any session selector
    /// `list` accepts (a d-tag, cli-session id, or `agentium:` word-id).
    Resume {
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
    let mut transport = engine
        .transport_handle()
        .ok_or("engine started without a relay loop")?;

    // Connect installs the PNS discovery subscription — kind-1080 events authored
    // by this identity's derived PNS pubkey — which streams the whole encrypted
    // session corpus into the cache (where ndb decrypts it) and points publishes
    // at the relay. Best-effort: an unreachable relay just leaves us reading
    // whatever the cache already holds. A `--author` pointing at someone else
    // still can't decrypt *their* private sessions — only they hold that key.
    engine.connect(&mut transport, &relay)?;

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
        Command::Resume { session } => {
            cmd_resume(&engine, &mut transport, &read_pk, &session).await?
        }
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
async fn cmd_resume(
    engine: &Engine,
    transport: &mut impl Transport,
    author: &Pubkey,
    selector: &str,
) -> Result<()> {
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

    engine.resume_session(
        transport,
        &target_host,
        &cwd,
        &backend,
        &target_sid,
        &cli_sid,
    )?;

    // Flush: the publish rides the loop's FIFO, so a settle barrier enqueued
    // after it resolves once the loop has drained (sent) the publish. Bounded so
    // an unreachable relay can't stall exit — the event is already ingested
    // locally regardless.
    let _ = tokio::time::timeout(PUBLISH_FLUSH, engine.wait_for_sync()).await;

    println!("resume command sent to {target_host} for {uri}");
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

    for (host, group) in group_by_host(sessions) {
        println!("{}", paint(color, SGR_BOLD, &host));
        for s in group {
            println!("{}", session_row(&s, now, color));
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
/// working dir, backend, permission mode, and last-updated age.
fn session_row(s: &SessionState, now: u64, color: bool) -> String {
    let (glyph, label, sgr) = status_style(&s.status);
    let sref = agentium_core::wordid::session_ref(&s.claude_session_id);
    let sref_col = paint(color, "90", &col(&sref, 28));
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
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        let Some((name, rest)) = positionals.split_first() else {
            return Ok(None);
        };
        let command = parse_command(name, rest)?;

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

fn parse_command(name: &str, rest: &[String]) -> Result<Command> {
    Ok(match name {
        "list" => Command::List,
        "show" => Command::Show {
            // The selector is optional: fall back to `$AGENTIUM_SESSION` (the
            // `agentium:` ref a running Dave session exports) so `agentium show`
            // with no argument describes the current session. An empty env var
            // is treated as unset. `cmd_show` errors if neither is present.
            session: rest
                .first()
                .cloned()
                .or_else(|| env::var("AGENTIUM_SESSION").ok().filter(|s| !s.is_empty())),
        },
        "resume" => Command::Resume {
            session: arg(rest, 0, name)?,
        },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
        other => return Err(format!("unknown command '{other}' (try `agentium --help`)").into()),
    })
}

/// The `idx`th positional argument to a command, or an error naming the command.
fn arg(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    rest.get(idx)
        .cloned()
        .ok_or_else(|| format!("`{cmd}` is missing an argument").into())
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
    resume <session>  Reopen a closed (even soft-deleted) session on its host so
                      a new message drives its backend again. Takes any selector
                      `list` accepts (d-tag, cli-session id, or agentium: ref);
                      revives the session's agentium: reference in place.
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
        let row = session_row(&s, 60, false);
        assert!(!row.contains('\x1b'), "no ANSI when color=false: {row:?}");
        assert!(row.contains("agentium:"), "row leads with the sayable ref");
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
        match parse_command("show", &["agentium:a-b-c".to_string()]).unwrap() {
            Command::Show { session } => assert_eq!(session.as_deref(), Some("agentium:a-b-c")),
            _ => panic!("expected Show"),
        }
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
}
