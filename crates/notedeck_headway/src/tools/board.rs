//! The board-level tools: listing the boards an account can see, and the
//! commands that create, seal, share, delete or restore one, or move a card
//! between two — `nd headway board`, `seed`, `migrate`, `share`,
//! `delete-board`, `restore-board`, `link` and `move-board`.
//!
//! Every board here is the selected account's own unless the roster says
//! otherwise: the app has no `--author`, so a board shared with the account
//! is one it can edit (`link`, `move-board`) but never seal, share or delete,
//! which only its owner may. [`own_board`] is that check.

use headway::boards::{self, Identity, Roster};
use headway::event::{self, BoardView, resolve_card};
use headway::{render, store};
use nostrdb_net::Pubkey;
use notedeck::{AppTool, ToolArg, ToolArgType, ToolContext, ToolSpec};
use serde::{Deserialize, Serialize, Serializer};

use super::{
    BOARD_ENV, ResolvedTarget, Route, account_secret, card_arg, card_board_arg, default_board_arg,
    publisher, resolve_target,
};

/// A NIP-59 gift-wrap: what a key-share travels as.
const KIND_GIFT_WRAP: u32 = 1059;

/// What a board command answers: `{ok, events, board}` as `nd headway
/// delete-board --json` prints it, or `noop` when the board was already as
/// asked, plus the line the CLI prints for it.
#[derive(Serialize, Debug)]
pub(super) struct BoardAck {
    ok: bool,
    /// How many events the command signed and published.
    events: usize,
    board: String,
    /// Why nothing was written: the board was already in the asked-for state.
    #[serde(skip_serializing_if = "Option::is_none")]
    noop: Option<String>,
    /// The line `nd` prints.
    #[serde(skip)]
    line: String,
}

impl BoardAck {
    /// A command that wrote `events` events to `board`, said as `line`.
    fn wrote(board: &str, events: usize, line: String) -> Self {
        Self {
            ok: true,
            events,
            board: board.to_owned(),
            noop: None,
            line,
        }
    }

    /// The line, as `nd` prints it.
    fn render(&self) -> String {
        format!("{}\n", self.line)
    }
}

/// The selected account's own board `board_id`, with the roster that folded
/// it — for the commands only a board's owner may run (`verb` names the one
/// asking). A board someone shared with us under that slug is refused by
/// name, since its channel would seal (or its share would hand on) a board
/// that isn't ours; deleted boards are returned, so a restore can find one.
fn own_board(cx: &ToolContext, board_id: &str, verb: &str) -> Result<OwnBoard, String> {
    let me = *cx.accounts.selected_account_pubkey();
    let roster = Roster::read(cx.ndb, Identity::own(me));
    if let Some(view) = boards::load_board(cx.ndb, &roster, board_id) {
        return Ok(OwnBoard { roster, view });
    }
    match roster.owners_of(board_id).first() {
        Some(owner) => Err(format!(
            "only the board owner can {verb} it — '{board_id}' belongs to {}",
            owner.npub().unwrap_or_else(|| owner.hex())
        )),
        None => Err(format!("no board '{board_id}'")),
    }
}

/// What [`own_board`] found.
struct OwnBoard {
    roster: Roster,
    view: BoardView,
}

/// `headway_list_boards`: the boards the account can see, as the app's
/// switcher does: its own, those shared with it, and its deleted ones.
pub(super) struct ListBoards;

/// Arguments for [`ListBoards`].
#[derive(Deserialize)]
pub(super) struct ListBoardsArgs {
    /// `nd headway board <id>` switched a persisted current board, which is
    /// retired; given, it is refused with what to use instead.
    #[serde(default)]
    id: Option<String>,
    /// The caller's board, marked in the listing: `$HEADWAY_BOARD`.
    #[serde(default)]
    default_board: Option<String>,
}

/// What [`ListBoards`] found: the account's live boards, the boards shared
/// with it, its deleted boards, and the board it marks.
pub(super) struct BoardList {
    own: Vec<BoardView>,
    shared: Vec<BoardView>,
    deleted: Vec<BoardView>,
    current: String,
}

