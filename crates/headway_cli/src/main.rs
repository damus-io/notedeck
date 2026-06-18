//! `headway` — a CLI for reading and mutating a Headway board against a running
//! notedeck's embedded relay.
//!
//! The CLI keeps its **own** nostrdb as a cache. Each run it reconciles that
//! cache against the relay with NIP-77 negentropy — pulling the events the relay
//! has that it lacks and pushing the ones it holds that the relay lacks — then
//! folds the board locally with the pure [`headway`] reducer. Edits forward the
//! events they produce back to the relay so the running app sees the change.

mod relay;

use std::collections::HashSet;
use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use enostr::{NoteId, Pubkey};
use negentropy::{Id, NegentropyStorageVector};
use nostrdb::{Config, Ndb, Transaction};
use serde_json::json;

use headway::event::{self, BoardView, CardView};
use headway::store::{self, BoardAction, Publisher};

use relay::Relay;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Default URL of notedeck's embedded relay (see `--relay-bind`, default
/// `127.0.0.1:6677`).
const DEFAULT_RELAY: &str = "ws://127.0.0.1:6677";

/// How many event ids to request per `REQ` when pulling reconciled events down.
/// The relay caps a single `REQ`'s stored replay, so we fetch in chunks under
/// that cap.
const ID_FETCH_CHUNK: usize = 300;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// A parsed command. Card arguments are still raw strings here; they're resolved
/// against the board once it's folded.
enum Command {
    Show,
    Seed,
    Add {
        title: String,
        col: Option<String>,
    },
    Move {
        card: String,
        col: String,
        row: Option<usize>,
    },
    Title {
        card: String,
        title: String,
    },
    Desc {
        card: String,
        text: String,
    },
    Label {
        card: String,
        labels: Vec<String>,
    },
    Delete {
        card: String,
    },
    Archive {
        card: String,
    },
    Restore {
        card: String,
    },
}

