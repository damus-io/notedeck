//! `headway` — a CLI for reading and mutating a Headway board against a running
//! notedeck's embedded relay.
//!
//! The CLI keeps its **own** nostrdb as a cache. Each run it reconciles that
//! cache against the relay with NIP-77 negentropy — pulling the events the relay
//! has that it lacks and pushing the ones it holds that the relay lacks — then
//! folds the board locally with the pure [`headway`] reducer. Edits forward the
//! events they produce back to the relay so the running app sees the change.

mod relay;

use std::collections::{HashMap, HashSet};
use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use enostr::{NoteId, Pubkey};
use negentropy::{Id, NegentropyStorageVector};
use nostrdb::{Config, Ndb, Transaction};
use serde_json::json;

use headway::event::{self, BoardView, CardView, CommentView};
use headway::store::{self, BoardAction, Publisher};
use headway::wordid;

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
        Command::Login { nsec } => return login(nsec),
        Command::Logout => return logout(),
        _ => {}
    }

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
        // The app being closed is the common case, not an error worth warning
        // about — fall back to the local cache quietly.
        Err(_) => None,
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
///
/// Addressable events (board/placement) are deduplicated to their latest
/// revision per `(kind, d-tag)` right in the query; immutable events (issues,
/// labels, covers) pass through untouched. The local cache is append-only and
/// keeps every old revision, but a relay holds only the latest and rejects the
/// rest as "replaced" — pushing stale revisions is pointless and would keep the
/// reconcile from ever converging (the dropped id can never land, so it
/// re-flushes every run). The winner follows NIP-33 resolution: newest
/// `created_at`, ties broken by the lexically lowest id, matching what the relay
/// and app keep.
fn frames_where(ndb: &Ndb, author: &Pubkey, keep: impl Fn(&[u8; 32]) -> bool) -> Vec<String> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };

    // Threaded accumulator: the winning revision per addressable coordinate, plus
    // every immutable event in arrival order.
    type Latest = HashMap<(u32, String), (u64, [u8; 32], String)>;
    let (latest, plain) = ndb
        .fold(
            &txn,
            &[event::headway_filter(author)],
            (Latest::new(), Vec::<([u8; 32], String)>::new()),
            |(mut latest, mut plain), note| {
                let id = *note.id();
                let Ok(msg) = enostr::ClientMessage::event(&note) else {
                    return (latest, plain);
                };
                let Ok(frame) = msg.to_json() else {
                    return (latest, plain);
                };

                let kind = note.kind();
                if (kind == event::KIND_BOARD || kind == event::KIND_PLACEMENT)
                    && let Some(d) = d_tag(&note)
                {
                    let at = note.created_at();
                    let win = latest
                        .get(&(kind, d.clone()))
                        .is_none_or(|(t, i, _)| at > *t || (at == *t && id < *i));
                    if win {
                        latest.insert((kind, d), (at, id, frame));
                    }
                } else {
                    plain.push((id, frame));
                }
                (latest, plain)
            },
        )
        .unwrap_or_default();

    latest
        .into_values()
        .map(|(_, id, frame)| (id, frame))
        .chain(plain)
        .filter(|(id, _)| keep(id))
        .map(|(_, frame)| frame)
        .collect()
}

/// The value of a note's `d` tag, if any.
fn d_tag(note: &nostrdb::Note) -> Option<String> {
    note.tags().iter().find_map(|tag| {
        (tag.get_str(0) == Some("d"))
            .then(|| tag.get_str(1).map(str::to_owned))
            .flatten()
    })
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
        dim(&rel_time(c.created_at)),
        dim(&format!("#{}", wordid::encode(c.id.bytes()))),
    );
    if let Some(parent) = &c.parent {
        header.push_str(&dim(&format!(
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
    dim(&format!("{}#{}", view.id, wordid::encode(id.bytes())))
}

/// Mute `s` with an ANSI color, but only when stdout is a terminal — so ids
/// read as muted beside titles interactively, while a piped or redirected
/// listing stays plain text for scripts to parse.
///
/// Uses the "bright black" foreground (SGR 90), not the dim attribute (SGR 2):
/// dim is widely unimplemented — urxvt, among others, ignores it and renders
/// the id at full strength — whereas the bright-black color is part of the
/// standard 16-color palette every terminal honors.
fn dim(s: &str) -> String {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        format!("\x1b[90m{s}\x1b[0m")
    } else {
        s.to_string()
    }
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
        let mut nsec = env::var("HEADWAY_NSEC").ok().or_else(stored_nsec);
        let mut relay = env::var("HEADWAY_RELAY")
            .ok()
            .unwrap_or_else(|| DEFAULT_RELAY.to_string());
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
            (_, Some(nsec)) => Some(parse_nsec(&nsec)?),
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

// ---------------------------------------------------------------------------
// stored signing key
// ---------------------------------------------------------------------------

/// Where the signing key lives when stored via `login`: a single `nsec...` line
/// in `<data-dir>/headway-cli/nsec` (e.g. `~/.local/share/headway-cli/nsec` on
/// Linux), alongside the cache. It lets a key be set once so later runs — and the
/// agents driving them — never have to pass `--nsec` or export `HEADWAY_NSEC`.
fn nsec_config_path() -> Result<std::path::PathBuf> {
    Ok(dirs::data_dir()
        .ok_or("no data dir; set HEADWAY_NSEC or pass --nsec")?
        .join("headway-cli")
        .join("nsec"))
}

/// Read the stored signing key, if any. Missing file, unreadable file, or an
/// empty one all read as "no stored key" — the caller falls back to the env var
/// or `--nsec`.
fn stored_nsec() -> Option<String> {
    let contents = std::fs::read_to_string(nsec_config_path().ok()?).ok()?;
    let trimmed = contents.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Validate `nsec` and store it for later runs. We derive the pubkey first so a
/// malformed key is rejected before it's written, and lock the file to the owner
/// since it holds a secret.
fn login(nsec: &str) -> Result<()> {
    let (_, pubkey) = parse_nsec(nsec)?;
    let path = nsec_config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("{nsec}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!(
        "stored signing key for {} in {}",
        pubkey.hex(),
        path.display()
    );
    Ok(())
}

/// Forget the stored signing key. Removing a key that isn't there is not an error.
fn logout() -> Result<()> {
    let path = nsec_config_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => println!("removed stored signing key at {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("no stored signing key"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
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
        board = store::BOARD_ID,
    );
}
