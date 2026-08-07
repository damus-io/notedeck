//! `headway` — a CLI for reading and mutating a Headway board against a running
//! notedeck's embedded relay.
//!
//! The cache/sync/relay plumbing — keeping the CLI's own nostrdb, reconciling it
//! against the app's relay with NIP-77 negentropy, and the stored signing key —
//! lives in `nostrdb_net`'s `relay::sync` module (see [`notebook_cli`] for the
//! other consumer). This file is just the board's command surface: parsing,
//! resolving card/column arguments against the folded board, and rendering.

use std::env;
use std::process::ExitCode;

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use serde_json::json;

use headway::event::{self, BoardView, CardView, CommentView, Container, Date, Priority};
use headway::store::{self, BoardAction, Publisher};
use headway::{traversal, wordid};

use nostrdb_net::relay::sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/headway-cli` on Linux).
const APP: &str = "headway-cli";

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`headway show | head`) instead of
    // panicking in println! on EPIPE.
    nostrdb_net::relay::sync::reset_sigpipe();
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
        /// A card to parent the new one under (created as a subissue).
        parent: Option<String>,
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
    /// Set a card's priority (none/low/medium/high/urgent).
    Priority {
        card: String,
        level: String,
    },
    /// Set a card's due date (`YYYY-MM-DD`, or `none` to clear).
    Due {
        card: String,
        date: String,
    },
    /// Set a card's estimate (a number, or `none` to clear).
    Estimate {
        card: String,
        points: String,
    },
    /// Position a card in a container's work-order (see [`SeqSpec`]). `container`
    /// is a card ref (its subissues) or omitted/board-slug (the board root).
    Seq {
        card: String,
        spec: SeqSpec,
        container: Option<String>,
    },
    /// Make a card a subissue of another card, or detach it (no parent given).
    Parent {
        card: String,
        parent: Option<String>,
    },
    /// Print what to work on next: walk a container's work-order and show the
    /// ready frontier (see [`traversal`]). A read command — it never signs.
    Next {
        /// `--in`: the container. A card ref (its subissues) or the board slug
        /// (board root); omitted means the board root. Shares the `seq --in`
        /// grammar via [`resolve_container`].
        container: Option<String>,
        /// `--ready`: print the whole ready set (parallel-dispatch frontier),
        /// not just the single next card.
        ready: bool,
        /// `-n <k>`: cap the number of cards printed.
        limit: Option<usize>,
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
    /// Place a card from the current board onto another board too, keeping it on
    /// both (placement-driven membership — it's the same card, not a copy).
    Link {
        card: String,
        to_board: String,
    },
    /// Move a card from the current board to another board: link it there and
    /// remove it from the current one.
    MoveBoard {
        card: String,
        to_board: String,
    },
    /// With an `id`, switch the persisted current board; without one, list the
    /// boards in the cache and mark the current selection.
    Board {
        id: Option<String>,
    },
    /// Rename the current board's display title (its slug is unchanged, so
    /// existing card refs keep working).
    Rename {
        title: String,
    },
    Login {
        nsec: String,
    },
    Logout,
}

/// Where `seq` should place the card, parsed from `--after/--before/--first/--last`
/// (any card refs are resolved later, in [`build_action`]).
enum SeqSpec {
    First,
    Last,
    After(String),
    Before(String),
}

/// The `seq` position flags, collected during arg parsing and validated into a
/// [`SeqSpec`] by [`seq_spec`].
#[derive(Default)]
struct SeqFlags {
    after: Option<String>,
    before: Option<String>,
    first: bool,
    last: bool,
    /// `--in`: the container selector (card ref or board slug).
    container: Option<String>,
}

/// Validate that exactly one position flag was given and turn it into a [`SeqSpec`].
fn seq_spec(flags: &SeqFlags) -> Result<SeqSpec> {
    let count = flags.after.is_some() as u8
        + flags.before.is_some() as u8
        + flags.first as u8
        + flags.last as u8;
    if count != 1 {
        return Err(
            "seq needs exactly one of --after <card> / --before <card> / --first / --last".into(),
        );
    }
    Ok(if let Some(a) = &flags.after {
        SeqSpec::After(a.clone())
    } else if let Some(b) = &flags.before {
        SeqSpec::Before(b.clone())
    } else if flags.first {
        SeqSpec::First
    } else {
        SeqSpec::Last
    })
}

impl Command {
    /// The board named by this command's card selectors, when any of them
    /// carries a `<board>#<word-id>` prefix (see [`ref_board`]). Used to
    /// self-route the command to that board. Errors when two selectors name
    /// different boards — cards are resolved against a single board per run.
    fn selector_board(&self) -> Result<Option<String>> {
        let mut selectors: Vec<&str> = Vec::new();
        match self {
            Command::Show { cards } => selectors.extend(cards.iter().map(String::as_str)),
            Command::Add { parent, .. } => selectors.extend(parent.as_deref()),
            Command::Parent { card, parent } => {
                selectors.push(card);
                selectors.extend(parent.as_deref());
            }
            Command::Seq {
                card,
                spec,
                container,
            } => {
                selectors.push(card);
                if let SeqSpec::After(a) | SeqSpec::Before(a) = spec {
                    selectors.push(a);
                }
                selectors.extend(container.as_deref());
            }
            Command::Next { container, .. } => selectors.extend(container.as_deref()),
            Command::Move { card, .. }
            | Command::Title { card, .. }
            | Command::Desc { card, .. }
            | Command::Label { card, .. }
            | Command::Priority { card, .. }
            | Command::Due { card, .. }
            | Command::Estimate { card, .. }
            | Command::Comment { card, .. }
            | Command::Delete { card }
            | Command::Archive { card }
            | Command::Restore { card }
            | Command::Link { card, .. }
            | Command::MoveBoard { card, .. } => selectors.push(card),
            Command::Seed
            | Command::Rename { .. }
            | Command::Board { .. }
            | Command::Login { .. }
            | Command::Logout => {}
        }

        let mut found: Option<String> = None;
        for slug in selectors.into_iter().filter_map(ref_board) {
            match &found {
                Some(cur) if cur != &slug => {
                    return Err(
                        format!("card refs name different boards ('{cur}' and '{slug}')").into(),
                    );
                }
                _ => found = Some(slug),
            }
        }
        Ok(found)
    }
}