async fn run() -> Result<()> {
    let cli = match Cli::parse(env::args().skip(1))? {
        Some(cli) => cli,
        None => {
            print_usage();
            return Ok(());
        }
    };

    // The author whose board we read/write: an explicit override, else the
    // signing key's own pubkey.
    let author = match (&cli.author, &cli.secret) {
        (Some(pk), _) => *pk,
        (None, Some((_, pk))) => *pk,
        (None, None) => return Err("need --nsec to sign, or --author to read a board".into()),
    };

    let ndb = open_ndb(cli.db.as_deref())?;

    // The relay is best-effort: it's how we sync fresh events in and fan edits
    // back out to the running app, but the CLI keeps its own nostrdb cache and
    // folds the board from that. So if nothing is listening we carry on offline
    // against the cache rather than aborting — `show` still reads, `seed`/edits
    // still ingest locally (they just don't reach the app until a relay is up).
    let mut relay = match Relay::connect(&cli.relay).await {
        Ok(relay) => Some(relay),
        Err(e) => {
            eprintln!("warning: {e}");
            eprintln!("working offline against the local cache (--relay to point elsewhere)");
            None
        }
    };

    // When a relay is reachable, reconcile both ways so the local cache and the
    // app converge regardless of which side an edit happened on. NIP-77
    // negentropy gives us the set difference in O(difference) — without
    // transferring the board — and from it:
    //
    //   pull  — `REQ` the ids the relay has that we lack, and ingest them, so we
    //           fold from the relay's latest state.
    //   push  — forward the events we hold that the relay lacks. That's how edits
    //           made while offline reach the app on the next connected run;
    //           without this they'd stay stranded in the cache.
    if let Some(relay) = &mut relay {
        let filter = json!({ "kinds": event::HEADWAY_KINDS, "authors": [author.hex()] });
        match relay
            .reconcile(&filter.to_string(), local_set(&ndb, &author)?)
            .await
        {
            Ok(diff) => {
                // Pull missing events by id. The relay caps a single `REQ`'s
                // stored replay, so fetch in chunks rather than one oversized
                // filter.
                for chunk in diff.need.chunks(ID_FETCH_CHUNK) {
                    let ids: Vec<String> = chunk.iter().map(hex::encode).collect();
                    let received = relay
                        .sync_into(&ndb, &json!({ "ids": ids }).to_string())
                        .await?;
                    await_ingest(&ndb, &received).await;
                }

                // Push events the relay is missing (e.g. edits made offline).
                // Best-effort: a rejected flush (or a dropped connection mid-push)
                // mustn't abort the command — `show` should still print.
                let have: HashSet<[u8; 32]> = diff.have.iter().copied().collect();
                let pending = frames_where(&ndb, &author, |id| have.contains(id));
                if !pending.is_empty() {
                    match relay.publish(&pending).await {
                        Ok(()) => {
                            eprintln!("flushed {} local event(s) to the relay", pending.len())
                        }
                        Err(e) => eprintln!("warning: couldn't flush local events: {e}"),
                    }
                }
            }
            // Sync is best-effort. A relay that doesn't speak NIP-77 (an older
            // notedeck, or a plain NIP-01 relay) can't reconcile — fall back to a
            // full NIP-01 sync rather than failing or, worse, hanging. If even
            // that fails, warn and carry on against the cache.
            Err(e) => {
                eprintln!("warning: negentropy reconcile unavailable: {e}");
                if let Err(e) = fallback_sync(relay, &ndb, &author, &filter).await {
                    eprintln!("warning: fallback sync failed: {e}");
                }
            }
        }
    }

    let board = cli.board;
    let as_json = cli.json;
    let secret = cli.secret.map(|(s, _)| s);

    match cli.command {
        Command::Show => match load_board(&ndb, &author, &board) {
            Some(view) => print_board(&view, as_json),
            None if as_json => println!("null"),
            None => println!(
                "no board '{}' for {} — run `headway seed`",
                board,
                author.hex()
            ),
        },

        Command::Seed => {
            let secret = secret.ok_or("seed needs --nsec to sign")?;
            if load_board(&ndb, &author, &board).is_some() {
                return Err(format!("board '{board}' already exists").into());
            }
            let mut sink = Collect::default();
            store::seed_default_board(&ndb, &author, &secret, &board, &mut sink);
            let n = sink.0.len();
            publish(&mut relay, sink).await?;
            println!(
                "seeded board '{board}' ({n} events){}",
                offline_note(&relay)
            );
        }

        edit => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let view = load_board(&ndb, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let action = build_action(&view, edit)?;

            let mut sink = Collect::default();
            store::apply(&ndb, &board, &view, &author, &secret, action, &mut sink);
            if sink.0.is_empty() {
                return Err("action produced no events (unknown card or column?)".into());
            }
            let n = sink.0.len();
            publish(&mut relay, sink).await?;
            println!("ok ({n} events){}", offline_note(&relay));
        }
    }

    Ok(())
}

/// Translate a resolved [`Command`] into a [`BoardAction`], resolving card and
/// column arguments against `view`.
fn build_action(view: &BoardView, command: Command) -> Result<BoardAction> {
    Ok(match command {
        Command::Add { title, col } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            BoardAction::AddCard { col, title }
        }
        Command::Move { card, col, row } => {
            let card = resolve_card(view, &card)?;
            let to_col = resolve_col(view, &col)?;
            let to_row = row.unwrap_or(view.columns[to_col].cards.len());
            BoardAction::MoveCard {
                card,
                to_col,
                to_row,
            }
        }
        Command::Title { card, title } => BoardAction::EditTitle {
            card: resolve_card(view, &card)?,
            title,
        },
        Command::Desc { card, text } => BoardAction::EditDescription {
            card: resolve_card(view, &card)?,
            description: text,
        },
        Command::Label { card, labels } => BoardAction::SetLabels {
            card: resolve_card(view, &card)?,
            labels,
        },
        Command::Delete { card } => BoardAction::DeleteCard {
            card: resolve_card(view, &card)?,
        },
        Command::Archive { card } => BoardAction::ArchiveCard {
            card: resolve_card(view, &card)?,
        },
        Command::Restore { card } => BoardAction::RestoreCard {
            card: resolve_card(view, &card)?,
        },
        Command::Show | Command::Seed => unreachable!("handled before build_action"),
    })
}