/// One board in [`BoardList`]'s JSON.
#[derive(Serialize)]
struct BoardSummary<'a> {
    /// The board's slug, as other tools address it.
    id: &'a str,
    title: &'a str,
    /// Live (non-archived) cards across all columns.
    card_count: usize,
    /// The owner's hex key, on a board shared with the account.
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    /// A deleted board: listed so it can be restored, never edited.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    deleted: bool,
}

impl<'a> BoardSummary<'a> {
    fn of(view: &'a BoardView) -> Self {
        Self {
            id: &view.id,
            title: &view.title,
            card_count: render::card_count(view),
            owner: None,
            deleted: view.deleted,
        }
    }
}

impl Serialize for BoardList {
    /// One array: own boards, then shared (each with its `owner`), then
    /// deleted (each `deleted: true`).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let shared = self.shared.iter().map(|view| BoardSummary {
            owner: Some(hex::encode(view.author)),
            ..BoardSummary::of(view)
        });
        serializer.collect_seq(
            self.own
                .iter()
                .map(BoardSummary::of)
                .chain(shared)
                .chain(self.deleted.iter().map(BoardSummary::of)),
        )
    }
}

impl AppTool for ListBoards {
    type Args = ListBoardsArgs;
    type Output = BoardList;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_list_boards",
            "List the headway boards the current account can see: its own, then the ones shared with it (each with its `owner`), then its deleted ones (`deleted: true`, restorable with `headway_restore_board`). Each has its id (slug), title and live card count.",
            vec![
                ToolArg::new(
                    "id",
                    ToolArgType::String,
                    "Retired: switching a persisted current board. Name the board on each call instead.",
                )
                .positional(),
                ToolArg::new(
                    "default_board",
                    ToolArgType::String,
                    "The caller's board, marked in the listing. Defaults to the primary board.",
                )
                .env(BOARD_ENV),
            ],
        )
        .titled("List boards")
        .read_only()
        .command(&["headway", "board"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        if let Some(id) = args.id {
            return Err(format!(
                "there is no current board to switch to any more — name the board on \
                 each command with --board {id}, or `export {BOARD_ENV}={id}` for a shell"
            ));
        }
        let me = *cx.accounts.selected_account_pubkey();
        let roster = Roster::read(cx.ndb, Identity::own(me));
        let (own, deleted) = boards::list_all_boards(cx.ndb, &roster)
            .into_iter()
            .partition(|view| !view.deleted);
        Ok(BoardList {
            own,
            shared: boards::list_shared_with_me(cx.ndb, &roster),
            deleted,
            current: args
                .default_board
                .unwrap_or_else(|| store::BOARD_ID.to_owned()),
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!(
            "{}{}",
            render::boards(&out.own, &out.shared, &out.current, true),
            render::deleted_boards(&out.deleted, true)
        ))
    }
}

/// `headway_seed_board`: create a board, born sealed — `nd headway seed`.
pub(super) struct SeedBoard;

/// Arguments for [`SeedBoard`].
#[derive(Deserialize)]
pub(super) struct SeedBoardArgs {
    #[serde(default)]
    board: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

impl AppTool for SeedBoard {
    type Args = SeedBoardArgs;
    type Output = BoardAck;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_seed_board",
            "Create a headway board with the default columns, sealed from its first note as a team-of-one channel, so it can later be shared without re-sealing anything. The board is `board`, or with none the slug derived from `title` (\"Tune Assistant\" seeds `tune-assistant`); the title defaults to the slug.",
            vec![
                ToolArg::new(
                    "board",
                    ToolArgType::String,
                    "The new board's id (slug). Never the caller's default board: with neither this nor `title` the call is refused.",
                ),
                ToolArg::new("title", ToolArgType::String, "The board's display title.").short('t'),
            ],
        )
        .titled("Seed board")
        .command(&["headway", "seed"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = match (args.board, args.title.as_deref()) {
            (Some(board), _) => board,
            (None, Some(title)) => store::board_slug(title, |_| false),
            (None, None) => {
                return Err(
                    "seed never uses the caller's default board — pass --board <slug>, \
                     or --title <title> to derive the slug from the title"
                        .to_owned(),
                );
            }
        };
        let secret = account_secret(cx)?;
        let me = *cx.accounts.selected_account_pubkey();
        let roster = Roster::read(cx.ndb, Identity::own(me));
        match boards::load_board(cx.ndb, &roster, &board) {
            Some(view) if view.deleted => {
                return Err(format!(
                    "board '{board}' was deleted — bring it back with `nd headway restore-board {board}`"
                ));
            }
            Some(_) => return Err(format!("board '{board}' already exists")),
            None => {}
        }
        // Titled by its slug unless told otherwise, so no board is called
        // "Headway" by accident.
        let title = args.title.unwrap_or_else(|| board.clone());
        // Derived, never minted: the same slug on another device lands the
        // same channel.
        let root = nostrdb_net::sns::derive_board_root(&secret, &board);
        let ndb = cx.ndb;
        let mut out = publisher(cx)?;
        if !store::create_shared_board(ndb, &me, &secret, &board, &title, &root, &mut out) {
            return Err(format!("failed to seal new board '{board}'"));
        }
        let n = out.sent;
        Ok(BoardAck::wrote(
            &board,
            n,
            format!("seeded sealed board '{board}' ({n} events)"),
        ))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_delete_board` and `headway_restore_board`: tombstone an own
/// board, or lift the tombstone — `nd headway delete-board` / `restore-board`.
pub(super) enum Tombstone {
    Delete,
    Restore,
}

/// [`Tombstone`] as a tool.
pub(super) struct TombstoneTool(pub(super) Tombstone);

/// Arguments for [`TombstoneTool`].
#[derive(Deserialize)]
pub(super) struct TombstoneArgs {
    board: String,
}

impl AppTool for TombstoneTool {
    type Args = TombstoneArgs;
    type Output = BoardAck;

    fn spec(&self) -> ToolSpec {
        let (name, description, title, command, board): (_, _, _, &'static [&'static str], _) =
            match self.0 {
                Tombstone::Delete => (
                    "headway_delete_board",
                    "Delete a headway board you own. Reversible: the board's definition is republished marked deleted, so it drops out of the board list and refuses edits, but its cards stay and `headway_restore_board` brings it back as it was.",
                    "Delete board",
                    &["headway", "delete-board"],
                    "The board to delete. Named outright, never defaulted, so a stale selection can't delete the wrong one.",
                ),
                Tombstone::Restore => (
                    "headway_restore_board",
                    "Bring back a headway board you deleted. `headway_list_boards` lists deleted boards last.",
                    "Restore board",
                    &["headway", "restore-board"],
                    "The deleted board to restore.",
                ),
            };
        ToolSpec::new(
            name,
            description,
            vec![
                ToolArg::new("board", ToolArgType::String, board)
                    .required(true)
                    .positional(),
            ],
        )
        .titled(title)
        .command(command)
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let delete = matches!(self.0, Tombstone::Delete);
        let (verb, already, action) = if delete {
            ("delete", "already deleted", store::BoardAction::DeleteBoard)
        } else {
            ("restore", "not deleted", store::BoardAction::RestoreBoard)
        };
        let board = args.board;
        let OwnBoard { roster, view } = own_board(cx, &board, verb)?;
        if view.deleted == delete {
            return Ok(BoardAck {
                ok: true,
                events: 0,
                board: board.clone(),
                noop: Some(already.to_owned()),
                line: format!("ok (0 events) — board '{board}' is {already}"),
            });
        }
        let secret = account_secret(cx)?;
        let channel = roster.channel(&board);
        let ndb = cx.ndb;
        let mut out = publisher(cx)?;
        store::apply(
            ndb,
            &board,
            &view,
            &roster.identity().me,
            &store::Signer::new(&secret, channel.as_ref()),
            action,
            &mut out,
        );
        let n = out.sent;
        if n == 0 {
            return Err(format!("{verb}-board produced no events"));
        }
        let cards = render::card_count(&view);
        let line = if delete {
            format!(
                "deleted board '{board}' ({cards} cards kept) — undo with `nd headway restore-board {board}`"
            )
        } else {
            format!("restored board '{board}' ({cards} cards)")
        };
        Ok(BoardAck::wrote(&board, n, line))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// `headway_link_card` and `headway_move_card_to_board`: put a card on a
/// second board, keeping it on the first or taking it off — `nd headway link` /
/// `move-board`.
pub(super) enum CrossBoard {
    Link,
    Move,
}

/// [`CrossBoard`] as a tool.
pub(super) struct CrossBoardTool(pub(super) CrossBoard);

/// Arguments for [`CrossBoardTool`].
#[derive(Deserialize)]
pub(super) struct CrossBoardArgs {
    card: String,
    /// The board to place it on.
    to: String,
    #[serde(flatten)]
    route: Route,
}

impl AppTool for CrossBoardTool {
    type Args = CrossBoardArgs;
    type Output = BoardAck;

    fn spec(&self) -> ToolSpec {
        let (name, description, title, command, to): (_, _, _, &'static [&'static str], _) =
            match self.0 {
                CrossBoard::Link => (
                    "headway_link_card",
                    "Also place a headway card on another board, keeping it where it is. Membership follows placement, so it is the same card on both boards, not a copy.",
                    "Link card to board",
                    &["headway", "link"],
                    "The board to place it on too.",
                ),
                CrossBoard::Move => (
                    "headway_move_card_to_board",
                    "Move a headway card to another board: place it there, then take it off the board it was on. Same card, same history.",
                    "Move card to board",
                    &["headway", "move-board"],
                    "The destination board.",
                ),
            };
        ToolSpec::new(
            name,
            description,
            vec![
                card_arg(),
                ToolArg::new("to", ToolArgType::String, to).required(true),
                card_board_arg(),
                default_board_arg(),
            ],
        )
        .titled(title)
        .command(command)
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.route.board([args.card.as_str()])?;
        let to = args.to;
        let me = *cx.accounts.selected_account_pubkey();
        let teams = headway::teams::teams_from_ndb(cx.ndb, &me);
        let source = resolve_target(cx.ndb, &me, &board, &teams)?;
        let target = resolve_target(cx.ndb, &me, &to, &teams)
            .map_err(|e| format!("{e} — the target board must exist first"))?;
        for (id, ResolvedTarget { view, .. }) in [(&board, &source), (&to, &target)] {
            if view.deleted {
                return Err(render::deleted_board_error(id));
            }
        }
        let card = resolve_card(&source.view, &args.card)?;
        let secret = account_secret(cx)?;
        let ndb = cx.ndb;
        let mut out = publisher(cx)?;
        let source = store::BoardRef {
            id: &board,
            view: &source.view,
            channel: source.channel.as_ref(),
        };
        let target = store::BoardRef {
            id: &to,
            view: &target.view,
            channel: target.channel.as_ref(),
        };
        let (moved, verb) = match self.0 {
            CrossBoard::Link => (
                store::link_card(ndb, source, target, &secret, card, &mut out),
                "linked",
            ),
            CrossBoard::Move => (
                store::move_card_between_boards(ndb, source, target, &secret, card, &mut out),
                "moved",
            ),
        };
        moved.map_err(cross_board_error)?;
        let n = out.sent;
        Ok(BoardAck::wrote(
            &to,
            n,
            format!("{verb} to '{to}' ({n} events)"),
        ))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(out.render())
    }
}

/// A refused cross-board move as the CLI says it, with what to do instead.
/// Both halves are refused together, so the card stays where it was.
fn cross_board_error(err: store::CrossBoardError) -> String {
    match err {
        store::CrossBoardError::UnreadableOnTarget { .. } => format!(
            "{err}.\nBoards are sealed per-board, so a card can't yet be moved \
             between two of them. Re-create it on the target board instead."
        ),
        other => other.to_string(),
    }
}

/// `headway_migrate_board`: seal a plaintext board into a channel —
/// `nd headway migrate`.
pub(super) struct MigrateBoard;

/// Arguments for [`MigrateBoard`].
#[derive(Deserialize)]
pub(super) struct MigrateBoardArgs {
    board: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    new_channel: bool,
}

/// What [`MigrateBoard`] did, or with `dry_run` would do.
#[derive(Serialize, Debug)]
pub(super) struct Migrated {
    ok: bool,
    board: String,
    dry_run: bool,
    /// `existing` when the board already had a channel, `new` when this
    /// sealed it into one for the first time.
    channel: &'static str,
    /// Every note on the board.
    notes: usize,
    /// Of those, the ones still plaintext: what a run re-seals.
    unsealed: usize,
    /// How many notes the run re-sealed; 0 on a dry run.
    resealed: usize,
    /// How many events it signed and published.
    events: usize,
}

impl AppTool for MigrateBoard {
    type Args = MigrateBoardArgs;
    type Output = Migrated;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_migrate_board",
            "Seal a plaintext headway board you own into a channel, so it can be shared: its notes are re-sealed in place, with no data loss. Sealed events can't be unpublished, so the board is named outright and `dry_run` reports what would change first. A board that already has a channel reseals into it; one with none needs `new_channel` to say so.",
            vec![
                ToolArg::new(
                    "board",
                    ToolArgType::String,
                    "The board to seal. Never defaulted.",
                )
                .required(true),
                ToolArg::new(
                    "dry_run",
                    ToolArgType::Boolean,
                    "Report what would be re-sealed and publish nothing.",
                ),
                ToolArg::new(
                    "new_channel",
                    ToolArgType::Boolean,
                    "Seal a board with no channel here into a new one. Refused without it, since a board whose channel just hasn't synced would be split in two.",
                ),
            ],
        )
        .titled("Migrate board")
        .command(&["headway", "migrate"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board = args.board;
        let secret = account_secret(cx)?;
        let OwnBoard { roster, .. } = own_board(cx, &board, "migrate")?;
        let me = roster.identity().me;
        let preview = store::preview_migration(cx.ndb, &me, &board);
        // A board with a channel must reuse it: its sealed notes can't move
        // to a second one. Creating one is a one-way door, and "no channel"
        // looks the same as "its channel hasn't synced", so it is never
        // inferred.
        let (root, channel) = match roster.team_root(&board) {
            Some(root) => (root, "existing"),
            None if !args.new_channel => {
                return Err(format!(
                    "board '{board}' has no channel here, so migrate would create one. If it \
                     is already shared, that would split it in two — let notedeck sync and \
                     re-check `nd headway board`. If it really is a plaintext board, pass \
                     --new-channel to seal it into a new channel."
                ));
            }
            None => (nostrdb_net::sns::derive_board_root(&secret, &board), "new"),
        };
        let mut migrated = Migrated {
            ok: true,
            board: board.clone(),
            dry_run: args.dry_run,
            channel,
            notes: preview.total,
            unsealed: preview.unsealed,
            resealed: 0,
            events: 0,
        };
        if args.dry_run {
            return Ok(migrated);
        }
        let ndb = cx.ndb;
        let mut out = publisher(cx)?;
        migrated.resealed = store::migrate_board_to_sns(ndb, &me, &secret, &board, &root, &mut out);
        migrated.events = out.sent;
        Ok(migrated)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let Migrated {
            board,
            channel,
            notes,
            unsealed,
            resealed,
            ..
        } = out;
        let channel = if *channel == "new" { "NEW" } else { channel };
        Some(if out.dry_run {
            format!(
                "dry run: would re-seal {unsealed} of {notes} notes on '{board}' into its {channel} channel\n\
                 (the other {} are already sealed — re-wrapping those is a no-op)\n\
                 re-run without --dry-run to publish; sealed events cannot be unpublished\n",
                notes - unsealed
            )
        } else {
            format!(
                "migrated board '{board}' to SNS ({resealed} notes re-sealed into its {channel} channel)\n"
            )
        })
    }
}

/// `headway_share_board`: make someone a member of a sealed board —
/// `nd headway share`.
pub(super) struct ShareBoard;

/// Arguments for [`ShareBoard`].
#[derive(Deserialize)]
pub(super) struct ShareBoardArgs {
    /// Their npub or hex key.
    recipient: String,
    board: String,
}

/// What [`ShareBoard`] answers, as `nd headway share --json` prints it.
#[derive(Serialize, Debug)]
pub(super) struct Shared {
    ok: bool,
    board: String,
    /// The member's hex key.
    recipient: String,
    /// The channel's team key, which the member's envelopes are signed by.
    team_pubkey: String,
    /// The member as the CLI shows them: npub, else hex.
    #[serde(skip)]
    shown: String,
}

impl AppTool for ShareBoard {
    type Args = ShareBoardArgs;
    type Output = Shared;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_share_board",
            "Make someone a member of a sealed headway board you own: its channel key is gift-wrapped to them as a key-share, and once it reaches them they read the board's whole history and can edit it. A key-share can't be revoked, so the board is named outright; it is sent once, through the account's private relays, so the call is refused when there are none.",
            vec![
                ToolArg::new(
                    "recipient",
                    ToolArgType::String,
                    "The new member's npub or hex key.",
                )
                .required(true)
                .positional(),
                ToolArg::new(
                    "board",
                    ToolArgType::String,
                    "The board to share. Never defaulted.",
                )
                .required(true),
            ],
        )
        .titled("Share board")
        .command(&["headway", "share"])
        .filed_under(crate::TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let recipient = Pubkey::parse(&args.recipient)
            .map_err(|e| format!("share: not an npub or hex pubkey: {e}"))?;
        let shared = share_key(cx, &args.board, &recipient)?;
        let secret = account_secret(cx)?;
        // Nothing re-sends a key-share later, so one with nowhere to go
        // would be lost while the call said it worked.
        let targets = cx.accounts.selected_account_private_relay_set().targets();
        if targets.for_kind(KIND_GIFT_WRAP).is_empty() {
            return Err(
                "share needs a private relay to send the key-share through — it is sent \
                 once and never re-sent, so add one first"
                    .to_owned(),
            );
        }
        let ndb = cx.ndb;
        let mut out = publisher(cx)?;
        if !store::share_board(
            ndb,
            &secret,
            &recipient,
            &shared.board_addr,
            &shared.root,
            &mut out,
        ) {
            return Err(format!(
                "failed to wrap the key-share for {}",
                recipient.hex()
            ));
        }
        Ok(Shared {
            ok: true,
            board: args.board,
            recipient: recipient.hex(),
            team_pubkey: shared.team_pubkey,
            shown: recipient.npub().unwrap_or_else(|| recipient.hex()),
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!(
            "shared board '{}' with {}\n  team pk {}\n",
            out.board,
            out.shown,
            render::dim(true, &out.team_pubkey)
        ))
    }
}

/// What sharing `board` hands a member: the coordinate the key-share names
/// and the root of the board's channel.
pub(super) struct ShareKey {
    pub(super) board_addr: String,
    pub(super) root: [u8; 32],
    team_pubkey: String,
}

/// The key-share [`ShareBoard`] sends `recipient` for `board`, or why it
/// mustn't: only an own, sealed board, and never to ourselves.
pub(super) fn share_key(
    cx: &ToolContext,
    board: &str,
    recipient: &Pubkey,
) -> Result<ShareKey, String> {
    let me = *cx.accounts.selected_account_pubkey();
    if *recipient == me {
        return Err("that's your own key — you already hold this board's channel".to_owned());
    }
    let OwnBoard { roster, .. } = own_board(cx, board, "share")?;
    // The primary channel's root from the roster, never re-derived from the
    // slug: a migrated board's channel isn't the derived one, and a member
    // handed that would join a channel nobody writes to.
    let root = roster.team_root(board).ok_or_else(|| {
        format!("'{board}' is not sealed — `nd headway migrate --board {board}` it first")
    })?;
    let team_pubkey = nostrdb_net::sns::derive_sns_keys(&root)
        .map(|keys| keys.team_keypair.pubkey.hex())
        .unwrap_or_default();
    Ok(ShareKey {
        board_addr: event::board_address(&me, board),
        root,
        team_pubkey,
    })
}
