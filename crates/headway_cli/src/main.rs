//! `headway` — a CLI for reading and mutating a Headway board against a running
//! notedeck's embedded relay.
//!
//! The cache/sync/relay plumbing — keeping the CLI's own nostrdb, reconciling it
//! against the app's relay with NIP-77 negentropy, and the stored signing key —
//! lives in `nostrdb_net`'s `relay::sync` module (see [`notebook_cli`] for the
//! other consumer). This file is just the board's command surface: parsing,
//! resolving card/column arguments against the folded board, and rendering.

mod boards;
mod edit;
mod help;
mod output;
mod sync;

use std::env;
use std::process::ExitCode;

use nostrdb_net::Pubkey;
use serde_json::json;

use headway::event::{self, Container, resolve_card};
use headway::store;
use headway::teams;

use nostrdb_net::relay::sync::Result;

use crate::boards::{Roster, list_boards, load_board};
use crate::edit::{Collect, build_action, resolve_container};
use crate::output::{
    card_count, declined_message, plain_ref, print_all_boards, print_board, print_boards,
    print_cards, print_next,
};
use crate::sync::{
    flush_own_selfshares, plaintext_sync_filter, pull_giftwraps, recover_derived_board,
    sync_envelopes,
};

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
    /// Create the target board as a born team-of-one SNS channel (sealed from note
    /// #1). Optional `--title` sets its display name; when unset it defaults to the
    /// board's slug (so a non-default board is never accidentally titled "Headway").
    Seed {
        title: Option<String>,
    },
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
    /// ready frontier (see [`traversal`](headway::traversal)). A read command — it never signs.
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
    /// Mark a column terminal (a "done" column) or clear the flag. A card in a
    /// terminal column clears its dependents and drops out of the `next`
    /// frontier. Republishes the board definition.
    Terminal {
        col: String,
        terminal: bool,
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
            Command::Seed { .. }
            | Command::Migrate
            | Command::Rename { .. }
            | Command::Terminal { .. }
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

/// Turn a refused cross-board `link`/`move-board` into the CLI's error, adding
/// the part the store can't know: what to do instead.
///
/// The refusal is the whole point of the command failing loudly — the bug it
/// replaces wrote half the move and left the card on neither board
/// (headway:headway/series-high-praise) — so the message has to explain a "no"
/// that used to look like a "yes".
fn cross_board_error(err: store::CrossBoardError) -> String {
    match err {
        store::CrossBoardError::UnreadableOnTarget { .. } => format!(
            "{err}.\nBoards are sealed per-board, so a card can't yet be moved \
             between two of them. Re-create it on the target board instead."
        ),
        other => other.to_string(),
    }
}

async fn run() -> Result<()> {
    let cli = match Cli::parse(env::args().skip(1))? {
        Invocation::Run(cli) => *cli,
        Invocation::Usage => {
            print_usage();
            return Ok(());
        }
        Invocation::CommandHelp(cmd) => {
            help::print_command(cmd);
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
    // unreachable relay leaves us working offline against the cache. The filter
    // excludes unwrapped rumors so a sealed board's edits are never pushed to the
    // relay as plaintext (see `plaintext_sync_filter`); their kind-1081 envelopes
    // ride the separate `sync_envelopes` leg below instead.
    let filter = plaintext_sync_filter(&author);
    let is_addressable = event::is_addressable;
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
    // Join every shared board we hold a key for. Registering a root re-peels any
    // envelope that arrived before it, so it is safe for this to run after the sync
    // above rather than before it. The registry is held across both loads below so
    // a root is only ever handed to nostrdb once (see `teams::RootRegistry`).
    let mut root_registry = teams::RootRegistry::default();
    let roster = Roster::load(&ndb, &author, &mut root_registry);

    // Sync each joined board's kind-1081 SNS envelopes (see `sync_envelopes`).
    // Runs after `Roster::load` has registered the channel roots, so every pulled
    // envelope auto-unwraps into a queryable rumor and the sealed board folds.
    if let Some(relay) = relay.as_mut() {
        sync_envelopes(relay, &ndb, &roster).await;
    }

    // Push half of the giftwrap leg: re-publish our own boards' self-shares so a
    // fresh cache / another device can join a board sealed while offline (or by a
    // front end that never fanned its self-share). Needs the signing key — a
    // self-share is re-wrapped, not forwarded — and a reachable relay.
    if let (Some(relay), Some((secret, _))) = (relay.as_mut(), cli.secret.as_ref()) {
        flush_own_selfshares(relay, &ndb, &roster, &author, secret, cli.db.as_deref()).await;
    }

    // Recover an own board named explicitly on the command line that the roster
    // cannot see, by *deriving* its channel instead of waiting for a key-share.
    // This is the whole reason `--board <slug>` now works on a second device even
    // when the board's kind-1059 self-share never reached a relay this device
    // reads (headway:headway/rocket-group-ginger): a root is
    // `derive_board_root(secret, slug)` and is never randomly minted, so the slug
    // alone reproduces the channel. Re-derive the roster afterwards so every
    // downstream read and write routes through the recovered channel.
    let mut roster = roster;
    if cli.board_explicit
        && let Some((secret, pk)) = cli.secret.as_ref()
        && pk == &author
        && load_board(&ndb, &roster, &author, &cli.board).is_none()
        && let Some(relay) = relay.as_mut()
        && recover_derived_board(relay, &ndb, &author, secret, &cli.board).await
    {
        roster = Roster::load(&ndb, &author, &mut root_registry);
    }

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

        Command::Seed { title } => {
            let secret = secret.ok_or("seed needs --nsec to sign")?;
            if load_board(&ndb, &roster, &author, &board).is_some() {
                return Err(format!("board '{board}' already exists").into());
            }
            // Title defaults to the slug (an explicit `--title` overrides). Every
            // board is named after itself — no board is titled "Headway" unless its
            // slug is, which fixes jb55's recurring "accidental Headway board" where
            // `headway --board work seed` used to create a second board also titled
            // "Headway".
            let title = title.unwrap_or_else(|| board.clone());
            // Born team-of-one SNS: seal the board under a per-board root *derived*
            // from the account secret and slug (never randomly minted), mirroring the
            // GUI. Deriving converges cross-device — the same slug on another device
            // lands the same channel — while a different slug derives an unrelated,
            // isolated root. `create_shared_board` also self-shares the root (a
            // kind-1082 key-share) so the board joins this account's roster.
            let root = nostrdb_net::sns::derive_board_root(&secret, &board);
            let mut sink = Collect::default();
            if !store::create_shared_board(&ndb, &author, &secret, &board, &title, &root, &mut sink)
            {
                return Err(format!("failed to seal new board '{board}'").into());
            }
            let n = sink.0.len();
            nostrdb_net::relay::sync::publish(&mut relay, &sink.0).await?;
            println!(
                "seeded sealed board '{board}' ({n} events){}",
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
                None => (nostrdb_net::sns::derive_board_root(&secret, &board), false),
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
            // Each board seals with its *own* channel — see `store::BoardRef`.
            let source_channel = roster.channel(&board);
            let target_channel = roster.channel(&to_board);
            store::link_card(
                &ndb,
                store::BoardRef {
                    id: &board,
                    view: &source,
                    channel: source_channel.as_ref(),
                },
                store::BoardRef {
                    id: &to_board,
                    view: &target,
                    channel: target_channel.as_ref(),
                },
                &secret,
                card_id,
                &mut sink,
            )
            .map_err(cross_board_error)?;
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
            // Each board seals with its *own* channel — see `store::BoardRef`.
            let source_channel = roster.channel(&board);
            let target_channel = roster.channel(&to_board);
            store::move_card_between_boards(
                &ndb,
                store::BoardRef {
                    id: &board,
                    view: &source,
                    channel: source_channel.as_ref(),
                },
                store::BoardRef {
                    id: &to_board,
                    view: &target,
                    channel: target_channel.as_ref(),
                },
                &secret,
                card_id,
                &mut sink,
            )
            .map_err(cross_board_error)?;
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
            let declined = store::apply(
                &ndb,
                &board,
                &view,
                &author,
                &store::Signer::new(&secret, channel.as_ref()),
                action,
                &mut sink,
            );
            // A deliberate decline is not a resolution failure. Say which one it
            // was, rather than letting the catch-all below send the caller off to
            // hunt for a longer card id that would resolve identically forever.
            if let Some(declined) = declined {
                let msg = declined_message(&view, &declined);
                if !declined.reason.is_noop() {
                    return Err(format!("refused: {msg}").into());
                }
                // Already in the asked-for state: idempotent success, not an error.
                if as_json {
                    println!("{}", json!({ "ok": true, "events": 0, "noop": msg }));
                } else {
                    println!("ok (0 events) — {msg}");
                }
                return Ok(());
            }
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

/// What a command line asks for: a run, or one of the two help pages.
enum Invocation {
    Run(Box<Cli>),
    /// `headway` with no command, or `headway --help`: the grouped overview.
    Usage,
    /// `headway <cmd> --help` or `headway help <cmd>`: one command's own page.
    CommandHelp(&'static help::Command),
}

impl Cli {
    /// Parse args (without the program name), deciding between a run and a help
    /// page (see [`Invocation`]).
    fn parse(args: impl Iterator<Item = String>) -> Result<Invocation> {
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
        // `seed` display title; when unset the title defaults to the board slug
        // (see `Command::Seed`).
        let mut title = None;
        let mut labels: Vec<String> = Vec::new();
        let mut seq = SeqFlags::default();
        // `next` flags.
        let mut ready = false;
        let mut count: Option<usize> = None;
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
                "--title" => title = Some(value("--title")?),
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
                    labels.extend(split_labels(&value("--label")?));
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

        // `headway help [cmd]` is the same two pages spelled as a command, so
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
        // rejected here, so `parse_command` never sees one and a command added
        // without a help entry can't ship undocumented.
        let Some(spec) = help::lookup(name) else {
            return Err(format!("unknown command '{name}' (try `headway --help`)").into());
        };
        if want_help {
            return Ok(Invocation::CommandHelp(spec));
        }
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
            title,
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
        // (and the `headway` store/event layer) speaks `nostrdb_net::Pubkey`. Both are
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

        Ok(Invocation::Run(Box::new(Cli {
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
        })))
    }
}

/// Split one label argument into the labels it names. A single argument may be a
/// comma-separated list, so `-l a,b` and `label <card> a,b` each set two labels;
/// surrounding whitespace is trimmed and empty entries (`a,,b`, a trailing comma)
/// dropped. Shared by the `-l`/`--label` flag and `label`'s positionals so the two
/// spellings can't disagree — a label containing a comma is deliberately
/// unrepresentable rather than settable by only one of them.
fn split_labels(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
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
    title: Option<String>,
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
        "seed" => Command::Seed { title },
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
        // Each positional may itself be a comma-separated list, so
        // `label <card> a,b` and `label <card> a b` agree — and agree with the
        // `add -l a,b` that may have set them. `-l`/`--label` flags are honoured
        // too: they'd otherwise leave no positionals, silently *clearing* the
        // card's labels (the no-labels-clears case) instead of setting them.
        "label" => Command::Label {
            card: card()?,
            labels: rest
                .get(1..)
                .unwrap_or_default()
                .iter()
                .flat_map(|l| split_labels(l))
                .chain(labels)
                .collect(),
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
        // `terminal <col> [on|off]` — mark a column as a "done" column, or clear
        // it with `off`. Defaults to `on` when the state is omitted.
        "terminal" => Command::Terminal {
            col: arg(rest, 0, name)?,
            terminal: match rest.get(1).map(String::as_str) {
                None | Some("on" | "true" | "yes") => true,
                Some("off" | "false" | "no") => false,
                Some(other) => {
                    return Err(format!("terminal state must be on/off, got '{other}'").into());
                }
            },
        },
        "board" => Command::Board {
            id: rest.first().cloned(),
        },
        "login" => Command::Login {
            nsec: arg(rest, 0, name)?,
        },
        "logout" => Command::Logout,
        // Unreachable in practice: `Cli::parse` rejects a name the help table
        // doesn't list, so reaching here means a documented command was never
        // given a parser (see `every_documented_command_parses`).
        other => return Err(format!("command '{other}' has no parser").into()),
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

/// Print the grouped command list — `headway --help`, or a bare `headway`.
fn print_usage() {
    help::print_usage(nostrdb_net::relay::sync::DEFAULT_RELAY, store::BOARD_ID);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        match Cli::parse(args.iter().map(|s| s.to_string())).expect("parse ok") {
            Invocation::Run(cli) => *cli,
            _ => panic!("expected a command, got help"),
        }
    }

    /// The help page a command line asks for, or `None` when it asks to run.
    fn help_of(args: &[&str]) -> Option<Option<&'static str>> {
        match Cli::parse(args.iter().map(|s| s.to_string())).expect("parse ok") {
            Invocation::Run(_) => None,
            Invocation::Usage => Some(None),
            Invocation::CommandHelp(cmd) => Some(Some(cmd.name)),
        }
    }

    /// `--help` picks the overview or one command's page depending on whether a
    /// command was named, in either spelling and either order — and `--help`
    /// wins over the command's own argument checking, so `headway move --help`
    /// prints the page rather than erroring about the missing `--col`.
    #[test]
    fn help_resolves_to_a_page() {
        assert_eq!(help_of(&[]), Some(None));
        assert_eq!(help_of(&["--help"]), Some(None));
        assert_eq!(help_of(&["help"]), Some(None));
        assert_eq!(help_of(&["move", "--help"]), Some(Some("move")));
        assert_eq!(help_of(&["-h", "move"]), Some(Some("move")));
        assert_eq!(help_of(&["help", "move"]), Some(Some("move")));
        assert_eq!(help_of(&["help", "move-board"]), Some(Some("move-board")));
        // Without it, the same line runs (and here fails on its own missing flag).
        assert!(help_of(&["show"]).is_none());
    }

    /// An unrecognised command is rejected by name, whether or not help was
    /// asked for — `headway frobnicate --help` has no page to print.
    #[test]
    fn unknown_commands_are_rejected() {
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
            let rest = ["a".to_string(), "b".to_string()];
            let res = parse_command(
                cmd.name,
                &rest,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Vec::new(),
                SeqFlags::default(),
                false,
                None,
            );
            // A command that wants a flag we didn't pass errors about *that*;
            // only the fallback arm means the name has no parser at all.
            if let Err(e) = res {
                assert!(
                    !e.to_string().contains("has no parser"),
                    "`{}` is documented but has no parser",
                    cmd.name
                );
            }
        }
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

    /// The labels of a `label` command, whatever spelling produced them.
    fn labels_of(cli: Cli) -> Vec<String> {
        match cli.command {
            Command::Label { labels, .. } => labels,
            _ => panic!("expected a Label command"),
        }
    }

    /// `label` takes its labels the same ways `add -l` does: separate
    /// positionals, one comma-separated positional, or the `-l`/`--label` flag —
    /// all equivalent. The flag case used to leave `rest` empty and so *clear*
    /// the card's labels instead of setting them.
    #[test]
    fn label_command_accepts_commas_and_the_label_flag() {
        let expected = ["a".to_string(), "b".to_string()];
        assert_eq!(labels_of(parse(&["label", "deadbeef", "a", "b"])), expected);
        assert_eq!(labels_of(parse(&["label", "deadbeef", "a,b"])), expected);
        assert_eq!(
            labels_of(parse(&["label", "deadbeef", "-l", "a,b"])),
            expected
        );
        assert_eq!(
            labels_of(parse(&["label", "deadbeef", "--label", "a", "-l", "b"])),
            expected
        );
        // Whitespace trimmed, empty entries dropped.
        assert_eq!(labels_of(parse(&["label", "deadbeef", "a, ,b,"])), expected);
        // No labels at all still clears — the one case that must stay empty.
        assert!(labels_of(parse(&["label", "deadbeef"])).is_empty());
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