// ---------------------------------------------------------------------------
// nostrdb plumbing
// ---------------------------------------------------------------------------

/// Open (creating if needed) the CLI's own nostrdb cache.
fn open_ndb(db: Option<&str>) -> Result<Ndb> {
    let path = match db {
        Some(p) => std::path::PathBuf::from(p),
        None => dirs::data_dir()
            .ok_or("no data dir; pass --db <path>")?
            .join("headway-cli"),
    };
    std::fs::create_dir_all(&path)?;
    let path = path.to_str().ok_or("db path is not valid utf-8")?;
    Ok(Ndb::new(path, &Config::new())?)
}

fn load_board(ndb: &Ndb, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    let txn = Transaction::new(ndb).ok()?;
    event::load_board(ndb, &txn, author, board_id)
}

/// nostrdb ingests on background threads, so a freshly-synced event isn't
/// queryable immediately. Poll until every received id is present (ids already
/// in the cache resolve at once; new ones once the ingester commits them), or a
/// short deadline elapses.
async fn await_ingest(ndb: &Ndb, ids: &[[u8; 32]]) {
    if ids.is_empty() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !all_present(ndb, ids) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn all_present(ndb: &Ndb, ids: &[[u8; 32]]) -> bool {
    let Ok(txn) = Transaction::new(ndb) else {
        return false;
    };
    ids.iter().all(|id| ndb.get_note_by_id(&txn, id).is_ok())
}

/// The sealed negentropy set of `author`'s cached headway events, keyed by
/// `(created_at, id)`. This is the local side handed to [`Relay::reconcile`].
fn local_set(ndb: &Ndb, author: &Pubkey) -> Result<NegentropyStorageVector> {
    let txn = Transaction::new(ndb)?;
    let mut storage = NegentropyStorageVector::new();
    ndb.fold(
        &txn,
        &[event::headway_filter(author)],
        &mut storage,
        |acc, note| {
            // insert only fails on a bad id length, which can't happen for a stored
            // note; ignore the Result to keep the fold infallible.
            let _ = acc.insert(note.created_at(), Id::from_byte_array(*note.id()));
            acc
        },
    )?;
    storage.seal()?;
    Ok(storage)
}

/// The `["EVENT", {...}]` frames for the cached headway events of `author` whose
/// id satisfies `keep` — the events to push so the relay (and app) catch up.
/// `keep` selects which side of a reconcile to forward (the ids we hold that the
/// relay lacks).
fn frames_where(ndb: &Ndb, author: &Pubkey, keep: impl Fn(&[u8; 32]) -> bool) -> Vec<String> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    ndb.fold(
        &txn,
        &[event::headway_filter(author)],
        Vec::new(),
        |mut acc, note| {
            if keep(note.id())
                && let Ok(msg) = enostr::ClientMessage::event(&note)
                && let Ok(frame) = msg.to_json()
            {
                acc.push(frame);
            }
            acc
        },
    )
    .unwrap_or_default()
}

/// Degraded sync for relays that don't speak NIP-77: `REQ` the whole board in,
/// ingest it, then push any local event the relay didn't return. O(board) on the
/// wire instead of O(difference), but it keeps the CLI working against plain
/// NIP-01 relays (or a notedeck whose relay predates negentropy).
async fn fallback_sync(
    relay: &mut Relay,
    ndb: &Ndb,
    author: &Pubkey,
    filter: &serde_json::Value,
) -> Result<()> {
    let received = relay.sync_into(ndb, &filter.to_string()).await?;
    await_ingest(ndb, &received).await;

    let on_relay: HashSet<[u8; 32]> = received.into_iter().collect();
    let pending = frames_where(ndb, author, |id| !on_relay.contains(id));
    if !pending.is_empty() {
        relay.publish(&pending).await?;
        eprintln!("flushed {} local event(s) to the relay", pending.len());
    }
    Ok(())
}

