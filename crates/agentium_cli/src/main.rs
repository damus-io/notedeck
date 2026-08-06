//! `agentium` — a CLI to query and control Dave agentic sessions over a running
//! notedeck's embedded relay.
//!
//! A sibling to [`headway_cli`] and [`notebook_cli`], but with the data layer
//! inverted: rather than folding events itself, this CLI drives the
//! platform-neutral [`agentium_core`] engine, which owns its own nostrdb cache
//! and a background relay connect/sync loop that streams this identity's
//! PNS-encrypted session corpus into that cache. The shared [`relay_sync`] crate
//! still owns the incidental plumbing — the stored signing key, the cache
//! directory convention, and `login`/`logout`. This file is just the command
//! surface: argument parsing and (in later cards) rendering.
//!
//! This is the scaffold: it proves the pipeline — key resolution, engine open,
//! relay connect, and an initial sync — compiles and connects end to end. The
//! `list` command's session-table rendering lands in a follow-up
//! (dave#lunch-twice-below).

use std::env;
use std::process::ExitCode;
use std::time::Duration;

use agentium_core::Engine;
use enostr::Pubkey;
use nostrdb::Transaction;

use relay_sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/agentium-cli` on Linux).
const APP: &str = "agentium-cli";

/// How long a read command waits for the relay's initial reconcile to stream
/// session state into the cache before reading it. Bounded so an empty or
/// unreachable relay falls through to the cache promptly rather than hanging.
const SYNC_SETTLE: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`agentium list | head`) instead of
    // panicking in println! on EPIPE.
    relay_sync::reset_sigpipe();
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
        Command::Login { nsec } => return relay_sync::login(nsec, APP),
        Command::Logout => return relay_sync::logout(APP),
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
    // it over the cache dir relay_sync manages so co-located tools share one
    // cache; the engine takes a clone and drives sync itself. Opening also
    // registers the device key with ndb so its ingest threads can decrypt this
    // identity's inbound kind-1080 PNS envelopes into queryable inner events.
    let ndb = relay_sync::open_ndb(db.as_deref(), APP)?;
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

    // Give the initial reconcile a bounded moment to land session state before
    // we read. Times out cleanly when the relay is empty or unreachable.
    if let Ok(mut watch) = engine.watch_sessions() {
        let _ = tokio::time::timeout(SYNC_SETTLE, watch.changed()).await;
    }

    // `--author` overrides whose sessions we read; it defaults to the signer.
    let read_pk = cli.author.unwrap_or(self_pk);

    match cli.command {
        Command::List => cmd_list(&engine, &read_pk, cli.json)?,
        Command::Login { .. } | Command::Logout => unreachable!("handled above"),
    }

    Ok(())
}

/// `agentium list` — enumerate this identity's sessions.
///
/// Scaffold stub: it exercises the read path (a transaction over the engine's
/// synced cache, scoped to `author`) so the whole pipeline is proven end to end,
/// but rendering the session table — id, title, status — lands in a follow-up
/// (dave#lunch-twice-below). Prints nothing for now.
fn cmd_list(engine: &Engine, author: &Pubkey, _as_json: bool) -> Result<()> {
    let txn = Transaction::new(engine.ndb())?;
    let _sessions =
        agentium_core::session_loader::load_session_states_for_author(engine.ndb(), &txn, author);
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
        relay_sync::write_config(APP, "relay", &url)?;
        return Ok(url);
    }
    Ok(env::var("AGENTIUM_RELAY")
        .ok()
        .or_else(|| relay_sync::read_config(APP, "relay"))
        .unwrap_or_else(|| relay_sync::DEFAULT_RELAY.to_string()))
}

/// Resolve the nostrdb cache dir. Precedence: `--db > stored config > default`
/// (`open_ndb`'s `<data-dir>/agentium-cli`). Passing `--db` persists it, same as
/// `--relay`; `None` lets `open_ndb` pick the default.
fn resolve_db(flag: Option<String>) -> Result<Option<String>> {
    if let Some(path) = flag {
        relay_sync::write_config(APP, "db", &path)?;
        return Ok(Some(path));
    }
    Ok(relay_sync::read_config(APP, "db"))
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
            .or_else(|| relay_sync::stored_nsec(APP));
        let mut relay = None;
        let mut db = None;
        let mut author = None;
        let mut json = false;
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
        let secret = match (&command, nsec) {
            (Command::Login { .. } | Command::Logout, _) => None,
            (_, Some(nsec)) => Some(relay_sync::parse_nsec(&nsec)?),
            (_, None) => None,
        };

        Ok(Some(Cli {
            secret,
            author,
            relay,
            db,
            json,
            command,
        }))
    }
}

fn parse_command(name: &str, rest: &[String]) -> Result<Command> {
    Ok(match name {
        "list" => Command::List,
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
    list              List this identity's sessions (--json for machine output)
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
    -h, --help        Print this help",
        DEFAULT_RELAY = relay_sync::DEFAULT_RELAY,
    );
}
