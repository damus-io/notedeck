//! `headway` — a CLI for reading and mutating a Headway board against a running
//! notedeck's embedded relay.
//!
//! The cache/sync/relay plumbing — keeping the CLI's own nostrdb, reconciling it
//! against the app's relay with NIP-77 negentropy, and the stored signing key —
//! lives in the shared [`relay_sync`] crate (see [`notebook_cli`] for the other
//! consumer). This file is just the board's command surface: parsing, resolving
//! card/column arguments against the folded board, and rendering.

use std::env;
use std::process::ExitCode;

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use serde_json::json;

use headway::event::{self, BoardView, CardView, CommentView};
use headway::store::{self, BoardAction, Publisher};
use headway::wordid;

use relay_sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/headway-cli` on Linux).
const APP: &str = "headway-cli";

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
    Show {
        /// Optional card selectors. When non-empty, `show` prints these cards
        /// in full `git show`-style detail rather than the whole board.
        cards: Vec<String>,
    },
    Seed,
    Add {
        title: String,
        col: Option<String>,
        labels: Vec<String>,
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
    Comment {
        card: String,
        body: String,
        /// A comment on the same card to thread this reply under.
        reply_to: Option<String>,
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

    // The author whose board we read/write: an explicit override, else the
    // signing key's own pubkey.
    let author = match (&cli.author, &cli.secret) {
        (Some(pk), _) => *pk,
        (None, Some((_, pk))) => *pk,
        (None, None) => return Err("need --nsec to sign, or --author to read a board".into()),
    };

    let ndb = relay_sync::open_ndb(cli.db.as_deref(), APP)?;

    // Reconcile the local cache against the relay both ways so the cache and the
    // app converge regardless of which side an edit happened on. Best-effort: an
    // unreachable relay leaves us working offline against the cache.
    let filter = event::headway_filter(&author);
    let is_addressable = |kind: u32| kind == event::KIND_BOARD || kind == event::KIND_PLACEMENT;
    let mut relay = relay_sync::connect_and_sync(
        &cli.relay,
        &ndb,
        &author,
        &event::HEADWAY_KINDS,
        &filter,
        &is_addressable,
    )
    .await?;

    let board = cli.board;
    let as_json = cli.json;
    let show_archived = cli.archived;
    let secret = cli.secret.map(|(s, _)| s);

    match cli.command {
        Command::Show { cards } => match load_board(&ndb, &author, &board) {
            Some(view) if cards.is_empty() => print_board(&view, as_json, show_archived),
            Some(view) => print_cards(&view, &cards, as_json)?,
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
            relay_sync::publish(&mut relay, &sink.0).await?;
            println!(
                "seeded board '{board}' ({n} events){}",
                relay_sync::offline_note(&relay)
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
            relay_sync::publish(&mut relay, &sink.0).await?;
            println!("ok ({n} events){}", relay_sync::offline_note(&relay));
        }
    }

    Ok(())
}

/// Translate a resolved [`Command`] into a [`BoardAction`], resolving card and
/// column arguments against `view`.
fn build_action(view: &BoardView, command: Command) -> Result<BoardAction> {
    Ok(match command {
        Command::Add { title, col, labels } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            BoardAction::AddCard { col, title, labels }
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
        Command::Comment {
            card,
            body,
            reply_to,
        } => {
            let card = resolve_card(view, &card)?;
            let reply_to = reply_to
                .as_deref()
                .map(|sel| resolve_comment(view, &card, sel))
                .transpose()?;
            BoardAction::AddComment {
                card,
                body,
                reply_to,
            }
        }
        Command::Delete { card } => BoardAction::DeleteCard {
            card: resolve_card(view, &card)?,
        },
        Command::Archive { card } => BoardAction::ArchiveCard {
            card: resolve_card(view, &card)?,
        },
        Command::Restore { card } => BoardAction::RestoreCard {
            card: resolve_card(view, &card)?,
        },
        Command::Show { .. } | Command::Seed | Command::Login { .. } | Command::Logout => {
            unreachable!("handled before build_action")
        }
    })
}

// ---------------------------------------------------------------------------
// board loading
// ---------------------------------------------------------------------------

fn load_board(ndb: &Ndb, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    let txn = Transaction::new(ndb).ok()?;
    event::load_board(ndb, &txn, author, board_id)
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

/// Resolve a card argument, accepting (in order): a full 64-char hex id; a word
/// id like `headway#maple-river-canyon` (the `<board>#` prefix is optional, and a
/// bare leading `#` is fine too, so `#maple-river-canyon` and
/// `maple-river-canyon` both work); or a unique hex prefix. Word ids and hex
/// prefixes are matched against every card on the board, archived ones included.
fn resolve_card(view: &BoardView, sel: &str) -> Result<NoteId> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();

    // Word id: drop an optional `<board>#` prefix (or a bare leading `#`), then
    // match by re-encoding each card — exactly how a git short hash is resolved.
    let words = sel
        .strip_prefix(&format!("{}#", view.id.to_lowercase()))
        .or_else(|| sel.strip_prefix('#'))
        .unwrap_or(&sel);
    if let Some(c) = all_cards(view).find(|c| wordid::encode(c.id.bytes()) == words) {
        return Ok(c.id);
    }

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

