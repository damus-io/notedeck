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

use headway::event::{
    self, BoardView, CardView, CommentView, Container, Date, Priority, resolve_card,
};
use headway::store::{self, BoardAction, Publisher};
use headway::{teams, traversal, wordid};

use nostrdb_net::relay::sync::Result;

/// The CLI's cache/key directory under the platform data dir (e.g.
/// `~/.local/share/headway-cli` on Linux).
const APP: &str = "headway-cli";

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`headway show | head`) instead of
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

/// A parsed command. Card arguments are still raw strings here; they're resolved
/// against the board once it's folded.
enum Command {
    Show {
        /// Optional card selectors. When non-empty, `show` prints these cards
        /// in full `git show`-style detail rather than the whole board.
        cards: Vec<String>,
    },
    Seed,
    /// Migrate the current board to SNS: seal it under a fresh per-board team key
    /// so it can be shared, re-sealing existing notes in place (no data loss). See
    /// [`store::migrate_board_to_sns`].
    Migrate,
    Add {
        title: String,
        col: Option<String>,
        labels: Vec<String>,
        /// A card to parent the new one under (created as a subissue).
        parent: Option<String>,
        /// Initial description (cover note), already resolved from `--desc` or
        /// `--desc-file`. `None` means no description event is emitted.
        description: Option<String>,
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
    /// Record that `card` is blocked by `on` (a dependency edge, independent of
    /// the parent axis and allowed to cross boards).
    Block {
        card: String,
        on: String,
    },
    /// Remove the `card`-blocked-by-`on` dependency edge.
    Unblock {
        card: String,
        on: String,
    },
    /// Relate `card` to `other` (an undirected "see also" edge — symmetric,
    /// carries no ordering or readiness meaning, and allowed to cross boards).
    Relate {
        card: String,
        other: String,
    },
    /// Remove the `card`-relates-`other` edge (symmetric: from either endpoint).
    Unrelate {
        card: String,
        other: String,
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
    /// The board named by this command's card selectors, when any of them is a
    /// `headway:<board>/<word-id>` reference (or the scheme-less shorthand — see
    /// [`ref_board`]). Used to self-route the command to that board. Errors when
    /// two selectors name different boards — cards are resolved against a single
    /// board per run.
    fn selector_board(&self) -> Result<Option<String>> {
        let mut selectors: Vec<&str> = Vec::new();
        match self {
            Command::Show { cards } => selectors.extend(cards.iter().map(String::as_str)),
            Command::Add { parent, .. } => selectors.extend(parent.as_deref()),
            Command::Parent { card, parent } => {
                selectors.push(card);
                selectors.extend(parent.as_deref());
            }
            Command::Block { card, on } | Command::Unblock { card, on } => {
                selectors.push(card);
                selectors.push(on);
            }
            Command::Relate { card, other } | Command::Unrelate { card, other } => {
                selectors.push(card);
                selectors.push(other);
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
            | Command::Migrate
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

/// The `<board>` segment of a card reference — `headway:<board>/<word-id>` or its
/// scheme-less `<board>/<word-id>` shorthand — lowercased (board slugs are
/// lowercase). `None` for anything that isn't a reference: plain word ids, hex
/// ids/prefixes, and segments that aren't slug-shaped (see [`headway::wordid::parse_ref`]).
fn ref_board(sel: &str) -> Option<String> {
    headway::wordid::parse_ref(sel).map(|(board, _)| board.to_lowercase())
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

    // Register the signing key so nostrdb unwraps gift-wraps addressed to us —
    // notably the kind-1082 key-shares a new shared board arrives as. Without it
    // a board shared with us since the last app run stays unjoinable here.
    if let Some((secret, _)) = &cli.secret {
        ndb.add_key(secret);
    }
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

    // Pull the account's gift-wraps before deriving the roster: a key-share only
    // ever travels as a kind-1059, and nostrdb peels it into the queryable 1082
    // the roster is built from. Skipping this is not a missing *feature* — it
    // silently empties the roster, and an empty roster makes every shared board
    // look private, so this CLI would fold it the plaintext way and write
    // plaintext edits no other client can read.
    if let Some(relay) = relay.as_mut() {
        pull_giftwraps(relay, &ndb, &author).await;
    }
    // Join every shared board we hold a key for. `register_teams` re-peels any
    // envelope that arrived before its root was registered, so it is safe for this
    // to run after the sync above rather than before it.
    let roster = Roster::load(&ndb, &author);

    let board = cli.board;
    let board_explicit = cli.board_explicit;
    let as_json = cli.json;
    let show_archived = cli.archived;
    let show_all = cli.all;
    let dry_run = cli.dry_run;
    let new_channel = cli.new_channel;
    let secret = cli.secret.map(|(s, _)| s);

    match cli.command {
        // `--all` fans the render across every board in the cache, ignoring any
        // card selectors (which address a single board).
        Command::Show { .. } if show_all => {
            print_all_boards(&list_boards(&ndb, &roster, &author), as_json, show_archived)
        }

        Command::Show { cards } => match load_board(&ndb, &roster, &author, &board) {
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
            if load_board(&ndb, &roster, &author, &board).is_some() {
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

        Command::Migrate => {
            // Migrate mutates live data and publishes sealed events, so — like
            // `next` — it refuses to lean on the persisted current board: sealing
            // the wrong board is unrecoverable (you can't unpublish from relays).
            if !board_explicit {
                return Err(
                    "migrate never uses the persisted current board — pass --board <id>, \
                     or a self-routing ref that names its own board, so you can't \
                     accidentally seal the wrong board"
                        .into(),
                );
            }
            let secret = secret.ok_or("migrate needs --nsec to sign")?;
            if load_board(&ndb, &roster, &author, &board).is_none() {
                return Err(format!("no board '{board}' to migrate — nothing to seal").into());
            }
            let preview = store::preview_migration(&ndb, &author, &board);
            // Which channel to seal into. A board already in the roster MUST
            // reuse its existing root: its already-sealed notes cannot be moved
            // to a second channel (nostrdb promotes a stored note to a rumor only
            // while it is still plaintext, so re-wrapping a sealed one under a new
            // root is a no-op), and minting a fresh root would leave the board
            // split across two channels with the roster picking between them
            // arbitrarily. An unshared board derives its root instead of minting
            // one, so every device holding the account key agrees on it.
            let (root, reused) = match roster.team_root(&board) {
                Some(root) => (root, true),
                // Creating a channel is a one-way door, so never infer it. "This
                // board has no channel" and "I couldn't see its channel" look
                // identical from here — an unsynced cache, an unregistered account
                // key, a key-share that never arrived — and guessing wrong splits a
                // live board across two roots that can never be merged back. Make
                // the caller say so.
                None if !new_channel => {
                    return Err(format!(
                        "board '{board}' has no channel in this cache, so migrate \
                         would create one. If it is already shared, that would split \
                         it in two — sync first and re-check `headway board`. If it \
                         really is a plaintext board, pass --new-channel to seal it \
                         into a new channel."
                    )
                    .into());
                }
                None => (enostr::sns::derive_board_root(&secret, &board), false),
            };
            let channel = if reused { "existing" } else { "NEW" };
            if dry_run {
                println!(
                    "dry run: would re-seal {} of {} notes on '{board}' into its {channel} channel\n\
                     (the other {} are already sealed — re-wrapping those is a no-op)\n\
                     re-run without --dry-run to publish; sealed events cannot be unpublished",
                    preview.unsealed,
                    preview.total,
                    preview.total - preview.unsealed,
                );
                return Ok(());
            }
            let mut sink = Collect::default();
            let n = store::migrate_board_to_sns(&ndb, &author, &secret, &board, &root, &mut sink);
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "migrated board '{board}' to SNS ({n} notes re-sealed into its {channel} channel){}",
                nostrdb_net::relay::sync::offline_note(&relay)
            );
        }

        Command::Board { id } => match id {
            // Switch: persist the new current board, then report whether it
            // already exists so the next step (seed vs. use) is obvious.
            Some(id) => {
                nostrdb_net::relay::sync::write_config(APP, "board", &id)?;
                match load_board(&ndb, &roster, &author, &id) {
                    Some(view) => {
                        println!("switched to board '{id}' ({} cards)", card_count(&view))
                    }
                    None => {
                        println!("switched to board '{id}' — doesn't exist yet, run `headway seed`")
                    }
                }
            }
            // List: every board in the cache, the current selection marked.
            None => print_boards(&list_boards(&ndb, &roster, &author), &board),
        },

        // Cross-board: place/move a card from the current board onto another.
        // These touch two boards, so they sidestep the single-board `apply` path.
        Command::Link { card, to_board } => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let source = load_board(&ndb, &roster, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let target = load_board(&ndb, &roster, &author, &to_board).ok_or_else(|| {
                format!("no target board '{to_board}' — switch to it and `headway seed` first")
            })?;
            let card_id = resolve_card(&source, &card)?;

            let mut sink = Collect::default();
            let channel = roster.channel(&board);
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
                &store::Signer::new(&secret, channel.as_ref()),
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
            let source = load_board(&ndb, &roster, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let target = load_board(&ndb, &roster, &author, &to_board).ok_or_else(|| {
                format!("no target board '{to_board}' — switch to it and `headway seed` first")
            })?;
            let card_id = resolve_card(&source, &card)?;

            let mut sink = Collect::default();
            let channel = roster.channel(&board);
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
                &store::Signer::new(&secret, channel.as_ref()),
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
                     or an --in <headway:board/word-id> card ref that names its own board"
                        .into(),
                );
            }
            let view = load_board(&ndb, &roster, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let container = match container.as_deref() {
                Some(sel) => resolve_container(&view, sel)?,
                None => Container::BoardRoot(view.id.clone()),
            };
            print_next(&view, &container, ready, limit, as_json);
        }

        edit => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let view = load_board(&ndb, &roster, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            // `add` mints a new card whose id we only learn by re-folding the
            // board and finding the id that wasn't there before; snapshot the
            // existing ids first so we can pick it out. Other edits act on a card
            // the caller already named, so there's nothing new to surface.
            let added = matches!(edit, Command::Add { .. });
            let before: std::collections::HashSet<String> = if added {
                event::all_cards(&view).map(|c| c.id.hex()).collect()
            } else {
                std::collections::HashSet::new()
            };
            let action = build_action(&view, edit)?;

            let mut sink = Collect::default();
            let channel = roster.channel(&board);
            store::apply(
                &ndb,
                &board,
                &view,
                &author,
                &store::Signer::new(&secret, channel.as_ref()),
                action,
                &mut sink,
            );
            if sink.0.is_empty() {
                return Err("action produced no events (unknown card or column?)".into());
            }
            let n = sink.0.len();
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;

            // Surface the created card's ref so a scripted follow-up edit doesn't
            // have to re-`show` to recover it. `apply` ingested locally, so a
            // re-fold sees the new card — the one id absent from `before`.
            let new_card = added
                .then(|| load_board(&ndb, &roster, &author, &board))
                .flatten()
                .and_then(|after| {
                    event::all_cards(&after)
                        .find(|c| !before.contains(&c.id.hex()))
                        .map(|c| (c.id.hex(), plain_ref(&after, &c.id)))
                });

            if as_json {
                let mut obj = serde_json::json!({ "ok": true, "events": n });
                if let Some((hex, card_ref)) = &new_card {
                    obj["card"] = serde_json::Value::String(hex.clone());
                    obj["ref"] = serde_json::Value::String(card_ref.clone());
                }
                println!("{obj}");
            } else {
                let ref_suffix = new_card
                    .as_ref()
                    .map(|(_, card_ref)| format!(" — {}", nostrdb_net::relay::sync::dim(card_ref)))
                    .unwrap_or_default();
                println!(
                    "ok ({n} events){ref_suffix}{}",
                    nostrdb_net::relay::sync::offline_note(&relay)
                );
            }
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
            description,
        } => {
            let col = col.as_deref().map_or(Ok(0), |c| resolve_col(view, c))?;
            let parent = parent
                .as_deref()
                .map(|sel| resolve_card(view, sel))
                .transpose()?;
            BoardAction::AddCard {
                col,
                title,
                description: description.unwrap_or_default(),
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
        Command::Block { card, on } => BoardAction::Block {
            card: resolve_card(view, &card)?,
            on: resolve_card(view, &on)?,
        },
        Command::Unblock { card, on } => BoardAction::Unblock {
            card: resolve_card(view, &card)?,
            on: resolve_card(view, &on)?,
        },
        Command::Relate { card, other } => BoardAction::Relate {
            card: resolve_card(view, &card)?,
            other: resolve_card(view, &other)?,
        },
        Command::Unrelate { card, other } => BoardAction::Unrelate {
            card: resolve_card(view, &card)?,
            other: resolve_card(view, &other)?,
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
        | Command::Migrate
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

/// The shared boards this account holds keys for, resolved once per run.
///
/// A shared board isn't a different kind of board, it's a different *transport*:
/// its edits are sealed into an SNS channel (kind-1081 envelopes under a team
/// key) instead of published as plaintext nostr events, and
/// [`event::fold_shared_board`] reads back only what was sealed. So every read
/// and every write here has to ask the roster which world a board lives in
/// first. Getting it wrong is silent both ways: folding a shared board the
/// plaintext way omits every co-member's edit, and writing plaintext to one
/// produces events no other front end will ever fold.
///
/// Note what this does *not* add: a sync leg for the envelopes themselves. The
/// CLI already pulls a shared board's content, because nostrdb stores the peeled
/// rumor beside the envelope and those rumors are ordinary events matching the
/// plaintext [`event::headway_filter`] the reconcile in `run` already syncs. What
/// was missing was never the events, only the keys to recognise and seal them.
/// The envelope pipe proper belongs to `relay::sync::Session`
/// (headway:notedeck/clap-matrix-machine), not to a fourth one-shot leg here.
struct Roster {
    /// Joined channels, each naming the board coordinate its key unlocks.
    teams: Vec<teams::Team>,
    /// Whose boards we address — the owner half of a board coordinate. The CLI
    /// is single-author (`--author`, else the signing key), so one suffices.
    author: Pubkey,
}

impl Roster {
    /// Derive the roster from nostrdb and register every `team_root` with it.
    ///
    /// Registration is what makes the channel readable *and writable*: our own
    /// sealed edits are ingested as envelopes and only become board events once
    /// nostrdb peels them, so an unregistered root means a write we can't even
    /// read back ourselves.
    fn load(ndb: &Ndb, author: &Pubkey) -> Self {
        let teams = teams::teams_from_ndb(ndb, author);
        teams::register_teams(ndb, &teams);
        Self {
            teams,
            author: *author,
        }
    }

    /// The channel new edits to `board_id` seal into — its *primary* channel (see
    /// [`teams::board_channels`]) — or `None` for a private board. Reads gather
    /// every channel instead; see [`Self::channel_pubkeys`].
    fn channel(&self, board_id: &str) -> Option<store::SnsChannel> {
        let addr = event::board_address(&self.author, board_id);
        let team = *teams::board_channels(&self.teams, &addr).first()?;
        Some(store::SnsChannel {
            keys: team.sns_keys()?,
        })
    }

    /// Every channel `board_id`'s content may be sealed into, primary first — what
    /// a read folds the union of, because a board's history can be split across
    /// channels that cannot be merged back together.
    fn channel_pubkeys(&self, board_id: &str) -> Vec<Pubkey> {
        let addr = event::board_address(&self.author, board_id);
        teams::board_channel_pubkeys(&self.teams, &addr)
    }

    /// The raw `team_root` behind `board_id`'s primary channel — what a re-seal
    /// needs in order to keep sealing a shared board into the channel it already
    /// has, rather than minting a second one (see the `migrate` command).
    fn team_root(&self, board_id: &str) -> Option<[u8; 32]> {
        let addr = event::board_address(&self.author, board_id);
        teams::board_channels(&self.teams, &addr)
            .first()?
            .root_bytes()
    }
}

/// Pull the kind-1059 gift-wraps addressed to `author` into the local cache, so
/// nostrdb can peel out the key-shares the roster is derived from.
///
/// Pull-only and best-effort: gift-wraps are addressed *to* us, so there is
/// nothing of ours to push back, and a relay that can't serve them just leaves us
/// with the boards we already knew about.
async fn pull_giftwraps(relay: &mut nostrdb_net::relay::sync::Relay, ndb: &Ndb, author: &Pubkey) {
    let filter = teams::giftwrap_filter(author);
    let wire = json!({ "kinds": [1059], "#p": [author.hex()] }).to_string();
    let local = match nostrdb_net::relay::sync::local_set(ndb, &filter) {
        Ok(local) => local,
        Err(e) => {
            eprintln!("warning: couldn't index local key-shares: {e}");
            return;
        }
    };
    let before = count_matching(ndb, &filter);
    if let Err(e) = nostrdb_net::relay::sync::pull_reconcile(relay, ndb, &wire, local).await {
        eprintln!("warning: couldn't sync shared-board key-shares: {e}");
    }
    // Peel what just arrived, but only if something did. A gift-wrap is unwrapped
    // against the registered account keys as it is ingested; one that landed in an
    // earlier run — before this key was registered, or before the peel existed —
    // stays sealed and invisible to the roster until this catch-up pass, the
    // giftwrap counterpart of `register_teams`' `process_sns`. It walks *every*
    // stored wrap and attempts a decrypt per wrap, so running it unconditionally
    // would re-try thousands of other people's wraps on every command, printing a
    // failure line for each. Gated on new arrivals it runs at most once per sync
    // that actually brought something.
    if count_matching(ndb, &filter) > before
        && let Ok(txn) = Transaction::new(ndb)
    {
        ndb.process_giftwraps(&txn);
    }
}

/// How many stored notes match `filter`, counted through the index walk rather
/// than by materialising them.
fn count_matching(ndb: &Ndb, filter: &nostrdb::Filter) -> usize {
    let Ok(txn) = Transaction::new(ndb) else {
        return 0;
    };
    ndb.fold(&txn, std::slice::from_ref(filter), 0usize, |n, _note| n + 1)
        .unwrap_or(0)
}

/// Fold one board, by whichever transport it uses: a joined shared board folds
/// by *coordinate* (gathering every member's sealed edits), a private one folds
/// from `author`'s own plaintext events.
fn load_board(ndb: &Ndb, roster: &Roster, author: &Pubkey, board_id: &str) -> Option<BoardView> {
    let txn = Transaction::new(ndb).ok()?;
    let channels = roster.channel_pubkeys(board_id);
    if channels.is_empty() {
        return event::load_board(ndb, &txn, author, board_id);
    }
    event::load_shared_board(
        ndb,
        &txn,
        &event::board_address(author, board_id),
        &channels,
    )
}

/// All of `author`'s boards in the cache, sorted by id for a stable listing.
/// Each shared board is re-folded by coordinate so its card counts match what
/// the app draws, rather than the plaintext-only subset the author fold sees.
fn list_boards(ndb: &Ndb, roster: &Roster, author: &Pubkey) -> Vec<BoardView> {
    let Ok(txn) = Transaction::new(ndb) else {
        return Vec::new();
    };
    let mut boards = event::fold_board(ndb, &txn, author)
        .map(|r| r.finalize())
        .unwrap_or_default();
    for board in &mut boards {
        // Only the shared ones: the author fold above already produced every
        // private board, and re-folding those would pay a whole extra walk each.
        if roster.channel_pubkeys(&board.id).is_empty() {
            continue;
        }
        if let Some(shared) = load_board(ndb, roster, author, &board.id) {
            *board = shared;
        }
    }
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

/// Resolve an `--in` container selector: a card ref names that card's container
/// (its subissues); anything else must be the board's own slug, naming the
/// board-root work-order.
fn resolve_container(view: &BoardView, sel: &str) -> Result<Container> {
    match resolve_card(view, sel) {
        Ok(id) => Ok(Container::Card(*id.bytes())),
        Err(_) if sel.eq_ignore_ascii_case(&view.id) => Ok(Container::BoardRoot(view.id.clone())),
        Err(e) => Err(e.into()),
    }
}

/// Resolve a `--reply-to` selector against the comments on `card`, accepting a
/// full hex id, a unique hex prefix, or a comment word-id. Comments render as a
/// bare word-id in a card's thread (not a `headway:<board>/…` card ref), so the
/// bare word-id is the selector here.
fn resolve_comment(view: &BoardView, card: &NoteId, sel: &str) -> Result<NoteId> {
    let comments = event::all_cards(view)
        .find(|c| c.id == *card)
        .map(|c| c.comments.as_slice())
        .unwrap_or(&[]);

    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();
    if let Some(c) = comments
        .iter()
        .find(|c| wordid::encode(c.id.bytes()) == sel)
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
                "  {}{}{}{}{}  {}",
                blocked_prefix(c),
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
                let mut j = event::card_json(&view.id, card);
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
/// as a `headway:board/word-id` ref an agent can paste straight into `move`/`show`.
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

/// A card's `headway:<board>/<word-id>` ref, undimmed — unlike [`card_ref`],
/// `next` output is the thing an agent copies, so the ref itself must stay plain
/// text.
fn plain_ref(view: &BoardView, id: &NoteId) -> String {
    wordid::card_ref(&view.id, id.bytes())
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

    print_edges(view, "blocked by", &card.blocked_by);
    print_edges(view, "blocks", &card.blocks);
    print_related(view, &card.related);

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
        nostrdb_net::relay::sync::dim(&wordid::encode(c.id.bytes())),
    );
    if let Some(parent) = &c.parent {
        header.push_str(&nostrdb_net::relay::sync::dim(&format!(
            "  ↳ reply to {}",
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

/// Print a card-detail dependency section (`blocked by:` / `blocks:`) — one line
/// per edge, an `x` marking a cleared (done/archived) blocker so an open one
/// stands out. Nothing is printed when there are no edges.
fn print_edges(view: &BoardView, label: &str, edges: &[headway::event::EdgeRef]) {
    if edges.is_empty() {
        return;
    }
    println!("\n{label} ({})", edges.len());
    for e in edges {
        let mark = if e.done { "x" } else { " " };
        println!("    [{mark}] {}  {}", e.title, card_ref(view, &e.id));
    }
}

/// Print a card-detail `related` section — one `<title>  <ref>` line per edge.
/// Unlike [`print_edges`] there is no cleared marker: the relation is undirected
/// and semantics-free ("see also"), so a partner's doneness is irrelevant.
/// Nothing is printed when there are no related edges.
fn print_related(view: &BoardView, edges: &[headway::event::EdgeRef]) {
    if edges.is_empty() {
        return;
    }
    println!("\nrelated ({})", edges.len());
    for e in edges {
        println!("    {}  {}", e.title, card_ref(view, &e.id));
    }
}

/// A dim ⊘ glyph flagging a card as blocked (held back by an unfinished blocker)
/// on the board listing; empty when the card is free to work on.
fn blocked_prefix(card: &CardView) -> String {
    if card.is_blocked() {
        nostrdb_net::relay::sync::dim("⊘ ")
    } else {
        String::new()
    }
}

/// A card's human-friendly reference: `headway:<board>/<word-id>`, e.g.
/// `headway:dave/maple-river-canyon` — a URI scheme so it reads as a reference
/// inline (`Fixes: headway:dave/maple-river-canyon`) and in chat, survives
/// nostrdb tokenization, and needs no shell quoting. Just a rendering of the event
/// id — see [`headway::wordid`]. Rendered muted so the title stays the eye's anchor.
fn card_ref(view: &BoardView, id: &NoteId) -> String {
    nostrdb_net::relay::sync::dim(&wordid::card_ref(&view.id, id.bytes()))
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
    /// `migrate` reports what it would re-seal and publishes nothing. The seal is
    /// irreversible once it reaches a relay, so the dry run is how you look first.
    dry_run: bool,
    /// `migrate` may create a channel for a board that has none. Off by default:
    /// a board whose channel this cache merely can't *see* would be split in two.
    new_channel: bool,
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
        let mut dry_run = false;
        let mut new_channel = false;
        let mut col = None;
        let mut row = None;
        let mut to = None;
        let mut reply_to = None;
        let mut parent = None;
        // `add` description, from either the inline `--desc` or the escaping-free
        // `--desc-file` (a path, or `-` for stdin); folded into one value below.
        let mut desc = None;
        let mut desc_file = None;
        let mut on = None;
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
                "--desc" => desc = Some(value("--desc")?),
                "--desc-file" => desc_file = Some(value("--desc-file")?),
                "--on" => on = Some(value("--on")?),
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
                "--dry-run" => dry_run = true,
                "--new-channel" => new_channel = true,
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag '{other}'").into());
                }
                _ => positionals.push(arg),
            }
        }

        let Some((name, rest)) = positionals.split_first() else {
            return Ok(None);
        };
        // Fold `--desc`/`--desc-file` into one description: they name the same
        // thing (the new card's cover note), so passing both is a contradiction. A
        // file value of `-` means stdin, which lets a heredoc pipe a long markdown
        // description in with no shell-escaping.
        let description = match (desc, desc_file) {
            (Some(_), Some(_)) => {
                return Err("pass either --desc or --desc-file, not both".into());
            }
            (Some(text), None) => Some(text),
            (None, Some(path)) => Some(read_desc_source(&path)?),
            (None, None) => None,
        };
        let command = parse_command(
            name,
            rest,
            col,
            row,
            to,
            reply_to,
            parent,
            description,
            on,
            labels,
            seq,
            ready,
            count,
        )?;

        // A card selector like `headway:commerce/purse-metal-toilet` already names
        // its board, so the command self-routes there — the display id `show`
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
            dry_run,
            new_channel,
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
    description: Option<String>,
    on: Option<String>,
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
        "migrate" => Command::Migrate,
        "add" => Command::Add {
            title: joined(rest, 0, name)?,
            col,
            labels,
            parent,
            description,
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
        // `block <card> --on <blocker>`; the blocker may also be a second
        // positional so `block <card> <blocker>` works too.
        "block" => Command::Block {
            card: card()?,
            on: on
                .or_else(|| rest.get(1).cloned())
                .ok_or("block needs --on <card>")?,
        },
        "unblock" => Command::Unblock {
            card: card()?,
            on: on
                .or_else(|| rest.get(1).cloned())
                .ok_or("unblock needs --on <card>")?,
        },
        // `relate <card> --to <other>`; the partner may also be a second positional
        // so `relate <card> <other>` works too.
        "relate" => Command::Relate {
            card: card()?,
            other: to
                .or_else(|| rest.get(1).cloned())
                .ok_or("relate needs --to <card>")?,
        },
        "unrelate" => Command::Unrelate {
            card: card()?,
            other: to
                .or_else(|| rest.get(1).cloned())
                .ok_or("unrelate needs --to <card>")?,
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

/// Read a `--desc-file` value into description text: `-` reads stdin (so a long
/// markdown description can heredoc in with no shell-escaping), anything else is
/// a file path. Trailing whitespace is trimmed so a heredoc's closing newline
/// doesn't ride along.
fn read_desc_source(path: &str) -> Result<String> {
    use std::io::Read;
    let mut text = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut text)?;
    } else {
        text = std::fs::read_to_string(path)?;
    }
    Ok(text.trim_end().to_string())
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
    migrate                    Migrate this board to SNS: seal it under a fresh
                               per-board key so it can be shared, re-sealing
                               existing notes in place (no data loss)
    add <title...>             Add a card (--col <c> column, -l <labels> to tag,
                               --parent <card> to create it as a subissue,
                               --desc <text>/--desc-file <path> for a description)
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
                               a container's work-order, each a headway:board/word-id
                               ref. --ready prints the whole set, -n <k> caps it. Needs
                               --board or an --in <headway:board/word-id> ref (never the
                               persisted current board).
    parent <card> [parent]     Make a card a subissue of [parent] (omit to
                               detach)
    block <card> --on <b>      Mark <card> as blocked by <b> (a dependency edge,
                               may cross boards; cycles are refused)
    unblock <card> --on <b>    Remove the <card>-blocked-by-<b> edge
    relate <card> --to <o>     Relate <card> to <o> (undirected \"see also\"; shows
                               on both, may cross boards, no ordering meaning)
    unrelate <card> --to <o>   Remove the <card>-relates-<o> edge (either endpoint)
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
    --to <board>      Target board for `link`/`move-board`; or the partner card
                      for `relate`/`unrelate`
    --reply-to <c>    Parent comment for `comment` (id, prefix, or word-id)
    --parent <card>   Parent card for `add` (created as its subissue)
    --desc <text>     Initial description for `add` (mutually exclusive with
                      --desc-file)
    --desc-file <p>   Like --desc, but read the description from file <p> (or
                      stdin when <p> is `-`) — pass a long/multi-line markdown
                      description with no shell-escaping (heredoc)
    --on <card>       Blocker card for `block`/`unblock`
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

    /// A `headway:<board>/<word-id>` selector (or the scheme-less shorthand)
    /// routes the run to that board; a bare word-id or hex prefix doesn't.
    #[test]
    fn card_ref_routes_to_its_board() {
        assert_eq!(
            parse(&["show", "headway:commerce/purse-metal-toilet"]).board,
            "commerce"
        );
        assert_eq!(
            parse(&["move", "dave/stage-injury-surprise", "--col", "done"]).board,
            "dave"
        );
        // Case-normalised like the slugs themselves.
        assert_eq!(
            parse(&["show", "headway:Commerce/purse-metal-toilet"]).board,
            "commerce"
        );
    }

    /// `priority <card> <level>` parses into a Priority command, and a full
    /// card ref self-routes it to that card's board.
    #[test]
    fn priority_command_parses_and_routes() {
        let cli = parse(&["priority", "headway:commerce/purse-metal-toilet", "high"]);
        assert_eq!(cli.board, "commerce");
        match cli.command {
            Command::Priority { card, level } => {
                assert_eq!(card, "headway:commerce/purse-metal-toilet");
                assert_eq!(level, "high");
            }
            _ => panic!("expected a Priority command"),
        }
    }

    /// `add --desc <text>` folds the inline text into the card's description.
    #[test]
    fn add_desc_inline_sets_description() {
        let cli = parse(&["add", "a card", "--desc", "the details"]);
        match cli.command {
            Command::Add {
                title, description, ..
            } => {
                assert_eq!(title, "a card");
                assert_eq!(description.as_deref(), Some("the details"));
            }
            _ => panic!("expected an Add command"),
        }
    }

    /// `add --desc-file <path>` reads the description from a file, trimming the
    /// trailing whitespace a heredoc's closing newline leaves behind while
    /// keeping interior newlines.
    #[test]
    fn add_desc_file_is_read_and_trimmed() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "## Heading\n\nbody line\n\n").unwrap();
        let cli = parse(&["add", "a card", "--desc-file", f.path().to_str().unwrap()]);
        match cli.command {
            Command::Add { description, .. } => {
                assert_eq!(description.as_deref(), Some("## Heading\n\nbody line"))
            }
            _ => panic!("expected an Add command"),
        }
    }

    /// `--desc` and `--desc-file` name the same thing, so passing both errors
    /// rather than silently letting one win.
    #[test]
    fn add_desc_and_desc_file_conflict() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "from file").unwrap();
        let res = Cli::parse(
            [
                "add",
                "a card",
                "--desc",
                "inline",
                "--desc-file",
                f.path().to_str().unwrap(),
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let err = match res {
            Err(e) => e,
            Ok(_) => panic!("--desc + --desc-file should conflict"),
        };
        assert!(err.to_string().contains("not both"), "{err}");
    }

    /// A board is shared iff the roster holds a key for its full *coordinate*.
    /// Matching on the slug alone would be wrong in both directions: it would
    /// seal edits to our own private board because a *co-member's* board happens
    /// to share its slug, and — the case that actually bites — it would let a
    /// board we own but never shared fold through the team-key path, which
    /// ingests only sealed rumors and would show it empty.
    #[test]
    fn a_board_is_shared_only_at_its_own_coordinate() {
        let owner = enostr::FullKeypair::generate().pubkey;
        let other = enostr::FullKeypair::generate().pubkey;
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x42;
        let roster = Roster {
            teams: vec![
                // A board we own and shared.
                teams::Team {
                    team_root: hex::encode(root),
                    board_addr: event::board_address(&owner, "shared"),
                    epoch: None,
                    shared_at: 0,
                },
                // Someone else's board that happens to use a slug we also use.
                teams::Team {
                    team_root: hex::encode(root),
                    board_addr: event::board_address(&other, "private"),
                    epoch: None,
                    shared_at: 0,
                },
            ],
            author: owner,
        };

        assert!(
            roster.channel("shared").is_some(),
            "our own shared board must resolve to its channel"
        );
        assert!(
            roster.channel("private").is_none(),
            "a slug match under a different owner is not our board"
        );
        assert!(roster.channel("never-shared").is_none());
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
        let routed = parse(&["show", "headway:commerce/purse-metal-toilet"]).board;
        assert_eq!(routed, "commerce");
        for sel in ["purse-metal-toilet", "2716e5db"] {
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
        let err = parse_err(&["show", "commerce/a-b-c", "dave/d-e-f"]);
        assert!(err.contains("different boards"), "{err}");

        let err = parse_err(&["--board", "headway", "show", "commerce/a-b-c"]);
        assert!(err.contains("conflicts"), "{err}");
    }

    /// `--board` agreeing with the ref is fine, and two refs naming the same
    /// board are too.
    #[test]
    fn agreeing_refs_are_fine() {
        assert_eq!(
            parse(&["--board", "commerce", "show", "commerce/a-b-c"]).board,
            "commerce"
        );
        assert_eq!(
            parse(&["show", "commerce/a-b-c", "commerce/d-e-f"]).board,
            "commerce"
        );
    }

    /// `next --in <headway:board/word-id>` self-routes to the ref's board and marks
    /// the board explicit, and `--ready`/`-n` parse into the command.
    #[test]
    fn next_parses_and_routes() {
        let cli = parse(&[
            "next",
            "--in",
            "headway:notedeck/saddle-because-liquid",
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
                assert_eq!(
                    container.as_deref(),
                    Some("headway:notedeck/saddle-because-liquid")
                );
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

    /// `migrate` seals live data under a fresh key, so it must name its board
    /// explicitly — a bare `migrate` is not `board_explicit`, so dispatch rejects
    /// it rather than sealing the persisted current board.
    #[test]
    fn migrate_board_explicitness() {
        assert!(!parse(&["migrate"]).board_explicit);
        assert!(parse(&["migrate", "--board", "commerce"]).board_explicit);
    }

    #[test]
    fn ref_board_shapes() {
        // Full scheme form and scheme-less shorthand both name their board.
        assert_eq!(ref_board("headway:commerce/a-b-c"), Some("commerce".into()));
        assert_eq!(ref_board("commerce/a-b-c"), Some("commerce".into()));
        assert_eq!(ref_board("ios-port/a-b-c"), Some("ios-port".into()));
        // A bare word-id, hex, or empty segment is not a reference.
        assert_eq!(ref_board("a-b-c"), None);
        assert_eq!(ref_board("/a-b-c"), None);
        assert_eq!(ref_board("commerce/"), None);
        assert_eq!(ref_board("not a slug/a-b-c"), None);
    }
}
