//! `headway` — a CLI for reading and mutating a Headway board against a running
//! notedeck's embedded relay.
//!
//! The cache/sync/relay plumbing — keeping the CLI's own nostrdb, reconciling it
//! against the app's relay with NIP-77 negentropy, and the stored signing key —
//! lives in `nostrdb_net`'s `relay::sync` module (see [`notebook_cli`] for the
//! other consumer). This crate is just the board's command surface: parsing
//! ([`args`]), folding boards ([`boards`]) after the extra relay legs
//! ([`sync`]), resolving card/column arguments against the folded board
//! ([`edit`]), and rendering ([`output`]). This file is the entry point and the
//! per-command dispatch.

mod args;
mod boards;
mod diff;
mod edit;
mod grep;
mod help;
mod output;
mod review;
mod sync;

use std::env;
use std::process::ExitCode;

use serde_json::json;

use headway::event::{self, Container, resolve_card};
use headway::store;
use headway::teams;

use nostrdb::Ndb;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::args::{Cli, Command, Invocation};
use crate::boards::{Roster, list_boards, list_shared_with_me, load_board};
use crate::edit::{Collect, build_action, resolve_container};
use crate::output::{
    card_count, declined_message, plain_ref, print_all_boards, print_board, print_boards,
    print_cards, print_next,
};
use crate::sync::{
    flush_own_selfshares, plaintext_sync_filter, pull_giftwraps, pull_pns_keyshares,
    recover_derived_board, sync_envelopes,
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

/// Refuse a board-creating command (`seed`, `migrate`) when the signer isn't the
/// board's owner. Both mint a channel from the *signer's* secret at the *owner's*
/// coordinate, so a member running one against `--author <owner>` would seal a
/// board under a key the owner never derives — a split no fold can merge back.
fn require_owner(me: &Pubkey, owner: &Pubkey, cmd: &str) -> Result<()> {
    if me == owner {
        return Ok(());
    }
    Err(format!(
        "{cmd} acts on a board you own, but --author names {} — drop --author",
        owner.hex()
    )
    .into())
}

/// Whether `cmd` addresses a single existing board, and so should find a board
/// shared with us by its slug alone when there's no `--author` (see
/// [`shared_owner`]).
///
/// Not the listings — `headway board`, `show --all` and `grep --all` cover
/// every board, so no one slug is theirs to resolve (and an ambiguous current
/// board mustn't break them). Not `seed`/`migrate` either: they only ever act on a board *we* own,
/// and `headway --board shared seed` must create our own `shared` even when
/// someone else's is in the roster.
fn resolves_owner_by_slug(cmd: &Command, show_all: bool) -> bool {
    !matches!(
        cmd,
        Command::Board { id: None } | Command::Seed { .. } | Command::Migrate
    ) && !(show_all && matches!(cmd, Command::Show { .. } | Command::Grep { .. }))
}

/// The owner of the board `board_id` names when it isn't one of ours: the one
/// owner in the roster sharing a board under that slug with `me`.
///
/// `None` when we have our own board by that name (ours always wins) or no one
/// shared one — the caller keeps `me` as the owner and the usual "no board" path
/// takes over. Several owners is an error, since picking one would silently act
/// on a board the caller may not have meant; it lists them so `--author` can
/// choose.
///
/// "Our own" means one in this cache. A sealed board of ours that only another
/// device has seen is found later by deriving its root, which this runs before —
/// so if someone also shared a board under the same slug, that one wins here and
/// `--author <our own key>` is how to reach ours.
fn shared_owner(ndb: &Ndb, roster: &Roster, me: &Pubkey, board_id: &str) -> Result<Option<Pubkey>> {
    if load_board(ndb, roster, me, board_id).is_some() {
        return Ok(None);
    }
    let owners = roster.owners_of(me, board_id);
    match owners.as_slice() {
        [] => Ok(None),
        [owner] => Ok(Some(*owner)),
        _ => {
            let list: Vec<String> = owners
                .iter()
                .map(|pk| format!("  {}", pk.npub().unwrap_or_else(|| pk.hex())))
                .collect();
            Err(format!(
                "'{board_id}' names boards shared with you by {} owners — pass \
                 --author <owner> to pick one:\n{}",
                owners.len(),
                list.join("\n")
            )
            .into())
        }
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

    // The board *owner* whose coordinate (`30619:<owner>:<slug>`) we read and
    // write: an explicit `--author`, else the signing key's own pubkey.
    let author = match (&cli.author, &cli.secret) {
        (Some(pk), _) => *pk,
        (None, Some((_, pk))) => *pk,
        (None, None) => return Err("need --nsec to sign, or --author to read a board".into()),
    };
    // Who *we* are: the signing key's pubkey, falling back to the owner for a
    // read-only `--author` run. Distinct from `author` when a member works a
    // board someone else owns — then the gift-wraps to pull, the key-shares the
    // roster is built from, and the acting editor are all ours, while the board
    // coordinate stays the owner's. Conflating the two left a member with "no
    // board" either way (headway:headway/vacuum-priority-ordinary).
    let me = cli.secret.as_ref().map_or(author, |(_, pk)| *pk);

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
        pull_giftwraps(relay, &ndb, &me).await;
    }
    // The same key-shares for our own boards, carried over PNS: the copy that
    // reaches this device when a private relay won't serve it the gift-wrap
    // (headway:headway/pepper-rack-usual). Deriving the PNS stream needs the
    // signing key, so a read-only `--author` run skips it.
    if let (Some(relay), Some((secret, _))) = (relay.as_mut(), cli.secret.as_ref()) {
        pull_pns_keyshares(relay, &ndb, secret).await;
    }
    // Join every shared board we hold a key for. Registering a root re-peels any
    // envelope that arrived before it, so it is safe for this to run after the sync
    // above rather than before it. The registry is held across both loads below so
    // a root is only ever handed to nostrdb once (see `teams::RootRegistry`).
    let mut root_registry = teams::RootRegistry::default();
    let roster = Roster::load(&ndb, &me, &author, &mut root_registry);

    // With no `--author`, a slug we don't own ourselves may still name a board
    // someone shared with us. Only the roster knows who owns it, so this is the
    // earliest the owner can be settled; everything below reads and writes at the
    // resolved owner's coordinate.
    let (author, roster) = if cli.author.is_none() && resolves_owner_by_slug(&cli.command, cli.all)
    {
        match shared_owner(&ndb, &roster, &me, &cli.board)? {
            Some(owner) => (owner, roster.for_author(owner)),
            None => (author, roster),
        }
    } else {
        (author, roster)
    };

    // Sync each joined board's kind-1081 SNS envelopes (see `sync_envelopes`).
    // Runs after `Roster::load` has registered the channel roots, so every pulled
    // envelope auto-unwraps into a queryable rumor and the sealed board folds.
    if let Some(relay) = relay.as_mut() {
        sync_envelopes(relay, &ndb, &roster).await;
    }

    // Push half of the giftwrap leg: re-publish our own boards' self-shares so a
    // fresh cache / another device can join a board sealed while offline (or by a
    // front end that never fanned its self-share). Needs the signing key — a
    // self-share is re-wrapped, not forwarded — and a reachable relay. Scoped to
    // boards *we* own (`me`), never the `--author` we are reading: a member's
    // roster holds the owner's roots too, and re-wrapping those to the owner
    // under the member's signature is exactly what this must not do.
    if let (Some(relay), Some((secret, _))) = (relay.as_mut(), cli.secret.as_ref()) {
        flush_own_selfshares(relay, &ndb, &roster, &me, secret, cli.db.as_deref()).await;
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
        roster = Roster::load(&ndb, &me, &author, &mut root_registry);
    }

    let board = cli.board;
    let board_explicit = cli.board_explicit;
    let as_json = cli.json;
    let show_archived = cli.archived;
    let show_all = cli.all;
    let dry_run = cli.dry_run;
    let new_channel = cli.new_channel;
    let secret = cli.secret.map(|(s, _)| s);
    let comment_secret = cli.comment_secret.map(|(s, _)| s);

    match cli.command {
        // `--all` fans the render across every board in the cache, ignoring any
        // card selectors (which address a single board).
        Command::Show { .. } if show_all => {
            let mut boards = list_boards(&ndb, &roster, &author);
            // Only when listing our own: `--author <someone>` asks for their boards.
            if author == me {
                boards.extend(list_shared_with_me(&ndb, &roster, &me));
            }
            print_all_boards(&boards, &me, as_json, show_archived)
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
            require_owner(&me, &author, "seed")?;
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
            require_owner(&me, &author, "migrate")?;
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

        Command::Share { recipient } => {
            // Like `migrate`, never lean on the persisted current board: a
            // key-share can't be revoked, so sharing the wrong board leaks it for
            // good.
            if !board_explicit {
                return Err(
                    "share never uses the persisted current board — pass --board <id>, \
                     so you can't accidentally share the wrong board"
                        .into(),
                );
            }
            let secret = secret.ok_or("share needs --nsec to sign")?;
            // Owner-only, and checked before the roster lookup below: a member's
            // roster holds the owner's root too, so without this a member could
            // hand the board on to anyone.
            if me != author {
                return Err(format!(
                    "only the board owner can share it — '{board}' belongs to {}",
                    author.hex()
                )
                .into());
            }
            if recipient == me {
                return Err("that's your own key — you already hold this board's channel".into());
            }
            if load_board(&ndb, &roster, &author, &board).is_none() {
                return Err(format!("no board '{board}' to share").into());
            }
            // The root of the board's *primary* channel, from the roster — never
            // re-derived from the slug. A migrated or epoch-bumped board seals into
            // a channel whose root isn't `derive_board_root(secret, slug)`, and a
            // member handed the derived one would join a channel nobody writes to.
            let root = roster.team_root(&board).ok_or_else(|| {
                format!("'{board}' is not sealed — `headway migrate --board {board}` it first")
            })?;
            // Nothing re-sends a key-share later: the gift-wrap leg is pull-only and
            // the self-share flush re-wraps only our own boards to ourselves. So a
            // share made offline would be ingested here and never reach the member —
            // refuse instead of printing a success that isn't one.
            let Some(live) = relay.as_mut() else {
                return Err(format!(
                    "can't reach {} — share needs a live relay, since a key-share is \
                     never re-sent by a later run",
                    cli.relay
                )
                .into());
            };
            let mut sink = Collect::default();
            let addr = event::board_address(&author, &board);
            if !store::share_board(&ndb, &secret, &recipient, &addr, &root, &mut sink) {
                return Err(format!("failed to wrap the key-share for {}", recipient.hex()).into());
            }
            live.publish(&sink.0).await?;
            let team_pubkey = nostrdb_net::sns::derive_sns_keys(&root)
                .map(|k| k.team_keypair.pubkey.hex())
                .unwrap_or_default();
            if as_json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "board": board,
                        "recipient": recipient.hex(),
                        "team_pubkey": team_pubkey,
                    })
                );
            } else {
                println!(
                    "shared board '{board}' with {}",
                    recipient.npub().unwrap_or_else(|| recipient.hex())
                );
                println!("  team pk {}", nostrdb_net::relay::sync::dim(&team_pubkey));
            }
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
            None => {
                // Boards shared with us only alongside our own: `--author
                // <someone>` asks for their boards, not ours.
                let shared = if author == me {
                    list_shared_with_me(&ndb, &roster, &me)
                } else {
                    Vec::new()
                };
                print_boards(&list_boards(&ndb, &roster, &author), &shared, &board)
            }
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

        // Read command: search card text. One fold per board searched (just
        // this one, or every readable board under `--all`) and one pass over
        // its cards. Never signs.
        Command::Grep { pattern, container } => {
            let boards = if show_all {
                if container.is_some() {
                    return Err(
                        "--in searches one card's subtree on its own board — drop --all".into(),
                    );
                }
                let mut boards = list_boards(&ndb, &roster, &author);
                // Only when searching our own: `--author <someone>` asks for theirs.
                if author == me {
                    boards.extend(list_shared_with_me(&ndb, &roster, &me));
                }
                boards
            } else {
                vec![
                    load_board(&ndb, &roster, &author, &board)
                        .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?,
                ]
            };
            // `--in <board-slug>` is the whole board, the same as no `--in`.
            let subtree = match (container.as_deref(), boards.first()) {
                (Some(sel), Some(view)) => match resolve_container(view, sel)? {
                    Container::Card(id) => Some(id),
                    Container::BoardRoot(_) => None,
                },
                _ => None,
            };
            let out = grep::GrepOutput {
                json: as_json,
                color: cli.color,
                pager: cli.pager,
            };
            grep::cmd_grep(&boards, subtree, &pattern, show_archived, out)?;
        }

        // Read command: resolve the card's review record to a commit (fetching
        // it if this host lacks it) and print it. The bare cache lives beside
        // the nostrdb cache, so a `--db` run keeps its git cache there too.
        Command::Diff { card, record } => {
            let view = load_board(&ndb, &roster, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            let cache_root = match cli.db.as_deref() {
                Some(db) => std::path::Path::new(db).join("git"),
                None => nostrdb_net::relay::sync::config_path(APP, "git")?,
            };
            diff::print_diff(&view, &card, record.as_deref(), &cache_root)?;
        }

        edit => {
            let secret = secret.ok_or("this command needs --nsec to sign")?;
            let view = load_board(&ndb, &roster, &author, &board)
                .ok_or_else(|| format!("no board '{board}' — run `headway seed`"))?;
            // `review` echoes what it recorded (below), so keep a copy before
            // `build_action` consumes the command.
            let recorded = match &edit {
                Command::Review { review, .. } => Some(review.clone()),
                _ => None,
            };
            let action = build_action(&view, edit)?;

            let mut sink = Collect::default();
            let channel = roster.channel(&board);
            let signing_key =
                signing_key(&action, &secret, comment_secret.as_ref(), &channel, &board)?;
            let store::ApplyOutcome { declined, created } = store::apply_outcome(
                &ndb,
                &board,
                &view,
                // The acting editor, not the board owner: `apply_outcome` anchors
                // the edit at the owner's coordinate itself (from `view.author`),
                // and reads our own replaceable sets (blockers, related) by `me`.
                &me,
                &store::Signer::new(signing_key, channel.as_ref()),
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
            // have to re-`show` to recover it. Take the id from `apply_outcome`
            // rather than re-folding: on a sealed board the new card's envelope
            // is still being unwrapped, so a re-fold usually misses it.
            let new_card = created.map(|id| (id.hex(), plain_ref(&view, &id)));

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
                if let Some(review) = &recorded {
                    review::print_recorded(review);
                }
            }
        }
    }

    Ok(())
}