/// The `<board>` prefix of a full `<board>#<word-id>` card reference, lowercased
/// (board slugs are lowercase). `None` for anything that isn't one: bare
/// `#word-id` refs, plain word ids, hex ids/prefixes, and prefixes that aren't
/// slug-shaped (slugs are ascii alphanumerics and `-`, see `store::board_slug`).
fn ref_board(sel: &str) -> Option<String> {
    let (slug, words) = sel.split_once('#')?;
    if slug.is_empty()
        || words.is_empty()
        || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    Some(slug.to_lowercase())
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

    // The author whose board we read/write: an explicit override, else the
    // signing key's own pubkey.
    let author = match (&cli.author, &cli.secret) {
        (Some(pk), _) => *pk,
        (None, Some((_, pk))) => *pk,
        (None, None) => return Err("need --nsec to sign, or --author to read a board".into()),
    };

    let ndb = nostrdb_net::relay::sync::open_ndb(cli.db.as_deref(), APP)?;

    // Reconcile the local cache against the relay both ways so the cache and the
    // app converge regardless of which side an edit happened on. Best-effort: an
    // unreachable relay leaves us working offline against the cache.
    let filter = event::headway_filter(&author);
    let is_addressable = |kind: u32| kind == event::KIND_BOARD || kind == event::KIND_PLACEMENT;
    // `connect_and_sync` speaks `nostrdb_net::Pubkey`; convert our enostr author
    // across the boundary (both are `[u8; 32]` newtypes).
    let author_nn = nostrdb_net::Pubkey::new(*author.bytes());
    let mut relay = nostrdb_net::relay::sync::connect_and_sync(
        &cli.relay,
        &ndb,
        &author_nn,
        &event::HEADWAY_KINDS,
        &filter,
        &is_addressable,
    )
    .await?;

    let board = cli.board;
    let board_explicit = cli.board_explicit;
    let as_json = cli.json;
    let show_archived = cli.archived;
    let show_all = cli.all;
    let secret = cli.secret.map(|(s, _)| s);

    match cli.command {
        // `--all` fans the render across every board in the cache, ignoring any
        // card selectors (which address a single board).
        Command::Show { .. } if show_all => {
            print_all_boards(&list_boards(&ndb, &author), as_json, show_archived)
        }

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
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "seeded board '{board}' ({n} events){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
        }

        Command::Board { id } => match id {
            // Switch: persist the new current board, then report whether it
            // already exists so the next step (seed vs. use) is obvious.
            Some(id) => {
                nostrdb_net::relay::sync::write_config(APP, "board", &id)?;
                match load_board(&ndb, &author, &id) {
                    Some(view) => {
                        println!("switched to board '{id}' ({} cards)", card_count(&view))
                    }
                    None => {
                        println!("switched to board '{id}' — doesn't exist yet, run `headway seed`")
                    }
                }
            }
            // List: every board in the cache, the current selection marked.
            None => print_boards(&list_boards(&ndb, &author), &board),
        },

        // Cross-board: place/move a card from the current board onto another.
        // These touch two boards, so they sidestep the single-board `apply` path.
        Command::Link { card, to_board } => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let source = load_board(&ndb, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let target = load_board(&ndb, &author, &to_board).ok_or_else(|| {
                format!("no target board '{to_board}' — switch to it and `headway seed` first")
            })?;
            let card_id = resolve_card(&source, &card)?;

            let mut sink = Collect::default();
            store::link_card(
                &ndb,
                store::BoardRef {
                    id: &board,
                    view: &source,
                },
                store::BoardRef {
                    id: &to_board,
                    view: &target,
                },
                &author,
                &secret,
                card_id,
                &mut sink,
            );
            let n = sink.0.len();
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "linked to '{to_board}' ({n} events){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
        }

        Command::MoveBoard { card, to_board } => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let source = load_board(&ndb, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let target = load_board(&ndb, &author, &to_board).ok_or_else(|| {
                format!("no target board '{to_board}' — switch to it and `headway seed` first")
            })?;
            let card_id = resolve_card(&source, &card)?;

            let mut sink = Collect::default();
            store::move_card_between_boards(
                &ndb,
                store::BoardRef {
                    id: &board,
                    view: &source,
                },
                store::BoardRef {
                    id: &to_board,
                    view: &target,
                },
                &author,
                &secret,
                card_id,
                &mut sink,
            );
            let n = sink.0.len();
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "moved to '{to_board}' ({n} events){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
        }

        // Read command: walk the work-order and print the ready frontier. Never
        // signs, and refuses to lean on the persisted current board (see
        // `board_explicit`), so an autonomous agent's `next` can't be raced.
        Command::Next {
            container,
            ready,
            limit,
        } => {
            if !board_explicit {
                return Err(
                    "next never uses the persisted current board — pass --board <id>, \
                     or an --in <board#word-id> card ref that names its own board"
                        .into(),
                );
            }
            let view = load_board(&ndb, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let container = match container.as_deref() {
                Some(sel) => resolve_container(&view, sel)?,
                None => Container::BoardRoot(view.id.clone()),
            };
            print_next(&view, &container, ready, limit, as_json);
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
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "ok ({n} events){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
        }
    }

    Ok(())
}