/// Resolve a `--reply-to` selector against the comments on `card`, accepting a
/// full hex id, a unique hex prefix, or a comment word-id — the same forms
/// [`resolve_card`] accepts, but scoped to one card's thread.
fn resolve_comment(view: &BoardView, card: &NoteId, sel: &str) -> Result<NoteId> {
    let comments = all_cards(view)
        .find(|c| c.id == *card)
        .map(|c| c.comments.as_slice())
        .unwrap_or(&[]);

    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();
    let words = sel.strip_prefix('#').unwrap_or(&sel);
    if let Some(c) = comments
        .iter()
        .find(|c| wordid::encode(c.id.bytes()) == words)
    {
        return Ok(c.id);
    }

    let mut hits = comments.iter().filter(|c| c.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id),
        (Some(_), Some(_)) => Err(format!("ambiguous comment prefix '{sel}'").into()),
        _ => Err(format!("no comment matching '{sel}' on this card").into()),
    }
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

fn print_board(view: &BoardView, as_json: bool, show_archived: bool) {
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
                "  {}{}  {}",
                c.title,
                labels_suffix(&c.labels),
                card_ref(view, &c.id),
            );
        }
    }
    if !view.archived.is_empty() {
        if show_archived {
            println!("\nArchived ({})", view.archived.len());
            for a in &view.archived {
                println!("  {}  {}", a.card.title, card_ref(view, &a.card.id));
            }
        } else {
            println!(
                "\nArchived ({}) — use `show --archived` to list",
                view.archived.len()
            );
        }
    }
}

/// Print only the cards named by `sels` (each a card id or unique short prefix).
/// In JSON mode this is an array of card objects, each with the `column` it
/// currently sits in; otherwise one card per line.
fn print_cards(view: &BoardView, sels: &[String], as_json: bool) -> Result<()> {
    // Resolve every selector first so a bad id fails the whole command rather
    // than printing a partial result.
    let cards: Vec<(&CardView, String)> = sels
        .iter()
        .map(|sel| {
            let id = resolve_card(view, sel)?;
            find_card(view, &id).ok_or_else(|| format!("no card matching '{sel}'").into())
        })
        .collect::<Result<_>>()?;

    if as_json {
        let out: Vec<_> = cards
            .iter()
            .map(|(card, col)| {
                let mut j = event::card_json(card);
                j["column"] = json!(col);
                j
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "null".into())
        );
    } else {
        for (i, (card, col)) in cards.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_card_detail(view, card, col);
        }
    }
    Ok(())
}