/// Print the grouped command list — `headway --help`, or a bare `headway`.
/// The key that signs `action`: the comment key for a comment (on the card or
/// on a review record's commit) when one is set, the run's signing key for
/// everything else.
///
/// A comment signed by another key only shows on a sealed board. The shared fold
/// takes any rumor sealed into the board's channel, whoever signed it, so the
/// comment folds and is attributed to the comment key. A plaintext board folds
/// only its owner's own events ([`event::fold_board`]), so a comment signed by
/// anyone else would be published and never seen — refused here instead.
fn signing_key<'a>(
    action: &store::BoardAction,
    secret: &'a [u8; 32],
    comment_secret: Option<&'a [u8; 32]>,
    channel: &Option<store::SnsChannel>,
    board: &str,
) -> Result<&'a [u8; 32]> {
    let is_comment = matches!(
        action,
        store::BoardAction::AddComment { .. } | store::BoardAction::AddReviewComments { .. }
    );
    let (true, Some(comment_secret)) = (is_comment, comment_secret) else {
        return Ok(secret);
    };
    if channel.is_none() {
        return Err(format!(
            "'{board}' is a plaintext board, which shows only its owner's own events, so a \
             comment signed with --comment-nsec / $HEADWAY_COMMENT_NSEC(_FILE) would never appear. \
             Unset it to comment as yourself, or seal the board first (`headway migrate`)"
        )
        .into());
    }
    Ok(comment_secret)
}

fn print_usage() {
    help::print_usage(nostrdb_net::relay::sync::DEFAULT_RELAY, store::BOARD_ID);
}