/// Translate a resolved [`Command`] into a [`BoardAction`], resolving card and
/// column arguments against `view`.
fn build_action(view: &BoardView, command: Command) -> Result<BoardAction> {
    Ok(match command {
        Command::Add {
            title,
            col,
            labels,
            parent,
        } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            let parent = parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?;
            BoardAction::AddCard {
                col,
                title,
                labels,
                parent,
            }
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
        Command::Priority { card, level } => BoardAction::SetPriority {
            card: resolve_card(view, &card)?,
            priority: Priority::parse(&level),
        },
        Command::Due { card, date } => BoardAction::SetDue {
            card: resolve_card(view, &card)?,
            due: parse_clearable(&date, |s| {
                Date::parse(s).ok_or_else(|| format!("invalid date '{s}' (want YYYY-MM-DD)").into())
            })?,
        },
        Command::Estimate { card, points } => BoardAction::SetEstimate {
            card: resolve_card(view, &card)?,
            estimate: parse_clearable(&points, |s| {
                s.parse::<u32>()
                    .map_err(|_| format!("invalid estimate '{s}' (want a number)").into())
            })?,
        },
        Command::Parent { card, parent } => BoardAction::SetParent {
            card: resolve_card(view, &card)?,
            parent: parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?,
        },
        Command::Seq {
            card,
            spec,
            container,
        } => {
            let card = resolve_card(view, &card)?;
            let container = match container.as_deref() {
                Some(sel) => resolve_container(view, sel)?,
                None => Container::BoardRoot(view.id.clone()),
            };
            let position = match spec {
                SeqSpec::First => store::SeqPosition::First,
                SeqSpec::Last => store::SeqPosition::Last,
                SeqSpec::After(a) => store::SeqPosition::After(resolve_card(view, &a)?),
                SeqSpec::Before(a) => store::SeqPosition::Before(resolve_card(view, &a)?),
            };
            let rank = store::seq_rank(view, &container, card, &position)?;
            BoardAction::SetSequence {
                card,
                container,
                rank,
            }
        }
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
        Command::Rename { title } => BoardAction::RenameBoard { title },
        Command::Show { .. }
        | Command::Next { .. }
        | Command::Seed
        | Command::Link { .. }
        | Command::MoveBoard { .. }
        | Command::Board { .. }
        | Command::Login { .. }
        | Command::Logout => {
            unreachable!("handled before build_action")
        }
    })
}

