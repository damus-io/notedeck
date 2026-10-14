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
//! argument parsing, config resolution, and dispatch to the per-command modules.

mod config_cmd;
mod grep;
mod help;
mod interrupt;
mod link;
mod list;
mod log;
mod permission;
mod publish;
mod resume;
mod send;
mod show;
mod spawn;
mod term;
mod transcript;
mod watch;

use std::env;
use std::process::ExitCode;
use std::time::Duration;

use agentium_core::Engine;
use agentium_core::session_events::SessionRelation;
use nostrdb_net::Pubkey;
use regex::Regex;

use cli_term::{CaseMode, ColorWhen, PagerMode, compile_pattern};
use nostrdb_net::relay::sync::Result;

use config_cmd::{ConfigAction, ConfigFilters, cmd_config};
use grep::cmd_grep;
use interrupt::cmd_interrupt;
use link::cmd_link;
use list::{ListFilters, ListScope, cmd_list};
use log::{cmd_follow, cmd_log};
use permission::{Decision, ResponseOpts, cmd_mode, cmd_respond};
use resume::cmd_resume;
use send::cmd_send;
use show::cmd_show;
use spawn::{SpawnOpts, cmd_spawn};
use transcript::MessageView;
use watch::{WatchOpts, cmd_watch};

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/agentium-cli` on Linux).
const APP: &str = "agentium-cli";

/// Hard cap on the settle wait, so a reachable-but-silent relay can't stall the
/// read: past this we give up on the reconcile and read whatever the cache holds.
const SYNC_MAX: Duration = Duration::from_secs(6);

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

/// Validate an issue URL — `spawn --issue-url`'s value or `link`'s argument,
/// named by `what` in the error: a URI naming the issue the session works.
///
/// Stored verbatim, so this only checks it *is* a URI — a non-empty
/// `scheme:rest` with an RFC 3986 scheme. That rejects a bare headway word-id
/// (or `board/word-id`), which looks plausible but names no board and so can't
/// be opened from the session; the error points at the routable form instead.
/// Validate a `--model` value: trimmed, non-empty, and a single word.
///
/// Which models exist is the backend's business (Claude takes `opus`/`sonnet`
/// aliases or full ids, Codex its own ids), so the name itself is passed
/// through. What is caught here is a value no backend could take — empty, or
/// with whitespace inside, which is usually a stray word swallowed as the
/// flag's value — so it fails before anything is published.
fn parse_model_flag(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        return Err(format!(
            "--model '{value}' is not a model name — pass an alias the backend \
             knows (e.g. opus, sonnet) or a full model id"
        )
        .into());
    }
    Ok(value.to_string())
}

fn parse_issue_url(value: &str, what: &str) -> Result<String> {
    let value = value.trim();
    let has_scheme = value.split_once(':').is_some_and(|(scheme, rest)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !rest.is_empty()
    });
    if !has_scheme {
        return Err(format!(
            "{what} '{value}' is not a URI — for a headway card use \
             headway:<board>/<word-id>"
        )
        .into());
    }
    Ok(value.to_string())
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
    /// A live dashboard of every session the `list` filters select, redrawn as
    /// sessions change — or, with `--once`, a single frame.
    Watch {
        opts: WatchOpts,
    },
    /// Print git-show-style detail for one resolved session: its kind-31988
    /// state, the run-configs on its host+cwd, its latest usage, a
    /// conversation summary, and a subagent rollup. The selector is optional — it defaults to
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
    /// is the remaining positionals joined with spaces. `--immediate` also
    /// interrupts the session's running turn, so the agent reads the message now
    /// rather than when that turn ends.
    Send {
        session: String,
        text: String,
        immediate: bool,
    },
    /// Publish a kind-31989 spawn command telling a (local or remote) Dave host
    /// to create a fresh session, then optionally block until the host answers
    /// with its kind-31988 state and print the new session's durable `agentium:`
    /// ref. `host`/`cwd`/`backend` default to the *current* session's own state
    /// (`$AGENTIUM_SESSION`) so a bare `agentium spawn` starts a sibling in the
    /// same worktree on the same host. `--title` gives the session an explicit,
    /// sticky title; `--prompt` (which implies `--wait`) delivers a first `user`
    /// message once the session exists; `--permission-mode` picks the mode the
    /// session's agent starts in; `--model` picks the model it starts on;
    /// `--issue-url` links the session to the issue
    /// (e.g. headway card) it works. `--allow-duplicate` opts out of both
    /// duplicate defences (the pre-publish guard and the host's idempotency key).
    Spawn {
        host: Option<String>,
        cwd: Option<String>,
        backend: Option<String>,
        title: Option<String>,
        prompt: Option<String>,
        /// Already normalized to a canonical wire spelling by
        /// [`parse_mode_flag`], so an alias can never reach the command event.
        permission_mode: Option<String>,
        /// Already checked to be a single non-empty word by [`parse_model_flag`].
        model: Option<String>,
        /// Already checked to be a URI by [`parse_issue_url`].
        issue_url: Option<String>,
        /// `--child` / `--successor`, already checked not to be both.
        relation: SessionRelation,
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
    /// Answer a live session's pending permission request (`approve`/`deny`):
    /// the newest one, or the one `--request` names by perm-id prefix. The
    /// selector is required, like `send`'s.
    Respond {
        session: String,
        opts: ResponseOpts,
    },
    /// Change a live session's permission mode on its host — the CLI companion
    /// to Ctrl+M in Dave.
    Mode {
        session: String,
        /// Already normalized to a canonical wire spelling by
        /// [`parse_mode_flag`]: the host reads an unknown string as `default`.
        mode: String,
    },
    /// Link a live session to the issue it works (`link`, `issue_url = Some`) or
    /// clear its link (`unlink`, `None`) on its host, after the fact — for a
    /// session spawned without `--issue-url`.
    Link {
        session: String,
        /// Already checked to be a URI by [`parse_issue_url`].
        issue_url: Option<String>,
    },
    /// List, show, add, edit or remove run configs (kind-31991): the named
    /// shell commands Dave's run bar launches in a session's host+cwd.
    Config {
        action: ConfigAction,
    },
    /// Print the `agentium:` ref for each given kind-31988 d-tag. Pure
    /// computation — no key, cache or relay — so a script that already holds a
    /// session's d-tag (say, from `ndb query --kind 31988`) gets its sayable ref
    /// without resolving it against a corpus.
    Id {
        d_tags: Vec<String>,
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
            Command::List | Command::Show { .. } | Command::Grep { .. } | Command::Id { .. } => {
                false
            }
            // A follow is a live stream, so it needs the connection even though
            // its initial tail is a cache read.
            Command::Log { view, .. } => view.follow,
            // Likewise a live dashboard; a single `--once` frame is a cache read.
            Command::Watch { opts } => !opts.once,
            Command::Config { action } => action.publishes(),
            Command::Resume { .. }
            | Command::Send { .. }
            | Command::Spawn { .. }
            | Command::Interrupt { .. }
            | Command::Respond { .. }
            | Command::Mode { .. }
            | Command::Link { .. }
            | Command::Login { .. }
            | Command::Logout => true,
        }
    }
}

async fn run() -> Result<()> {
    let cli = match Cli::parse(env::args().skip(1))? {
        Invocation::Run(cli) => *cli,
        Invocation::Usage => {
            help::print_usage(nostrdb_net::relay::sync::DEFAULT_RELAY);
            return Ok(());
        }
        Invocation::CommandHelp(cmd) => {
            help::print_command(cmd);
            return Ok(());
        }
    };

    // `login`/`logout` manage the stored key and `id` is pure computation;
    // none of them touch the cache or a relay, so handle them before any of
    // that machinery spins up.
    match &cli.command {
        Command::Login { nsec } => return nostrdb_net::relay::sync::login(nsec, APP),
        Command::Logout => return nostrdb_net::relay::sync::logout(APP),
        Command::Id { d_tags } => {
            for d_tag in d_tags {
                println!("{}", agentium_core::wordid::session_ref(d_tag));
            }
            return Ok(());
        }
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
        Command::Watch { opts } => {
            cmd_watch(&engine, &read_pk, &filters, cli.list_scope, &opts).await?
        }
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
        Command::Send {
            session,
            text,
            immediate,
        } => cmd_send(&engine, &read_pk, &session, &text, immediate, cli.json).await?,
        Command::Spawn {
            host,
            cwd,
            backend,
            title,
            prompt,
            permission_mode,
            model,
            issue_url,
            relation,
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
                model,
                issue_url,
                relation,
                idempotency_key,
                allow_duplicate,
                wait,
                wait_timeout,
            };
            cmd_spawn(&engine, &read_pk, &opts, cli.json).await?
        }
        Command::Interrupt { session } => cmd_interrupt(&engine, &read_pk, &session).await?,
        Command::Respond { session, opts } => {
            cmd_respond(&engine, &read_pk, &session, &opts, cli.json).await?
        }
        Command::Mode { session, mode } => {
            cmd_mode(&engine, &read_pk, &session, &mode, cli.json).await?
        }
        Command::Link { session, issue_url } => {
            cmd_link(&engine, &read_pk, &session, issue_url.as_deref(), cli.json).await?
        }
        Command::Config { action } => {
            let filters = ConfigFilters {
                host: filters.host,
                cwd: filters.cwd,
            };
            cmd_config(&engine, &read_pk, &secret, &action, &filters, cli.json).await?
        }
        Command::Login { .. } | Command::Logout | Command::Id { .. } => {
            unreachable!("handled above")
        }
    }

    Ok(())
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

/// What a command line asks for: a run, or one of the two help pages.
enum Invocation {
    Run(Box<Cli>),
    /// `agentium` with no command, or `agentium --help`: the grouped overview.
    Usage,
    /// `agentium <cmd> --help` or `agentium help <cmd>`: one command's own page.
    CommandHelp(&'static cli_term::help::CommandHelp),
}

impl Cli {
    /// Parse args (without the program name), deciding between a run and a help
    /// page (see [`Invocation`]).
    fn parse(args: impl Iterator<Item = String>) -> Result<Invocation> {
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
        // `watch --once`: one frame, then exit.
        let mut once = false;
        // `spawn` flags. `--title`/`--prompt` are values; `--wait` is a switch
        // (also implied by `--prompt`, resolved in `cmd_spawn`). `--prompt-file`
        // is the escaping-free alternative to `--prompt`: a path (or `-` for
        // stdin) whose contents become the prompt, resolved once the loop ends.
        let mut title = None;
        let mut prompt = None;
        let mut prompt_file = None;
        let mut permission_mode = None;
        let mut model = None;
        let mut issue_url = None;
        let mut child = false;
        let mut successor = false;
        let mut idempotency_key = None;
        let mut allow_duplicate = false;
        let mut wait = false;
        let mut wait_timeout = None;
        // `approve`/`deny` flags: which pending request, the reply text, and
        // (deny only) whether to stop the turn too.
        let mut request = None;
        let mut message = None;
        let mut interrupt = false;
        // `send --immediate`: interrupt the running turn after sending.
        let mut immediate = false;
        // `config add`/`edit` fields.
        let mut config_name = None;
        let mut config_command = None;
        let mut positionals: Vec<String> = Vec::new();
        // `-h`/`--help` is answered after the loop, once the positionals say
        // *which* help — the overview, or one command's page.
        let mut want_help = false;

        let mut args = args;
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value").into())
                    as Result<String>
            };
            match arg.as_str() {
                "-h" | "--help" => want_help = true,
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
                "--once" => once = true,
                "--title" => title = Some(value("--title")?),
                "--prompt" => prompt = Some(value("--prompt")?),
                "--prompt-file" => prompt_file = Some(value("--prompt-file")?),
                "--permission-mode" => {
                    permission_mode = Some(parse_mode_flag(&value("--permission-mode")?)?)
                }
                "--model" => model = Some(parse_model_flag(&value("--model")?)?),
                "--issue-url" => {
                    issue_url = Some(parse_issue_url(&value("--issue-url")?, "--issue-url")?)
                }
                "--child" => child = true,
                "--successor" => successor = true,
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
                "--request" => request = Some(value("--request")?),
                "--message" => message = Some(value("--message")?),
                "--interrupt" => interrupt = true,
                "--immediate" => immediate = true,
                "--name" => config_name = Some(value("--name")?),
                "--command" => config_command = Some(value("--command")?),
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        // `agentium help [cmd]` is the same two pages spelled as a command, so
        // fold it into the `--help` path rather than giving it its own dispatch.
        let mut positionals = positionals.as_slice();
        if let Some(first) = positionals.first()
            && first == "help"
        {
            want_help = true;
            positionals = &positionals[1..];
        }
        let Some((name, rest)) = positionals.split_first() else {
            return Ok(Invocation::Usage);
        };
        // The help table is the CLI's list of commands: an unknown name is
        // rejected here, so nothing below dispatches one and a command added
        // without a help entry can't ship undocumented.
        let Some(spec) = help::lookup(name) else {
            return Err(format!("unknown command '{name}' (try `agentium --help`)").into());
        };
        // Help wins over the command's own argument checks (and over reading
        // a `--prompt-file`), so `agentium send --help` prints send's page
        // rather than erroring about the missing session.
        if want_help {
            return Ok(Invocation::CommandHelp(spec));
        }
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
        let command = if name == "approve" || name == "deny" {
            let decision = if name == "approve" {
                Decision::Approve
            } else {
                Decision::Deny
            };
            if interrupt && decision == Decision::Approve {
                return Err("--interrupt only applies to `deny` (deny and stop the turn)".into());
            }
            Command::Respond {
                session: arg(rest, 0, name)?,
                opts: ResponseOpts {
                    decision,
                    request,
                    message,
                    interrupt,
                },
            }
        } else if name == "watch" {
            Command::Watch {
                opts: WatchOpts {
                    once,
                    color: view.color,
                },
            }
        } else if name == "config" {
            Command::Config {
                action: ConfigAction::parse(rest, config_name, config_command)?,
            }
        } else if name == "spawn" {
            let relation = match (child, successor) {
                (true, true) => {
                    return Err("--child and --successor are exclusive: a spawned session \
                                either reports back to you or carries your work on"
                        .into());
                }
                (true, false) => SessionRelation::Child,
                (false, true) => SessionRelation::Successor,
                (false, false) => SessionRelation::None,
            };
            Command::Spawn {
                host: host.clone(),
                cwd: cwd.clone(),
                backend: backend.clone(),
                title,
                prompt,
                permission_mode,
                model,
                issue_url,
                relation,
                idempotency_key,
                allow_duplicate,
                wait,
                wait_timeout,
            }
        } else if name == "send" {
            Command::Send {
                session: arg(rest, 0, name)?,
                text: join_message(&rest[1..])?,
                immediate,
            }
        } else {
            parse_command(name, rest, view, case)?
        };

        // `--no-sync` is a read-only shortcut; a command that publishes or
        // streams can't honor it. Rejected here, before any engine or relay work
        // spins up, for the same reason `check_follow` is.
        if no_sync && command.needs_relay() {
            return Err(
                "--no-sync only applies to cache reads (list/show/log/grep/watch --once); a \
                 command that publishes — or `log --follow`/`watch`, which stream — needs the relay"
                    .into(),
            );
        }

        // `login`/`logout` manage the stored key themselves and `id` needs none,
        // so don't parse (and potentially reject on) whatever key is currently
        // configured.
        // `parse_nsec` hands back a `nostrdb_net::Pubkey`; the rest of the CLI
        // (and the `agentium_core` engine) speaks `nostrdb_net::Pubkey`. Both are
        // `[u8; 32]` newtypes, so bridge at this boundary and keep everything
        // downstream in enostr terms.
        let secret = match (&command, nsec) {
            (Command::Login { .. } | Command::Logout | Command::Id { .. }, _) => None,
            (_, Some(nsec)) => {
                let (sk, pk) = nostrdb_net::relay::sync::parse_nsec(&nsec)?;
                Some((sk, Pubkey::new(*pk.bytes())))
            }
            (_, None) => None,
        };

        // `grep` searches deleted sessions too unless `--deleted` narrows it to
        // only those: the conversation you're hunting for is as likely to be in
        // a session you've since closed, and `log` already opens deleted ones.
        // `list`/`watch` keep live-only as their default.
        let list_scope = match command {
            Command::Grep { .. } if !deleted => ListScope::All,
            _ => ListScope::from_flags(all, deleted),
        };
        let sync = !no_sync;

        Ok(Invocation::Run(Box::new(Cli {
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
        })))
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
        "interrupt" => Command::Interrupt {
            session: arg(rest, 0, name)?,
        },
        "mode" => Command::Mode {
            session: arg(rest, 0, name)?,
            mode: parse_mode_flag(&arg(rest, 1, name)?)?,
        },
        "link" => Command::Link {
            session: arg(rest, 0, name)?,
            issue_url: Some(parse_issue_url(&arg(rest, 1, name)?, "<issue-url>")?),
        },
        "unlink" => Command::Link {
            session: arg(rest, 0, name)?,
            issue_url: None,
        },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
        "id" => {
            if rest.is_empty() {
                return Err("id needs at least one <d-tag>".into());
            }
            Command::Id {
                d_tags: rest.to_vec(),
            }
        }
        // Unreachable in practice: `Cli::parse` rejects a name the help table
        // doesn't list, so only a documented command with no arm lands here.
        other => return Err(format!("`{other}` is documented but has no parser").into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::tests::view_all;

    #[test]
    fn show_selector_prefers_explicit_arg() {
        // An explicit positional is used verbatim. The $AGENTIUM_SESSION
        // fallback (only when none is given) is covered by
        // tests/show_renders.rs::show_defaults_to_agentium_session_env, which
        // sets the env on a child process — mutating it here would race the
        // other tests in this shared binary.
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
    fn id_command_takes_every_d_tag_and_needs_one() {
        match parse_command(
            "id",
            &["d-one".to_string(), "d-two".to_string()],
            view_all(),
            CaseMode::Smart,
        )
        .unwrap()
        {
            Command::Id { d_tags } => assert_eq!(d_tags, ["d-one", "d-two"]),
            _ => panic!("expected Id"),
        }
        assert!(parse_command("id", &[], view_all(), CaseMode::Smart).is_err());
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
            vec!["--nsec", TEST_NSEC, "--no-sync", "watch", "--once"],
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
            vec!["--nsec", TEST_NSEC, "--no-sync", "watch"],
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
    fn watch_carries_once_color_and_the_list_scope() {
        let cli = parse_cli(&[
            "--nsec", TEST_NSEC, "watch", "--once", "--color", "always", "--all",
        ])
        .expect("parses")
        .expect("a command");
        assert_eq!(cli.list_scope, ListScope::All);
        match cli.command {
            Command::Watch { opts } => {
                assert!(opts.once);
                assert_eq!(opts.color, ColorWhen::Always);
            }
            _ => panic!("expected watch"),
        }
        // Following by default.
        match parse_cli(&["--nsec", TEST_NSEC, "watch"])
            .expect("parses")
            .expect("a command")
            .command
        {
            Command::Watch { opts } => assert!(!opts.once),
            _ => panic!("expected watch"),
        }
    }

    /// Parse `args` (after `--nsec`) as a `send`, returning its fields.
    fn parse_send(args: &[&str]) -> Result<SendArgs> {
        let mut full = vec!["--nsec", TEST_NSEC];
        full.extend_from_slice(args);
        match parse_cli(&full)?.expect("a command").command {
            Command::Send {
                session,
                text,
                immediate,
            } => Ok(SendArgs {
                session,
                text,
                immediate,
            }),
            _ => panic!("expected Send"),
        }
    }

    /// The fields of a parsed `send`, for [`parse_send`].
    #[derive(Debug, PartialEq)]
    struct SendArgs {
        session: String,
        text: String,
        immediate: bool,
    }

    #[test]
    fn send_requires_session_and_joins_text() {
        // `send <sel> hey there` → the first positional is the selector, the rest
        // join into the message with single spaces (no quoting needed). Without
        // `--immediate` the send doesn't interrupt.
        assert_eq!(
            parse_send(&["send", "agentium:a-b-c", "hey", "there"]).unwrap(),
            SendArgs {
                session: "agentium:a-b-c".into(),
                text: "hey there".into(),
                immediate: false,
            }
        );
    }

    #[test]
    fn send_immediate_is_a_flag_anywhere_on_the_line() {
        // `--immediate` before the selector, between it and the text, or after
        // the text all set it, and never leak into the message.
        for args in [
            &["send", "--immediate", "agentium:a-b-c", "stop", "now"][..],
            &["send", "agentium:a-b-c", "--immediate", "stop", "now"][..],
            &["send", "agentium:a-b-c", "stop", "now", "--immediate"][..],
        ] {
            assert_eq!(
                parse_send(args).unwrap(),
                SendArgs {
                    session: "agentium:a-b-c".into(),
                    text: "stop now".into(),
                    immediate: true,
                },
                "{args:?}"
            );
        }
    }

    #[test]
    fn send_missing_session_and_empty_text_are_errors() {
        // No positionals at all → the missing-selector error (session is required;
        // there is no $AGENTIUM_SESSION default for `send`).
        assert!(parse_send(&["send"]).is_err());
        // A selector but no message words → the empty-message error.
        assert!(parse_send(&["send", "agentium:a-b-c"]).is_err());
        // A selector plus a whitespace-only quoted arg is also rejected.
        assert!(parse_send(&["send", "agentium:a-b-c", "   "]).is_err());
        // `--immediate` is no message.
        assert!(parse_send(&["send", "agentium:a-b-c", "--immediate"]).is_err());
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
    fn resume_requires_session() {
        // `resume <sel>` carries the selector verbatim, including a `deleted`
        // session's durable ref (resolution happens later, against the cache).
        let rest = ["agentium:a-b-c"].map(String::from);
        match parse_command("resume", &rest, view_all(), CaseMode::Smart).unwrap() {
            Command::Resume { session } => assert_eq!(session, "agentium:a-b-c"),
            _ => panic!("expected Resume"),
        }
        // No $AGENTIUM_SESSION default: resuming the session you're typing in
        // is meaningless, so a bare `resume` is the missing-argument error.
        assert!(parse_command("resume", &[], view_all(), CaseMode::Smart).is_err());
    }

    /// The error text of a failed [`parse_cli`].
    fn parse_err(args: &[&str]) -> String {
        match parse_cli(args) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("{args:?} should fail to parse"),
        }
    }

    #[test]
    fn unknown_command_and_flag_are_named_in_the_error() {
        let err = parse_err(&["--nsec", TEST_NSEC, "frobnicate"]);
        assert!(err.contains("unknown command 'frobnicate'"), "{err}");

        let err = parse_err(&["--nsec", TEST_NSEC, "--frobnicate", "list"]);
        assert!(err.contains("unknown flag '--frobnicate'"), "{err}");

        // A value flag at the end of argv has nothing to consume.
        let err = parse_err(&["--nsec", TEST_NSEC, "list", "--host"]);
        assert!(err.contains("--host needs a value"), "{err}");
    }

    /// `--help` picks the overview or one command's page depending on whether a
    /// command was named, in either spelling and either order — and `--help`
    /// wins over the command's own argument checking, so `agentium send
    /// --help` prints send's page rather than erroring about the missing
    /// session.
    #[test]
    fn help_resolves_to_a_page() {
        assert_eq!(help_of(&[]), Some(None));
        assert_eq!(help_of(&["--nsec", TEST_NSEC]), Some(None));
        assert_eq!(help_of(&["--help"]), Some(None));
        assert_eq!(help_of(&["-h"]), Some(None));
        assert_eq!(help_of(&["help"]), Some(None));
        assert_eq!(help_of(&["spawn", "--help"]), Some(Some("spawn")));
        assert_eq!(help_of(&["--help", "spawn"]), Some(Some("spawn")));
        assert_eq!(help_of(&["help", "spawn"]), Some(Some("spawn")));
        assert_eq!(help_of(&["send", "--help"]), Some(Some("send")));
        assert_eq!(help_of(&["deny", "-h"]), Some(Some("deny")));
        assert_eq!(help_of(&["config", "add", "--help"]), Some(Some("config")));
        // Help is answered before `--prompt-file` is read, so this neither
        // blocks on stdin nor fails on a missing file.
        assert_eq!(
            help_of(&["spawn", "--prompt-file", "/nonexistent/prompt", "--help"]),
            Some(Some("spawn"))
        );
        // Without it, the same line runs.
        assert!(help_of(&["--nsec", TEST_NSEC, "list"]).is_none());
    }

    /// An unrecognised command is rejected by name, whether or not help was
    /// asked for — `agentium frobnicate --help` has no page to print.
    #[test]
    fn unknown_commands_are_rejected_even_with_help() {
        for args in [&["frobnicate"][..], &["frobnicate", "--help"][..]] {
            let err = parse_err(args);
            assert!(err.contains("unknown command 'frobnicate'"), "{err}");
        }
    }

    /// Every command the help documents is one the parser builds, so a help
    /// entry can't drift off a command that was renamed or removed. The
    /// converse — a command with no help entry — can't happen: `Cli::parse`
    /// resolves the name through the help table before dispatching.
    #[test]
    fn every_documented_command_parses() {
        for cmd in help::COMMANDS {
            // Two positionals cover every command's required arguments; a
            // command that rejects them errors about *that*, and only the
            // fallback arm means the name has no parser at all.
            let res = parse_cli(&["--nsec", TEST_NSEC, cmd.name, "agentium:a-b-c", "plan"]);
            if let Err(e) = res {
                assert!(
                    !e.to_string().contains("has no parser"),
                    "`{}` is documented but has no parser",
                    cmd.name
                );
            }
        }
    }

    /// The autowork chain hands off with exactly this line, through the
    /// installed binary: if it stopped parsing, the chain would die at its
    /// next handoff.
    #[test]
    fn the_autowork_handoff_line_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt");
        std::fs::write(&path, "/autowork\n\nContinue the chain.\n").unwrap();
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "spawn",
            "--title",
            "headway:notedeck/x",
            "--issue-url",
            "headway:notedeck/x",
            "--wait",
            "--json",
            "--prompt-file",
            path.to_str().unwrap(),
        ])
        .expect("parses")
        .expect("a command");
        assert!(cli.json);
        match cli.command {
            Command::Spawn {
                title,
                issue_url,
                prompt,
                wait,
                ..
            } => {
                assert_eq!(title.as_deref(), Some("headway:notedeck/x"));
                assert_eq!(issue_url.as_deref(), Some("headway:notedeck/x"));
                assert_eq!(prompt.as_deref(), Some("/autowork\n\nContinue the chain."));
                assert!(wait);
            }
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn author_overrides_the_read_identity() {
        let hex = "ab".repeat(32);
        let cli = parse_cli(&["--nsec", TEST_NSEC, "--author", &hex, "list"])
            .expect("parses")
            .expect("a command");
        assert_eq!(cli.author, Some(Pubkey::new([0xab; 32])));

        // Unset, the read identity falls back to the signer later (in `run`).
        let cli = parse_cli(&["--nsec", TEST_NSEC, "list"])
            .expect("parses")
            .expect("a command");
        assert!(cli.author.is_none());

        // An unparseable key is refused up front rather than reading nobody.
        assert!(parse_cli(&["--nsec", TEST_NSEC, "--author", "nope", "list"]).is_err());
    }

    #[test]
    fn deleted_and_all_pick_the_list_scope() {
        let scope = |flags: &[&str]| {
            let mut argv = vec!["--nsec", TEST_NSEC];
            argv.extend_from_slice(flags);
            argv.push("list");
            parse_cli(&argv)
                .expect("parses")
                .expect("a command")
                .list_scope
        };
        assert_eq!(scope(&[]), ListScope::Live);
        assert_eq!(scope(&["--deleted"]), ListScope::Deleted);
        assert_eq!(scope(&["--all"]), ListScope::All);
        // `--all` is a superset of `--deleted`, so it wins in either order.
        assert_eq!(scope(&["--deleted", "--all"]), ListScope::All);
        assert_eq!(scope(&["--all", "--deleted"]), ListScope::All);
    }

    /// `grep` searches live and deleted sessions by default; `--deleted` still
    /// narrows it to the deleted ones.
    #[test]
    fn grep_searches_every_session_by_default() {
        let scope = |flags: &[&str]| {
            let mut argv = vec!["--nsec", TEST_NSEC];
            argv.extend_from_slice(flags);
            argv.extend(["grep", "x"]);
            parse_cli(&argv)
                .expect("parses")
                .expect("a command")
                .list_scope
        };
        assert_eq!(scope(&[]), ListScope::All);
        assert_eq!(scope(&["--all"]), ListScope::All);
        assert_eq!(scope(&["--deleted"]), ListScope::Deleted);
    }

    #[test]
    fn approve_and_deny_carry_their_flags() {
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "--request",
            "aaaa1111",
            "--message",
            "go ahead",
            "approve",
            "agentium:a-b-c",
        ])
        .unwrap()
        .unwrap();
        match cli.command {
            Command::Respond { session, opts } => {
                assert_eq!(session, "agentium:a-b-c");
                assert_eq!(opts.decision, Decision::Approve);
                assert_eq!(opts.request.as_deref(), Some("aaaa1111"));
                assert_eq!(opts.message.as_deref(), Some("go ahead"));
                assert!(!opts.interrupt);
            }
            _ => panic!("expected Respond"),
        }

        // A bare deny answers the newest request, with no reason and no stop.
        let cli = parse_cli(&["--nsec", TEST_NSEC, "deny", "agentium:a-b-c"])
            .unwrap()
            .unwrap();
        match cli.command {
            Command::Respond { opts, .. } => {
                assert_eq!(opts.decision, Decision::Deny);
                assert!(opts.request.is_none() && opts.message.is_none() && !opts.interrupt);
            }
            _ => panic!("expected Respond"),
        }

        let cli = parse_cli(&["--nsec", TEST_NSEC, "--interrupt", "deny", "agentium:a-b-c"])
            .unwrap()
            .unwrap();
        match cli.command {
            Command::Respond { opts, .. } => assert!(opts.interrupt),
            _ => panic!("expected Respond"),
        }
    }

    #[test]
    fn approve_rejects_interrupt_and_a_missing_session() {
        // Stopping the turn is a deny; an approve that also stops it is a
        // contradiction, so refuse it rather than pick one.
        assert!(
            parse_cli(&[
                "--nsec",
                TEST_NSEC,
                "--interrupt",
                "approve",
                "agentium:a-b-c"
            ])
            .is_err()
        );
        // The selector is required (no $AGENTIUM_SESSION default: answering your
        // own permission request from inside the turn it blocks makes no sense).
        assert!(parse_cli(&["--nsec", TEST_NSEC, "approve"]).is_err());
        assert!(parse_cli(&["--nsec", TEST_NSEC, "deny"]).is_err());
    }

    #[test]
    fn mode_normalizes_aliases_and_rejects_unknown_modes() {
        for (typed, canonical) in [
            ("plan", "plan"),
            ("manual", "default"),
            ("acceptEdits", "accept_edits"),
            ("bypassPermissions", "bypass"),
            ("auto", "auto"),
        ] {
            let cli = parse_cli(&["--nsec", TEST_NSEC, "mode", "agentium:a-b-c", typed])
                .unwrap()
                .unwrap();
            match cli.command {
                Command::Mode { session, mode } => {
                    assert_eq!(session, "agentium:a-b-c");
                    assert_eq!(mode, canonical, "'{typed}' should normalize");
                }
                _ => panic!("expected Mode"),
            }
        }
        // The host would read an unknown mode as `default`, so it must fail here.
        let Err(err) = parse_cli(&["--nsec", TEST_NSEC, "mode", "agentium:a-b-c", "yolo"]) else {
            panic!("an unknown mode must be rejected");
        };
        assert!(err.to_string().contains("yolo"), "{err}");
        // Both positionals are required.
        assert!(parse_cli(&["--nsec", TEST_NSEC, "mode", "agentium:a-b-c"]).is_err());
    }

    /// `link <sel> <issue-url>` checks the URL like `spawn --issue-url` does;
    /// `unlink <sel>` clears. Both need the relay, since only the host can
    /// apply them.
    #[test]
    fn link_and_unlink_parse() {
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "link",
            "agentium:a-b-c",
            "headway:gamedev/wall-harsh-hip",
        ])
        .unwrap()
        .unwrap();
        match cli.command {
            Command::Link { session, issue_url } => {
                assert_eq!(session, "agentium:a-b-c");
                assert_eq!(issue_url.as_deref(), Some("headway:gamedev/wall-harsh-hip"));
            }
            _ => panic!("expected Link"),
        }

        let cli = parse_cli(&["--nsec", TEST_NSEC, "unlink", "agentium:a-b-c"])
            .unwrap()
            .unwrap();
        match cli.command {
            Command::Link { session, issue_url } => {
                assert_eq!(session, "agentium:a-b-c");
                assert_eq!(issue_url, None);
            }
            _ => panic!("expected Link"),
        }

        // A bare word-id names no board, so it is refused with the routable form.
        let Err(err) = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "link",
            "agentium:a-b-c",
            "wall-harsh-hip",
        ]) else {
            panic!("a bare word-id must be refused");
        };
        assert!(
            err.to_string().contains("headway:<board>/<word-id>"),
            "{err}"
        );
        // Both positionals are required for link, the selector for unlink.
        assert!(parse_cli(&["--nsec", TEST_NSEC, "link", "agentium:a-b-c"]).is_err());
        assert!(parse_cli(&["--nsec", TEST_NSEC, "unlink"]).is_err());
        assert!(
            parse_cli(&["--nsec", TEST_NSEC, "--no-sync", "unlink", "agentium:a-b-c"]).is_err(),
            "--no-sync must be refused: only the host can apply it"
        );
    }

    #[test]
    fn permission_commands_need_the_relay() {
        for cmd in [
            vec![
                "--nsec",
                TEST_NSEC,
                "--no-sync",
                "approve",
                "agentium:a-b-c",
            ],
            vec!["--nsec", TEST_NSEC, "--no-sync", "deny", "agentium:a-b-c"],
            vec![
                "--nsec",
                TEST_NSEC,
                "--no-sync",
                "mode",
                "agentium:a-b-c",
                "plan",
            ],
        ] {
            assert!(
                parse_cli(&cmd).is_err(),
                "--no-sync must be refused for {cmd:?}"
            );
        }
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

    // -- `agentium spawn`: flag capture, target defaulting, wait/json shape -----

    /// `[7u8; 32]` as an nsec — passed to `Cli::parse` so key resolution is
    /// deterministic (overrides any stored/env key on the test machine).
    const TEST_NSEC: &str = "nsec1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursl6edet";

    /// Parse a full arg vector (without the program name) through
    /// [`Cli::parse`]: `Some` for a run, `None` for either help page.
    fn parse_cli(args: &[&str]) -> Result<Option<Cli>> {
        Ok(match Cli::parse(args.iter().map(|s| s.to_string()))? {
            Invocation::Run(cli) => Some(*cli),
            Invocation::Usage | Invocation::CommandHelp(_) => None,
        })
    }

    /// The help page a command line asks for — `Some(None)` for the overview,
    /// `Some(Some(name))` for a command's page — or `None` when it asks to run.
    fn help_of(args: &[&str]) -> Option<Option<&'static str>> {
        match Cli::parse(args.iter().map(|s| s.to_string())).expect("parses") {
            Invocation::Run(_) => None,
            Invocation::Usage => Some(None),
            Invocation::CommandHelp(cmd) => Some(Some(cmd.name)),
        }
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

    /// `--child` and `--successor` each pick their relation, neither leaves it
    /// `none`, and both together are refused: a session can't both report back
    /// to its spawner and carry its work on.
    #[test]
    fn spawn_relation_flags_are_exclusive() {
        let relation_of = |flags: &[&str]| {
            let mut args = vec!["--nsec", TEST_NSEC, "spawn"];
            args.extend_from_slice(flags);
            match parse_cli(&args).map(|c| c.unwrap().command) {
                Ok(Command::Spawn { relation, .. }) => Ok(relation),
                Ok(_) => panic!("expected Spawn"),
                Err(e) => Err(e.to_string()),
            }
        };
        assert_eq!(relation_of(&[]), Ok(SessionRelation::None));
        assert_eq!(relation_of(&["--child"]), Ok(SessionRelation::Child));
        assert_eq!(
            relation_of(&["--successor"]),
            Ok(SessionRelation::Successor)
        );
        let err = relation_of(&["--child", "--successor"]).unwrap_err();
        assert!(err.contains("exclusive"), "{err}");
    }

    /// `--issue-url` keeps a URI verbatim (trimmed) and refuses anything without
    /// a scheme — a bare headway word-id names no board, so it couldn't be
    /// opened from the session.
    #[test]
    fn spawn_issue_url_must_be_a_uri() {
        let cli = parse_cli(&[
            "--nsec",
            TEST_NSEC,
            "--issue-url",
            " headway:dave/receive-east-neutral ",
            "spawn",
        ])
        .unwrap()
        .unwrap();
        match cli.command {
            Command::Spawn { issue_url, .. } => {
                assert_eq!(
                    issue_url.as_deref(),
                    Some("headway:dave/receive-east-neutral")
                );
            }
            _ => panic!("expected Spawn"),
        }

        for bad in [
            "",
            "receive-east-neutral",
            "dave/receive-east-neutral",
            "headway:",
            "1x:y",
        ] {
            let err = parse_cli(&["--nsec", TEST_NSEC, "--issue-url", bad, "spawn"])
                .err()
                .unwrap_or_else(|| panic!("{bad:?} should be refused"))
                .to_string();
            assert!(err.contains("headway:<board>/<word-id>"), "{bad:?}: {err}");
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

    /// `--model` is captured trimmed and otherwise verbatim (the backend owns the
    /// names), absent leaves it `None`, and a value no backend could take is
    /// refused before anything is published.
    #[test]
    fn spawn_model_is_passed_through_and_blank_is_refused() {
        let model_of = |args: &[&str]| match parse_cli(args).unwrap().unwrap().command {
            Command::Spawn { model, .. } => model,
            _ => panic!("expected Spawn"),
        };
        assert_eq!(
            model_of(&["--nsec", TEST_NSEC, "--model", " claude-opus-5-5 ", "spawn"]).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            model_of(&["--nsec", TEST_NSEC, "spawn", "--model", "sonnet"]).as_deref(),
            Some("sonnet")
        );
        assert_eq!(model_of(&["--nsec", TEST_NSEC, "spawn"]), None);

        for bad in ["", "  ", "opus please"] {
            let Err(err) = parse_cli(&["--nsec", TEST_NSEC, "--model", bad, "spawn"]) else {
                panic!("--model '{bad}' must be refused");
            };
            assert!(err.to_string().contains("--model"), "error: {err}");
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
}