/// Print a single card in `git show` style: a header block of metadata, then the
/// title and description body indented underneath. Used when `show` is given
/// explicit card selectors, where the full card is more useful than the
/// one-line board summary.
fn print_card_detail(view: &BoardView, card: &CardView, col: &str) {
    println!("card    {}", card_ref(view, &card.id));
    println!("id      {}", card.id.hex());
    println!("column  {col}");
    if !card.labels.is_empty() {
        println!("labels  {}", card.labels.join(", "));
    }

    println!("\n    {}", card.title);
    if !card.description.is_empty() {
        println!();
        for line in card.description.lines() {
            if line.is_empty() {
                println!();
            } else {
                println!("    {line}");
            }
        }
    }

    if !card.comments.is_empty() {
        println!("\ncomments ({})", card.comments.len());
        for c in &card.comments {
            print_comment(c);
        }
    }
}

/// Print a single comment in the card-detail thread: an author/time header (with
/// the comment's own word-id so it can be `--reply-to`'d), then its body indented
/// beneath. Replies are flagged inline but still rendered flat for now.
fn print_comment(c: &CommentView) {
    let mut header = format!(
        "    {}  {}  {}",
        short_author(&c.author),
        relay_sync::dim(&rel_time(c.created_at)),
        relay_sync::dim(&format!("#{}", wordid::encode(c.id.bytes()))),
    );
    if let Some(parent) = &c.parent {
        header.push_str(&relay_sync::dim(&format!(
            "  ↳ reply to #{}",
            wordid::encode(parent.bytes())
        )));
    }
    println!("\n{header}");
    for line in c.body.lines() {
        if line.is_empty() {
            println!();
        } else {
            println!("        {line}");
        }
    }
}

/// A short, recognisable stand-in for a comment author: the first 12 hex chars of
/// their pubkey. The CLI has no profile data, so this is just a stable handle.
fn short_author(author: &[u8; 32]) -> String {
    Pubkey::new(*author).hex().chars().take(12).collect()
}

/// A coarse "x ago" rendering of a unix timestamp for the comment thread.
fn rel_time(created_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now.saturating_sub(created_at);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        86400..=604_799 => format!("{}d ago", secs / 86400),
        _ => format!("{}w ago", secs / 604_800),
    }
}

/// Find a card by id anywhere on the board, returning it alongside the name of
/// the column it sits in (or `"archived"`).
fn find_card<'a>(view: &'a BoardView, id: &NoteId) -> Option<(&'a CardView, String)> {
    for col in &view.columns {
        if let Some(card) = col.cards.iter().find(|c| c.id == *id) {
            return Some((card, col.name.clone()));
        }
    }
    view.archived
        .iter()
        .find(|a| a.card.id == *id)
        .map(|a| (&a.card, "archived".to_string()))
}

fn labels_suffix(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", labels.join(", "))
    }
}

/// A card's human-friendly reference: the board slug, a `#`, then three words,
/// e.g. `headway#maple-river-canyon` — GitHub's `repo#id` shape, so it reads as a
/// reference inline (`Fixes: headway#maple-river-canyon`) and in chat. Just a
/// rendering of the event id — see [`headway::wordid`]. Rendered muted so the
/// title stays the eye's anchor.
fn card_ref(view: &BoardView, id: &NoteId) -> String {
    relay_sync::dim(&format!("{}#{}", view.id, wordid::encode(id.bytes())))
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
    archived: bool,
    command: Command,
}