/// Parse a "set or clear" scalar CLI value: `none` (or an empty string) clears
/// the field (`Ok(None)`); any other value is run through `parse`. Shared by the
/// `due`/`estimate` commands so both take `none` to clear.
fn parse_clearable<T>(s: &str, parse: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    parse(s).map(Some)
}

// ---------------------------------------------------------------------------
// board loading
// ---------------------------------------------------------------------------

fn load_board(ndb: &Ndb, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    let txn = Transaction::new(ndb).ok()?;
    event::load_board(ndb, &txn, author, board_id)
}

/// All of `author`'s boards in the cache, sorted by id for a stable listing.
fn list_boards(ndb: &Ndb, author: &Pubkey) -> Vec<BoardView> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let mut boards = event::fold_board(ndb, &txn, author)
        .map(|r| r.finalize())
        .unwrap_or_default();
    boards.sort_by(|a, b| a.id.cmp(&b.id));
    boards
}

/// Live (non-archived) card count across a board's columns.
fn card_count(view: &BoardView) -> usize {
    view.columns.iter().map(|c| c.cards.len()).sum()
}

/// Render the board list, marking `current` with a `*` and dimming each id's
/// title/card-count. Falls back to a hint when the cache holds no boards.
fn print_boards(boards: &[BoardView], current: &str) {
    if boards.is_empty() {
        println!(
            "no boards yet — current selection is '{current}'. Run `headway seed` to create it."
        );
        return;
    }
    for view in boards {
        let mark = if view.id == current { "*" } else { " " };
        let detail =
            nostrdb_net::relay::sync::dim(&format!("{} · {} cards", view.title, card_count(view)));
        println!("{mark} {}  {detail}", view.id);
    }
    if !boards.iter().any(|v| v.id == current) {
        println!(
            "* {current}  {}",
            nostrdb_net::relay::sync::dim("(not created yet — run `headway seed`)")
        );
    }
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
    if let Some(id) = event::resolve_card_by_wordid(view, words) {
        return Ok(id);
    }

    let mut hits = event::all_cards(view).filter(|c| c.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(c), None) => Ok(c.id),
        (Some(_), Some(_)) => Err(format!("ambiguous card prefix '{sel}'").into()),
        _ => Err(format!("no card matching '{sel}'").into()),
    }
}

/// Resolve an `--in` container selector: a card ref names that card's container
/// (its subissues); anything else must be the board's own slug, naming the
/// board-root work-order.
fn resolve_container(view: &BoardView, sel: &str) -> Result<Container> {
    match resolve_card(view, sel) {
        Ok(id) => Ok(Container::Card(*id.bytes())),
        Err(_) if sel.eq_ignore_ascii_case(&view.id) => Ok(Container::BoardRoot(view.id.clone())),
        Err(e) => Err(e),
    }
}

