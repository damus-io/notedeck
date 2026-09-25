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
mod interrupt;
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
use nostrdb_net::Pubkey;
use regex::Regex;

use nostrdb_net::relay::sync::Result;

use config_cmd::{ConfigAction, ConfigFilters, cmd_config};
use grep::{CaseMode, cmd_grep, compile_pattern};
use interrupt::cmd_interrupt;
use list::{ListFilters, ListScope, cmd_list};
use log::{cmd_follow, cmd_log};
use permission::{Decision, ResponseOpts, cmd_mode, cmd_respond};
use resume::cmd_resume;
use send::cmd_send;
use show::cmd_show;
use spawn::{SpawnOpts, cmd_spawn};
use transcript::{ColorWhen, MessageView, PagerMode};
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
    /// List, show, add, edit or remove run configs (kind-31991): the named
    /// shell commands Dave's run bar launches in a session's host+cwd.
    Config {
        action: ConfigAction,
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
            // Likewise a live dashboard; a single `--once` frame is a cache read.
            Command::Watch { opts } => !opts.once,
            Command::Config { action } => action.publishes(),
            Command::Resume { .. }
            | Command::Send { .. }
            | Command::Spawn { .. }
            | Command::Interrupt { .. }
            | Command::Respond { .. }
            | Command::Mode { .. }
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
        Command::Respond { session, opts } => {
            cmd_respond(&engine, &read_pk, &session, &opts, cli.json).await?
        }
        Command::Mode { session, mode } => {
            cmd_mode(&engine, &read_pk, &session, &mode, cli.json).await?
        }
        Command::Config { action } => {
            let filters = ConfigFilters {
                host: filters.host,
                cwd: filters.cwd,
            };
            cmd_config(&engine, &read_pk, &secret, &action, &filters, cli.json).await?
        }
        Command::Login { .. } | Command::Logout => unreachable!("handled above"),
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
        let mut idempotency_key = None;
        let mut allow_duplicate = false;
        let mut wait = false;
        let mut wait_timeout = None;
        // `approve`/`deny` flags: which pending request, the reply text, and
        // (deny only) whether to stop the turn too.
        let mut request = None;
        let mut message = None;
        let mut interrupt = false;
        // `config add`/`edit` fields.
        let mut config_name = None;
        let mut config_command = None;
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
                "--once" => once = true,
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
                "--request" => request = Some(value("--request")?),
                "--message" => message = Some(value("--message")?),
                "--interrupt" => interrupt = true,
                "--name" => config_name = Some(value("--name")?),
                "--command" => config_command = Some(value("--command")?),
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
                "--no-sync only applies to cache reads (list/show/log/grep/watch --once); a \
                 command that publishes — or `log --follow`/`watch`, which stream — needs the relay"
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
        "mode" => Command::Mode {
            session: arg(rest, 0, name)?,
            mode: parse_mode_flag(&arg(rest, 1, name)?)?,
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
    watch             A live dashboard of every session: status, title, host,
                      cwd, backend, mode and last activity (which counts
                      streamed messages, not just status changes), sessions
                      waiting on you first. Redraws in place on a terminal until
                      Ctrl-C; into a pipe, prints each changed frame. Takes the
                      list filters and --color; --once prints one frame and
                      exits. Lines clip to $COLUMNS, else the terminal width;
                      piped frames are never clipped.
    show [session]    Show one session's detail: its state, the run-configs on
                      its host+cwd, its latest usage, and a conversation summary
                      (message count + each pending permission request, with
                      the id approve/deny --request takes). Takes any
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
    approve <session> / deny <session>
                      Answer a live session's pending permission request — by
                      default the newest; --request picks another. `show` lists
                      what is pending. An AskUserQuestion request can be denied
                      but not approved (it needs answers; use Dave). --json emits
                      {{ session, event_id, perm_id, … }} on one line. The host
                      matches the answer to the request it holds in memory: if it
                      restarted since, the answer is ignored and the request stays
                      pending, with no error.
    mode <session> <mode>
                      Change a live session's permission mode — the CLI companion
                      to Ctrl+M in Dave: default (aka manual) | plan |
                      accept_edits | auto | bypass. --json emits {{ session,
                      event_id, mode }}.
    config [list]     List run configs — the named shell commands Dave's run
                      bar launches in a host+cwd — on every host, grouped host →
                      cwd. --host/--cwd narrow it (substring, like list); --json
                      emits the flat array.
    config show <config>
                      Show one run config. <config> is an id prefix (the 8-char
                      ids `config list` prints) or an exact name; --host/--cwd
                      narrow what it resolves against.
    config add --name <n> --command <cmd>
                      Register a run config. It lands on --host/--cwd (exact
                      values here), which default to the current session's
                      ($AGENTIUM_SESSION). --json emits {{ action, event_id,
                      config }} on one line, as do edit and rm.
    config edit <config> [--name <n>] [--command <cmd>]
                      Rename a run config or change its command; its id stays.
    config rm <config>
                      Delete a run config (Dave kills it if it is running).
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
                      grep/watch --once). Rejected for commands that publish or
                      stream.

  list filters (case-insensitive):
    --host <h>        Only sessions whose host contains <h>
    --status <s>      Only sessions with exactly this status
                      (idle|working|needs_input|error|done|pending)
    --cwd <c>         Only sessions whose working dir contains <c>
    --backend <b>     Only sessions whose backend contains <b>
    --deleted         Show only soft-deleted (tombstoned) sessions
    --all             Show live and deleted sessions together
    --once            (watch) Print one frame and exit instead of following

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

  approve/deny options:
    --request <id>    The pending request to answer, by perm-id prefix (the
                      8-digit ids `show` prints). Default: the newest.
    --message <text>  Reply text sent with the decision — a deny reason, or a
                      note with an approve — which the agent sees
    --interrupt       (deny only) Deny and stop the turn, rather than letting
                      the agent carry on without the tool

    -h, --help        Print this help",
        DEFAULT_RELAY = nostrdb_net::relay::sync::DEFAULT_RELAY,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::tests::view_all;

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

    /// Parse a full arg vector (without the program name) through [`Cli::parse`].
    fn parse_cli(args: &[&str]) -> Result<Option<Cli>> {
        Cli::parse(args.iter().map(|s| s.to_string()))
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