impl Cli {
    /// Parse args (without the program name). Returns `Ok(None)` when usage
    /// should be printed (no command, `-h`/`--help`).
    fn parse(args: impl Iterator<Item = String>) -> Result<Option<Self>> {
        // Precedence: `--nsec` (set below) overrides the `HEADWAY_NSEC` env var,
        // which overrides the key stored by `login`.
        let mut nsec = env::var("HEADWAY_NSEC")
            .ok()
            .or_else(|| relay_sync::stored_nsec(APP));
        let mut relay = env::var("HEADWAY_RELAY")
            .ok()
            .unwrap_or_else(|| relay_sync::DEFAULT_RELAY.to_string());
        let mut db = None;
        let mut board = store::BOARD_ID.to_string();
        let mut author = None;
        let mut json = false;
        let mut archived = false;
        let mut col = None;
        let mut row = None;
        let mut reply_to = None;
        let mut labels: Vec<String> = Vec::new();
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
                "--reply-to" => reply_to = Some(value("--reply-to")?),
                "-l" | "--label" | "--labels" => {
                    // Repeatable, and each value may be a comma-separated list,
                    // so `-l a,b --label c` and `-l a -l b -l c` are equivalent.
                    labels.extend(
                        value("--label")?
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                }
                "--row" => {
                    row = Some(
                        value("--row")?
                            .parse()
                            .map_err(|_| "--row must be a number")?,
                    )
                }
                "--json" => json = true,
                "--archived" => archived = true,
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        let Some((name, rest)) = positionals.split_first() else {
            return Ok(None);
        };
        let command = parse_command(name, rest, col, row, reply_to, labels)?;

        // `login`/`logout` manage the stored key themselves, so don't parse (and
        // potentially reject on) whatever key is currently configured — that would
        // keep `login` from replacing a stale or malformed stored key.
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
            board,
            json,
            archived,
            command,
        }))
    }
}

fn parse_command(
    name: &str,
    rest: &[String],
    col: Option<String>,
    row: Option<usize>,
    reply_to: Option<String>,
    labels: Vec<String>,
) -> Result<Command> {
    let card = || -> Result<String> { arg(rest, 0, name) };
    Ok(match name {
        "show" => Command::Show {
            cards: rest.to_vec(),
        },
        "seed" => Command::Seed,
        "add" => Command::Add {
            title: joined(rest, 0, name)?,
            col,
            labels,
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
        "comment" => Command::Comment {
            card: card()?,
            body: joined(rest, 1, name)?,
            reply_to,
        },
        "delete" => Command::Delete { card: card()? },
        "archive" => Command::Archive { card: card()? },
        "restore" => Command::Restore { card: card()? },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
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

fn print_usage() {
    eprintln!(
        "\
headway — interact with a Headway board over a running notedeck's relay

USAGE:
    headway [OPTIONS] <COMMAND>

COMMANDS:
    show [cards...]            Print the board, or the given cards in full
                               detail (--archived to list archived, --json for
                               machine output)
    seed                       Seed the default board if none exists
    add <title...>             Add a card (--col <c> column, -l <labels> to tag)
    move <card> --col <c>      Move a card to a column (--row to position)
    title <card> <title...>    Edit a card's title
    desc <card> <text...>      Edit a card's description
    label <card> [labels...]   Set a card's labels (empty clears)
    comment <card> <text...>   Comment on a card (--reply-to <c> to thread under
                               another comment)
    delete <card>              Remove a card (reversible tombstone)
    archive <card>             Archive a card off the board
    restore <card>             Restore an archived card
    login <nsec>               Store a signing key for later runs
    logout                     Forget the stored signing key

    <card> is a card id or a unique short prefix (see `show`).
    <c> is a column id or name (case-insensitive).

OPTIONS:
    --nsec <nsec>     Signing key for this run. Normally unnecessary — run
                      `headway login` once and it's reused. ($HEADWAY_NSEC,
                      if set, takes precedence over the stored key.)
    --author <pk>     Board author to read (defaults to the signer)
    --relay <url>     Relay URL (or $HEADWAY_RELAY) [default: {DEFAULT_RELAY}]
    --board <id>      Board id [default: {board}]
    --db <path>       nostrdb cache dir [default: <data-dir>/headway-cli]
    -l, --label <l>   Label(s) for `add` (repeatable; comma-separated allowed)
    --col <c>         Column for `add`/`move` (id or name)
    --reply-to <c>    Parent comment for `comment` (id, prefix, or word-id)
    --json            Machine-readable output (show)
    --archived        List archived cards in full (show)
    -h, --help        Print this help",
        DEFAULT_RELAY = relay_sync::DEFAULT_RELAY,
        board = store::BOARD_ID,
    );
}