/// Resolve a `--reply-to` selector against the comments on `card`, accepting a
/// full hex id, a unique hex prefix, or a comment word-id — the same forms
/// [`resolve_card`] accepts, but scoped to one card's thread.
fn resolve_comment(view: &BoardView, card: &NoteId, sel: &str) -> Result<NoteId> {
    let comments = event::all_cards(view)
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
                "  {}{}{}{}  {}",
                priority_prefix(c.priority),
                c.title,
                progress_suffix(c),
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

/// Render every board in the cache. In text mode each board is printed with
/// [`print_board`], the boards separated by a blank line; in JSON mode they
/// become a single array so the combined output stays machine-parseable.
fn print_all_boards(boards: &[BoardView], as_json: bool, show_archived: bool) {
    if as_json {
        let arr: Vec<_> = boards.iter().map(event::board_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    if boards.is_empty() {
        println!("no boards yet — run `headway seed` to create one");
        return;
    }
    for (i, view) in boards.iter().enumerate() {
        if i > 0 {
            println!();
        }
        // Lead with the addressable slug: board titles can collide (two boards
        // both titled "Headway"), and the slug is what `--board`/`board <id>`
        // take, so it's the anchor for the human-readable title that follows.
        println!("{}", nostrdb_net::relay::sync::dim(&view.id));
        print_board(view, false, show_archived);
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

/// Print the work-order frontier for `next`: the ready cards of `container`, each
/// as a `board#word-id` ref an agent can paste straight into `move`/`show`.
///
/// How many: `-n <k>` caps to `k`; otherwise `--ready` prints the whole ready set
/// (the parallel-dispatch frontier) and the default prints just the single next
/// card. In JSON mode it's an array of `{ref, id, title}` objects.
fn print_next(
    view: &BoardView,
    container: &Container,
    ready: bool,
    limit: Option<usize>,
    as_json: bool,
) {
    let frontier = traversal::ready(view, container);
    let count = match (ready, limit) {
        (_, Some(k)) => k,
        (true, None) => frontier.len(),
        (false, None) => 1,
    };
    let picked = frontier.into_iter().take(count);

    if as_json {
        let arr: Vec<_> = picked
            .map(|c| {
                json!({
                    "ref": plain_ref(view, &c.id),
                    "id": c.id.hex(),
                    "title": c.title,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
        );
        return;
    }

    let mut any = false;
    for c in picked {
        any = true;
        println!(
            "{}  {}",
            plain_ref(view, &c.id),
            nostrdb_net::relay::sync::dim(&c.title)
        );
    }
    if !any {
        eprintln!(
            "{}",
            nostrdb_net::relay::sync::dim(
                "nothing ready — the work-order is empty or everything is done"
            )
        );
    }
}

/// A card's `board#word-id` ref, undimmed — unlike [`card_ref`], `next` output is
/// the thing an agent copies, so the ref itself must stay plain text.
fn plain_ref(view: &BoardView, id: &NoteId) -> String {
    format!("{}#{}", view.id, wordid::encode(id.bytes()))
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
    if card.priority != Priority::None {
        println!("priority {}", card.priority.as_str());
    }
    if let Some(due) = card.due {
        println!("due     {due}");
    }
    if let Some(estimate) = card.estimate {
        println!("estimate {estimate}");
    }
    if let Some(parent) = &card.parent {
        println!("parent  {}", card_ref(view, parent));
    }
    println!("created {}", headway::fmt::rel_time(card.created_at));
    if card.updated_at > card.created_at {
        println!("updated {}", headway::fmt::rel_time(card.updated_at));
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

    if !card.subissues.is_empty() {
        let done = card.subissues.iter().filter(|s| s.done).count();
        println!("\nsubissues ({done}/{} done)", card.subissues.len());
        for s in &card.subissues {
            let mark = if s.done { "x" } else { " " };
            // Where the child sits, when we know: its column, or "archived".
            let place = if s.archived {
                Some("archived".to_string())
            } else {
                s.column.clone()
            };
            let place = place.map_or(String::new(), |p| {
                format!("  {}", nostrdb_net::relay::sync::dim(&format!("({p})")))
            });
            println!("    [{mark}] {}{place}  {}", s.title, card_ref(view, &s.id));
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
        headway::fmt::short_author(&c.author),
        nostrdb_net::relay::sync::dim(&headway::fmt::rel_time(c.created_at)),
        nostrdb_net::relay::sync::dim(&format!("#{}", wordid::encode(c.id.bytes()))),
    );
    if let Some(parent) = &c.parent {
        header.push_str(&nostrdb_net::relay::sync::dim(&format!(
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

/// A compact at-a-glance priority marker printed before a card's title on the
/// board listing: a dim glyph for the lower priorities and a bold `!` for urgent,
/// empty for [`Priority::None`] so unprioritised cards stay unadorned.
fn priority_prefix(priority: Priority) -> String {
    match priority {
        Priority::None => String::new(),
        Priority::Low => nostrdb_net::relay::sync::dim("↓ "),
        Priority::Medium => nostrdb_net::relay::sync::dim("= "),
        Priority::High => nostrdb_net::relay::sync::dim("↑ "),
        Priority::Urgent => "! ".to_string(),
    }
}

/// A dim `n/m` subissue rollup shown after a parent card's title on the board
/// listing; empty for cards with no children.
fn progress_suffix(card: &CardView) -> String {
    if card.subissues.is_empty() {
        return String::new();
    }
    let done = card.subissues.iter().filter(|s| s.done).count();
    format!(
        "  {}",
        nostrdb_net::relay::sync::dim(&format!("{done}/{}", card.subissues.len()))
    )
}

/// A card's human-friendly reference: the board slug, a `#`, then three words,
/// e.g. `headway#maple-river-canyon` — GitHub's `repo#id` shape, so it reads as a
/// reference inline (`Fixes: headway#maple-river-canyon`) and in chat. Just a
/// rendering of the event id — see [`headway::wordid`]. Rendered muted so the
/// title stays the eye's anchor.
fn card_ref(view: &BoardView, id: &NoteId) -> String {
    nostrdb_net::relay::sync::dim(&format!("{}#{}", view.id, wordid::encode(id.bytes())))
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
    /// Whether `board` was named explicitly — by `--board` or a self-routing
    /// `<board>#<word-id>` card ref — rather than falling back to
    /// `$HEADWAY_BOARD`, the persisted current board, or the default. `next`
    /// requires this so an autonomous agent can't be raced by another session
    /// flipping the persisted board between commands.
    board_explicit: bool,
    json: bool,
    archived: bool,
    /// `show` renders every board in the cache instead of just the current one.
    all: bool,
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
            .or_else(|| nostrdb_net::relay::sync::stored_nsec(APP));
        let mut relay = env::var("HEADWAY_RELAY")
            .ok()
            .unwrap_or_else(|| nostrdb_net::relay::sync::DEFAULT_RELAY.to_string());
        let mut db = None;
        // Resolved after the command is parsed: `--board` overrides the board
        // named by a `<board>#<word-id>` card ref, which overrides
        // `$HEADWAY_BOARD`, which overrides the board stored by `headway board
        // <id>`, which overrides the default board.
        let mut board: Option<String> = None;
        let mut author = None;
        let mut json = false;
        let mut archived = false;
        let mut all = false;
        let mut col = None;
        let mut row = None;
        let mut to = None;
        let mut reply_to = None;
        let mut parent = None;
        let mut labels: Vec<String> = Vec::new();
        let mut seq = SeqFlags::default();
        // `next` flags.
        let mut ready = false;
        let mut count: Option<usize> = None;
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
                "--board" => board = Some(value("--board")?),
                "--author" => author = Some(Pubkey::parse(&value("--author")?)?),
                "--col" => col = Some(value("--col")?),
                "--to" => to = Some(value("--to")?),
                "--reply-to" => reply_to = Some(value("--reply-to")?),
                "--parent" => parent = Some(value("--parent")?),
                "--after" => seq.after = Some(value("--after")?),
                "--before" => seq.before = Some(value("--before")?),
                "--first" => seq.first = true,
                "--last" => seq.last = true,
                "--in" => seq.container = Some(value("--in")?),
                "--ready" => ready = true,
                "-n" | "--count" => {
                    count = Some(value("-n")?.parse().map_err(|_| "-n must be a number")?)
                }
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
        let command = parse_command(
            name, rest, col, row, to, reply_to, parent, labels, seq, ready, count,
        )?;

        // A card selector like `commerce#purse-metal-toilet` already names its
        // board, so the command self-routes there — the display id `show`
        // prints is a working address wherever it's pasted, with no `--board`
        // needed. An explicit `--board` must agree with it rather than being
        // silently ignored (or silently winning and then failing "no card
        // matching" on the wrong board).
        if let Some(named) = command.selector_board()? {
            match &board {
                Some(flag) if flag != &named => {
                    return Err(
                        format!("--board {flag} conflicts with card ref board '{named}'").into(),
                    );
                }
                _ => board = Some(named),
            }
        }
        // Explicit iff `--board` or a self-routing ref set it above; the env /
        // persisted / default fallbacks below are implicit (see `board_explicit`).
        let board_explicit = board.is_some();
        let board = board
            .or_else(|| env::var("HEADWAY_BOARD").ok())
            .or_else(|| nostrdb_net::relay::sync::read_config(APP, "board"))
            .unwrap_or_else(|| store::BOARD_ID.to_string());

        // `login`/`logout` manage the stored key themselves, so don't parse (and
        // potentially reject on) whatever key is currently configured — that would
        // keep `login` from replacing a stale or malformed stored key.
        // `parse_nsec` hands back a `nostrdb_net::Pubkey`; the rest of the CLI
        // (and the `headway` store/event layer) speaks `enostr::Pubkey`. Both are
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

        Ok(Some(Cli {
            secret,
            author,
            relay,
            db,
            board,
            board_explicit,
            json,
            archived,
            all,
            command,
        }))
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_command(
    name: &str,
    rest: &[String],
    col: Option<String>,
    row: Option<usize>,
    to: Option<String>,
    reply_to: Option<String>,
    parent: Option<String>,
    labels: Vec<String>,
    seq: SeqFlags,
    ready: bool,
    count: Option<usize>,
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
            parent,
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
        "priority" => Command::Priority {
            card: card()?,
            level: arg(rest, 1, name)?,
        },
        "due" => Command::Due {
            card: card()?,
            date: arg(rest, 1, name)?,
        },
        "estimate" => Command::Estimate {
            card: card()?,
            points: arg(rest, 1, name)?,
        },
        "seq" => Command::Seq {
            card: card()?,
            spec: seq_spec(&seq)?,
            container: seq.container,
        },
        "next" => Command::Next {
            container: seq.container,
            ready,
            limit: count,
        },
        // `parent <card> <parent>` sets, `parent <card>` detaches — mirrors how
        // `label` with no labels clears.
        "parent" => Command::Parent {
            card: card()?,
            parent: rest.get(1).cloned(),
        },
        "comment" => Command::Comment {
            card: card()?,
            body: joined(rest, 1, name)?,
            reply_to,
        },
        "delete" => Command::Delete { card: card()? },
        "archive" => Command::Archive { card: card()? },
        "restore" => Command::Restore { card: card()? },
        "link" => Command::Link {
            card: card()?,
            to_board: to.ok_or("link needs --to <board>")?,
        },
        "move-board" => Command::MoveBoard {
            card: card()?,
            to_board: to.ok_or("move-board needs --to <board>")?,
        },
        "rename" => Command::Rename {
            title: joined(rest, 0, name)?,
        },
        "board" => Command::Board {
            id: rest.first().cloned(),
        },
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
                               detail (--archived to list archived, --all for
                               every board, --json for machine output)
    seed                       Seed the default board if none exists
    add <title...>             Add a card (--col <c> column, -l <labels> to tag,
                               --parent <card> to create it as a subissue)
    move <card> --col <c>      Move a card to a column (--row to position)
    title <card> <title...>    Edit a card's title
    desc <card> <text...>      Edit a card's description
    label <card> [labels...]   Set a card's labels (empty clears)
    priority <card> <level>    Set priority (none/low/medium/high/urgent)
    due <card> <date>          Set a due date (YYYY-MM-DD, or none to clear)
    estimate <card> <n>        Set an estimate (a number, or none to clear)
    seq <card> <pos> [--in <c>] Position a card in a container's work-order:
                               <pos> = --first | --last | --after <card> |
                               --before <card>; --in is a card ref (its
                               subissues) or the board slug (board root)
    next [--in <c>]            Print what to work on next: the ready frontier of
                               a container's work-order, each a board#word-id ref.
                               --ready prints the whole set, -n <k> caps it. Needs
                               --board or an --in <board#word-id> ref (never the
                               persisted current board).
    parent <card> [parent]     Make a card a subissue of [parent] (omit to
                               detach)
    comment <card> <text...>   Comment on a card (--reply-to <c> to thread under
                               another comment)
    delete <card>              Remove a card (reversible tombstone)
    archive <card>             Archive a card off the board
    restore <card>             Restore an archived card
    link <card> --to <board>   Also place a card on another board (keep both)
    move-board <card> --to <b> Move a card from this board to another board
    rename <title...>          Rename the current board's display title (slug
                               unchanged)
    board [id]                 Switch the current board to <id>, or list boards
                               and mark the current one
    login <nsec>               Store a signing key for later runs
    logout                     Forget the stored signing key

    <card> is a card id or a unique short prefix (see `show`). A full
    <board>#<word-id> ref routes the command to that board automatically.
    <c> is a column id or name (case-insensitive).

OPTIONS:
    --nsec <nsec>     Signing key for this run. Normally unnecessary — run
                      `headway login` once and it's reused. ($HEADWAY_NSEC,
                      if set, takes precedence over the stored key.)
    --author <pk>     Board author to read (defaults to the signer)
    --relay <url>     Relay URL (or $HEADWAY_RELAY) [default: {DEFAULT_RELAY}]
    --board <id>      Board for this run (or $HEADWAY_BOARD). Normally
                      unnecessary — a <board>#<word-id> card ref routes itself,
                      and `headway board <id>` sets it persistently.
                      [default: {board}]
    --db <path>       nostrdb cache dir [default: <data-dir>/headway-cli]
    -l, --label <l>   Label(s) for `add` (repeatable; comma-separated allowed)
    --col <c>         Column for `add`/`move` (id or name)
    --to <board>      Target board for `link`/`move-board`
    --reply-to <c>    Parent comment for `comment` (id, prefix, or word-id)
    --parent <card>   Parent card for `add` (created as its subissue)
    --in <c>          Container for `seq`/`next` (card ref or board slug)
    --ready           Print the whole ready set for `next` (not just the first)
    -n, --count <k>   Cap how many cards `next` prints
    --json            Machine-readable output (show, next)
    --archived        List archived cards in full (show)
    --all             Show every board in the cache, not just the current (show)
    -h, --help        Print this help",
        DEFAULT_RELAY = nostrdb_net::relay::sync::DEFAULT_RELAY,
        board = store::BOARD_ID,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse(args.iter().map(|s| s.to_string()))
            .expect("parse ok")
            .expect("a command")
    }

    /// A full `<board>#<word-id>` selector routes the run to that board; the
    /// forms `show` prints for other addressing (bare `#words`, plain words,
    /// hex prefixes) don't.
    #[test]
    fn card_ref_routes_to_its_board() {
        assert_eq!(
            parse(&["show", "commerce#purse-metal-toilet"]).board,
            "commerce"
        );
        assert_eq!(
            parse(&["move", "dave#stage-injury-surprise", "--col", "done"]).board,
            "dave"
        );
        // Case-normalised like the slugs themselves.
        assert_eq!(
            parse(&["show", "Commerce#purse-metal-toilet"]).board,
            "commerce"
        );
    }

    /// `priority <card> <level>` parses into a Priority command, and a full
    /// card ref self-routes it to that card's board.
    #[test]
    fn priority_command_parses_and_routes() {
        let cli = parse(&["priority", "commerce#purse-metal-toilet", "high"]);
        assert_eq!(cli.board, "commerce");
        match cli.command {
            Command::Priority { card, level } => {
                assert_eq!(card, "commerce#purse-metal-toilet");
                assert_eq!(level, "high");
            }
            _ => panic!("expected a Priority command"),
        }
    }

    /// `--all` is an independent flag on `show`, off unless passed.
    #[test]
    fn all_flag_toggles_show() {
        assert!(!parse(&["show"]).all);
        assert!(parse(&["show", "--all"]).all);
    }

    #[test]
    fn non_ref_selectors_do_not_route() {
        // These fall through to the usual precedence; with no env/config in a
        // test environment that may be the stored board, so only assert the
        // selector itself didn't force one by checking against a routed run.
        let routed = parse(&["show", "commerce#purse-metal-toilet"]).board;
        assert_eq!(routed, "commerce");
        for sel in ["#purse-metal-toilet", "purse-metal-toilet", "2716e5db"] {
            let cli = parse(&["show", sel]);
            // Whatever board was picked, it wasn't derived from the selector.
            assert_eq!(cli.command.selector_board().unwrap(), None);
        }
    }

    /// Parse args expecting an error, returning its message.
    fn parse_err(args: &[&str]) -> String {
        match Cli::parse(args.iter().map(|s| s.to_string())) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected an error for {args:?}"),
        }
    }

    #[test]
    fn conflicting_refs_error() {
        let err = parse_err(&["show", "commerce#a-b-c", "dave#d-e-f"]);
        assert!(err.contains("different boards"), "{err}");

        let err = parse_err(&["--board", "headway", "show", "commerce#a-b-c"]);
        assert!(err.contains("conflicts"), "{err}");
    }

    /// `--board` agreeing with the ref is fine, and two refs naming the same
    /// board are too.
    #[test]
    fn agreeing_refs_are_fine() {
        assert_eq!(
            parse(&["--board", "commerce", "show", "commerce#a-b-c"]).board,
            "commerce"
        );
        assert_eq!(
            parse(&["show", "commerce#a-b-c", "commerce#d-e-f"]).board,
            "commerce"
        );
    }

    /// `next --in <board#word-id>` self-routes to the ref's board and marks the
    /// board explicit, and `--ready`/`-n` parse into the command.
    #[test]
    fn next_parses_and_routes() {
        let cli = parse(&[
            "next",
            "--in",
            "notedeck#saddle-because-liquid",
            "--ready",
            "-n",
            "3",
        ]);
        assert_eq!(cli.board, "notedeck");
        assert!(cli.board_explicit);
        match cli.command {
            Command::Next {
                container,
                ready,
                limit,
            } => {
                assert_eq!(container.as_deref(), Some("notedeck#saddle-because-liquid"));
                assert!(ready);
                assert_eq!(limit, Some(3));
            }
            _ => panic!("expected a Next command"),
        }
    }

    /// `--board` marks the board explicit for the statelessness guard; a bare
    /// `next` (no ref, no `--board`) does not.
    #[test]
    fn next_board_explicitness() {
        assert!(parse(&["next", "--board", "headway"]).board_explicit);
        assert!(!parse(&["next"]).board_explicit);
        assert!(!parse(&["next", "--in", "headway"]).board_explicit);
    }

    #[test]
    fn ref_board_shapes() {
        assert_eq!(ref_board("commerce#a-b-c"), Some("commerce".into()));
        assert_eq!(ref_board("ios-port#a-b-c"), Some("ios-port".into()));
        assert_eq!(ref_board("#a-b-c"), None);
        assert_eq!(ref_board("a-b-c"), None);
        assert_eq!(ref_board("commerce#"), None);
        assert_eq!(ref_board("not a slug#a-b-c"), None);
    }
}