/// Collects the `["EVENT", {...}]` frames an edit produces so they can be
/// forwarded to the relay after `apply` returns.
#[derive(Default)]
struct Collect(Vec<String>);

impl Publisher for Collect {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

/// Forward an edit's collected frames to the relay if one is connected. With no
/// relay the events are already in the local cache; they simply won't reach the
/// running app until it's reachable, so this is a no-op.
async fn publish(relay: &mut Option<Relay>, sink: Collect) -> Result<()> {
    if let Some(relay) = relay {
        relay.publish(&sink.0).await?;
    }
    Ok(())
}

/// A trailing note for command output, flagging when a change landed only in the
/// local cache because no relay was reachable.
fn offline_note(relay: &Option<Relay>) -> &'static str {
    if relay.is_some() {
        ""
    } else {
        " — offline, not forwarded to the app"
    }
}

// ---------------------------------------------------------------------------
// argument resolution
// ---------------------------------------------------------------------------

fn resolve_col(view: &BoardView, sel: &str) -> Result<usize> {
    view.columns
        .iter()
        .position(|c| c.id == sel || c.name.eq_ignore_ascii_case(sel))
        .ok_or_else(|| {
            let names: Vec<&str> = view.columns.iter().map(|c| c.name.as_str()).collect();
            format!("no column matching '{sel}'; columns: {}", names.join(", ")).into()
        })
}

/// Resolve a card argument: a full 64-char hex id, or a unique short prefix of
/// one (matched against every card on the board, including archived ones).
fn resolve_card(view: &BoardView, sel: &str) -> Result<NoteId> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();
    let mut hits = all_cards(view).filter(|c| c.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id),
        (Some(_), Some(_)) => Err(format!("ambiguous card prefix '{sel}'").into()),
        _ => Err(format!("no card matching '{sel}'").into()),
    }
}

fn all_cards(view: &BoardView) -> impl Iterator<Item = &CardView> {
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .chain(view.archived.iter().map(|a| &a.card))
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

fn print_board(view: &BoardView, as_json: bool) {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&event::board_json(view))
                .unwrap_or_else(|_| "null".into())
        );
        return;
    }

    println!("{}", view.title);
    if !view.description.is_empty() {
        println!("{}", view.description);
    }
    for col in &view.columns {
        println!("\n{} ({})", col.name, col.cards.len());
        for c in &col.cards {
            println!(
                "  {}  {}{}",
                short(&c.id),
                c.title,
                labels_suffix(&c.labels)
            );
        }
    }
    if !view.archived.is_empty() {
        println!("\nArchived ({})", view.archived.len());
        for a in &view.archived {
            println!("  {}  {}", short(&a.card.id), a.card.title);
        }
    }
}

fn labels_suffix(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", labels.join(", "))
    }
}

fn short(id: &NoteId) -> String {
    id.hex().chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// argument parsing
// ---------------------------------------------------------------------------

struct Cli {
    secret: Option<([u8; 32], Pubkey)>,
    author: Option<Pubkey>,
    relay: String,
    db: Option<String>,
    board: String,
    json: bool,
    command: Command,
}

impl Cli {
    /// Parse args (without the program name). Returns `Ok(None)` when usage
    /// should be printed (no command, `-h`/`--help`).
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>> {
        let mut nsec = env::var("HEADWAY_NSEC").ok();
        let mut relay = env::var("HEADWAY_RELAY")
            .ok()
            .unwrap_or_else(|| DEFAULT_RELAY.to_string());
        let mut db = None;
        let mut board = store::BOARD_ID.to_string();
        let mut author = None;
        let mut json = false;
        let mut col = None;
        let mut row = None;
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
                "--relay" => relay = value("--relay")?,
                "--db" => db = Some(value("--db")?),
                "--board" => board = value("--board")?,
                "--author" => author = Some(Pubkey::parse(&value("--author")?)?),
                "--col" => col = Some(value("--col")?),
                "--row" => {
                    row = Some(
                        value("--row")?
                            .parse()
                            .map_err(|_| "--row must be a number")?,
                    )
                }
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
        let command = parse_command(name, rest, col, row)?;

        let secret = match nsec {
            Some(nsec) => Some(parse_nsec(&nsec)?),
            None => None,
        };

        Ok(Some(Cli {
            secret,
            author,
            relay,
            db,
            board,
            json,
            command,
        }))
    }
}

fn parse_command(
    name: &str,
    rest: &[String],
    col: Option<String>,
    row: Option<usize>,
) -> Result<Command> {
    let card = || -> Result<String> { arg(rest, 0, name) };
    Ok(match name {
        "show" => Command::Show,
        "seed" => Command::Seed,
        "add" => Command::Add {
            title: joined(rest, 0, name)?,
            col,
        },
        "move" => Command::Move {
            card: card()?,
            col: col.ok_or("move needs --col <column>")?,
            row,
        },
        "title" => Command::Title {
            card: card()?,
            title: joined(rest, 1, name)?,
        },
        "desc" => Command::Desc {
            card: card()?,
            text: joined(rest, 1, name)?,
        },
        "label" => Command::Label {
            card: card()?,
            labels: rest.get(1..).unwrap_or_default().to_vec(),
        },
        "delete" => Command::Delete { card: card()? },
        "archive" => Command::Archive { card: card()? },
        "restore" => Command::Restore { card: card()? },
        other => return Err(format!("unknown command '{other}' (try `headway --help`)").into()),
    })
}

/// The `idx`th positional argument to a command, or an error naming the command.
fn arg(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    rest.get(idx)
        .cloned()
        .ok_or_else(|| format!("`{cmd}` is missing an argument").into())
}

/// Everything from `idx` onward, space-joined — for free-text titles/bodies.
fn joined(rest: &[String], idx: usize, cmd: &str) -> Result<String> {
    let parts = rest.get(idx..).unwrap_or_default();
    if parts.is_empty() {
        return Err(format!("`{cmd}` is missing text").into());
    }
    Ok(parts.join(" "))
}

/// Decode an `nsec...` into raw secret bytes and its derived pubkey.
fn parse_nsec(nsec: &str) -> Result<([u8; 32], Pubkey)> {
    let (hrp, data) = bech32::decode(nsec).map_err(|_| "invalid nsec (not bech32)")?;
    if hrp.as_str() != "nsec" {
        return Err(format!("expected an nsec, got '{}' key", hrp.as_str()).into());
    }
    let secret: [u8; 32] = data
        .try_into()
        .map_err(|_| "nsec did not decode to 32 bytes")?;
    let keypair = enostr::Keypair::from_secret(enostr::SecretKey::from_slice(&secret)?);
    Ok((secret, keypair.pubkey))
}

fn print_usage() {
    eprintln!(
        "\
headway — interact with a Headway board over a running notedeck's relay

USAGE:
    headway [OPTIONS] <COMMAND>

COMMANDS:
    show                       Print the board (--json for machine output)
    seed                       Seed the default board if none exists
    add <title...>             Add a card (defaults to the first column)
    move <card> --col <c>      Move a card to a column (--row to position)
    title <card> <title...>    Edit a card's title
    desc <card> <text...>      Edit a card's description
    label <card> [labels...]   Set a card's labels (empty clears)
    delete <card>              Remove a card (reversible tombstone)
    archive <card>             Archive a card off the board
    restore <card>             Restore an archived card

    <card> is a card id or a unique short prefix (see `show`).
    <c> is a column id or name (case-insensitive).

OPTIONS:
    --nsec <nsec>     Signing key (or $HEADWAY_NSEC). Required to edit.
    --author <pk>     Board author to read (defaults to the signer)
    --relay <url>     Relay URL (or $HEADWAY_RELAY) [default: {DEFAULT_RELAY}]
    --board <id>      Board id [default: {board}]
    --db <path>       nostrdb cache dir [default: <data-dir>/headway-cli]
    --json            Machine-readable output (show)
    -h, --help        Print this help",
        board = store::BOARD_ID,
    );
}
