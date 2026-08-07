//! Agent tools exposing this app's headway boards to AI backends over the
//! shared per-app tool interface ([`notedeck::AppTool`]).
//!
//! Each tool is a read-only nostrdb query that reuses headway core's board
//! folding ([`event::fold_board`]) and JSON serializers ([`event::board_json`] /
//! [`event::card_json`]), so a backend (Dave, `notedeck --mcp`) sees exactly the
//! same curated schema the CLI emits with `--json`. Boards are folded for the
//! currently-selected account — the pubkey that authors this user's boards.

use enostr::RelayId;
use headway::event::{self, BoardView, CardView, Priority, resolve_card};
use headway::store::{self, BoardAction};
use nostrdb::Transaction;
use notedeck::{
    AppTool, ExplicitPublishApi, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolSpec,
    fan_out_event_frame,
};
use serde::{Deserialize, Serialize};

/// Every agent tool the Headway app contributes, ready to register.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        // Read.
        RegisteredTool::new(ListBoards),
        RegisteredTool::new(ShowBoard),
        RegisteredTool::new(ShowCard),
        // Write.
        RegisteredTool::new(AddCard),
        RegisteredTool::new(MoveCard),
        RegisteredTool::new(SetTitle),
        RegisteredTool::new(SetDescription),
        RegisteredTool::new(SetLabels),
        RegisteredTool::new(SetPriority),
        RegisteredTool::new(SetParent),
        RegisteredTool::new(CommentCard),
        RegisteredTool::new(ArchiveCard),
        RegisteredTool::new(RestoreCard),
        RegisteredTool::new(DeleteCard),
        RegisteredTool::new(RenameBoard),
    ]
}

/// Every card on `view`: the live column cards followed by the archived ones.
fn all_cards(view: &BoardView) -> impl Iterator<Item = &CardView> {
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .chain(view.archived.iter().map(|a| &a.card))
}

/// Fold the selected account's boards, or `Err` when the db can't be opened.
/// A `None` reducer (no boards yet) is left for the caller to treat as empty.
fn fold(cx: &ToolContext) -> Result<Option<event::BoardReducer>, String> {
    let author = *cx.accounts.selected_account_pubkey();
    let txn = Transaction::new(cx.ndb).map_err(|e| format!("failed to open db: {e}"))?;
    Ok(event::fold_board(cx.ndb, &txn, &author))
}

/// `headway_list_boards`: the boards for the current account.
struct ListBoards;

/// One entry in [`ListBoards`]' output.
#[derive(Serialize)]
struct BoardSummary {
    /// The board's stable id (slug), as used to address it in other tools.
    id: String,
    title: String,
    /// The number of live (non-archived) cards across all columns.
    card_count: usize,
}

/// `headway_list_boards` takes no arguments.
#[derive(Deserialize)]
struct ListBoardsArgs {}

impl AppTool for ListBoards {
    type Args = ListBoardsArgs;
    type Output = Vec<BoardSummary>;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_list_boards",
            "List the headway boards for the current account: each board's id (slug), title, and live card count.",
            vec![],
        )
    }

    fn call(&self, cx: &mut ToolContext, _args: Self::Args) -> Result<Self::Output, String> {
        let Some(reducer) = fold(cx)? else {
            return Ok(Vec::new());
        };
        Ok(reducer
            .finalize()
            .into_iter()
            .map(|board| BoardSummary {
                card_count: board.columns.iter().map(|c| c.cards.len()).sum(),
                id: board.id,
                title: board.title,
            })
            .collect())
    }
}

/// `headway_show_board`: a board's columns and cards.
struct ShowBoard;

/// Arguments for [`ShowBoard`].
#[derive(Deserialize)]
struct ShowBoardArgs {
    /// The board id (slug) to show; the primary board when omitted.
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for ShowBoard {
    type Args = ShowBoardArgs;
    type Output = serde_json::Value;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_show_board",
            "Show a headway board's columns and their cards as JSON. Omit `board` to show the primary board.",
            vec![ToolArg::new(
                "board",
                ToolArgType::String,
                "The board id (slug) to show. Defaults to the primary board when omitted.",
            )],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board_id = args.board.as_deref().unwrap_or(store::BOARD_ID);
        let author = *cx.accounts.selected_account_pubkey();
        let Some(reducer) = fold(cx)? else {
            return Err(format!("no board '{board_id}'"));
        };
        match event::pick_board(&reducer, &author, board_id) {
            Some(view) => Ok(event::board_json(&view)),
            None => Err(format!("no board '{board_id}'")),
        }
    }
}

/// `headway_show_card`: one card in full detail.
struct ShowCard;

/// Arguments for [`ShowCard`].
#[derive(Deserialize)]
struct ShowCardArgs {
    /// The card ref: a word-id (e.g. `swift-blue-fox`) or a hex id prefix.
    card: String,
    /// The board to resolve the card on; the primary board when omitted.
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for ShowCard {
    type Args = ShowCardArgs;
    type Output = serde_json::Value;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_show_card",
            "Show a single headway card in full — description, labels, priority, comments, activity, and subissues — as JSON. Address the card by its word-id (e.g. `swift-blue-fox`) or a hex id prefix.",
            vec![
                ToolArg::new(
                    "card",
                    ToolArgType::String,
                    "The card ref: a word-id or a hex id prefix.",
                )
                .required(true),
                ToolArg::new(
                    "board",
                    ToolArgType::String,
                    "The board to resolve the card on. Defaults to the primary board.",
                ),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let board_id = args.board.as_deref().unwrap_or(store::BOARD_ID);
        let author = *cx.accounts.selected_account_pubkey();
        let Some(reducer) = fold(cx)? else {
            return Err(format!("no board '{board_id}'"));
        };
        let Some(view) = event::pick_board(&reducer, &author, board_id) else {
            return Err(format!("no board '{board_id}'"));
        };
        let id = resolve_card(&view, &args.card)?;
        match all_cards(&view).find(|c| c.id == id) {
            Some(card) => Ok(event::card_json(card)),
            None => Err(format!("no card matching '{}'", args.card)),
        }
    }
}

// ===== write tools =====

/// A [`store::Publisher`] that fans locally-ingested board events out to the
/// selected account's private relays through a [`ToolContext`]'s publisher —
/// the tool-side analogue of the Headway app's own `PrivateRelayPublisher`.
struct RelayFanout<'x, 'a, 'p> {
    api: &'x mut ExplicitPublishApi<'a, 'p>,
    relays: Vec<RelayId>,
}

impl store::Publisher for RelayFanout<'_, '_, '_> {
    fn publish(&mut self, event_frame: &str) {
        fan_out_event_frame(self.api, event_frame, &self.relays);
    }
}

/// The result of a mutating tool: a simple acknowledgement. The change ingests
/// and publishes asynchronously, so re-query with `headway_show_board` /
/// `headway_show_card` to see the settled state.
#[derive(Serialize, Debug)]
struct Applied {
    ok: bool,
}

impl Applied {
    fn ok() -> Self {
        Applied { ok: true }
    }
}

/// Resolve a column selector — a column id or a case-insensitive name — to its
/// index on `view`, mirroring the CLI.
fn resolve_col(view: &BoardView, sel: &str) -> Result<usize, String> {
    view.columns
        .iter()
        .position(|c| c.id == sel || c.name.eq_ignore_ascii_case(sel))
        .ok_or_else(|| {
            let names: Vec<&str> = view.columns.iter().map(|c| c.name.as_str()).collect();
            format!("no column matching '{sel}'; columns: {}", names.join(", "))
        })
}

/// Split a comma-separated label argument into trimmed, non-empty labels.
/// `ToolArgType` has no array kind, so list arguments arrive as delimited
/// strings (as the CLI's `present_notes` tool does with note ids).
fn split_labels(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Apply a board mutation for the selected account and fan the resulting events
/// out to its private relays.
///
/// Folds `board_id` so `build` can resolve card/column refs against the live
/// view, then signs, ingests, and publishes the action. Errors when the account
/// is watch-only or the context has no publisher (a read-only context), so a
/// change is never silently dropped.
fn mutate(
    cx: &mut ToolContext,
    board_id: &str,
    build: impl FnOnce(&BoardView) -> Result<BoardAction, String>,
) -> Result<Applied, String> {
    let author = *cx.accounts.selected_account_pubkey();
    let relays = cx.accounts.selected_account_private_relays();

    // Fold + resolve first, so a bad card/column ref reports the specific
    // resolution error rather than a generic capability error.
    let view = {
        let txn = Transaction::new(cx.ndb).map_err(|e| format!("failed to open db: {e}"))?;
        event::load_board(cx.ndb, &txn, &author, board_id)
            .ok_or_else(|| format!("no board '{board_id}'"))?
    };
    let action = build(&view)?;

    let secret = cx
        .accounts
        .selected_filled()
        .map(|f| f.secret_key.secret_bytes())
        .ok_or("the selected account is watch-only and cannot sign changes")?;

    let Some(api) = cx.publish.as_mut() else {
        return Err("publishing is unavailable in this context".to_string());
    };
    let mut publisher = RelayFanout { api, relays };
    store::apply(
        cx.ndb,
        board_id,
        &view,
        &author,
        &store::Signer::new(&secret, None),
        action,
        &mut publisher,
    );
    Ok(Applied::ok())
}

/// The board a mutation targets: the `board` argument, or the primary board.
fn board_of(board: &Option<String>) -> &str {
    board.as_deref().unwrap_or(store::BOARD_ID)
}

/// `headway_add_card`: create a card on a board.
struct AddCard;

/// Arguments for [`AddCard`].
#[derive(Deserialize)]
struct AddCardArgs {
    title: String,
    /// Target column id or name; the first column when omitted.
    #[serde(default)]
    column: Option<String>,
    /// Comma-separated labels to tag the new card with.
    #[serde(default)]
    labels: Option<String>,
    /// A card ref to parent the new card under (create it as a subissue).
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for AddCard {
    type Args = AddCardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_add_card",
            "Create a card on a headway board. Defaults to the first column of the primary board.",
            vec![
                ToolArg::new("title", ToolArgType::String, "The card title.").required(true),
                ToolArg::new(
                    "column",
                    ToolArgType::String,
                    "Target column id or name. Defaults to the first column.",
                ),
                ToolArg::new(
                    "labels",
                    ToolArgType::String,
                    "Optional comma-separated labels to tag the card with.",
                ),
                ToolArg::new(
                    "parent",
                    ToolArgType::String,
                    "Optional card ref to parent the new card under (make it a subissue).",
                ),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            let col = match args.column.as_deref() {
                Some(sel) => resolve_col(view, sel)?,
                None => 0,
            };
            let parent = match args.parent.as_deref() {
                Some(p) => Some(resolve_card(view, p)?),
                None => None,
            };
            Ok(BoardAction::AddCard {
                col,
                title: args.title.clone(),
                labels: args.labels.as_deref().map(split_labels).unwrap_or_default(),
                parent,
            })
        })
    }
}

/// `headway_move_card`: move a card to another column (appended to its end).
struct MoveCard;

/// Arguments for [`MoveCard`].
#[derive(Deserialize)]
struct MoveCardArgs {
    card: String,
    /// Target column id or name.
    column: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for MoveCard {
    type Args = MoveCardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_move_card",
            "Move a headway card to another column (appended to the end of that column).",
            vec![
                card_arg(),
                ToolArg::new("column", ToolArgType::String, "Target column id or name.")
                    .required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            let card = resolve_card(view, &args.card)?;
            let to_col = resolve_col(view, &args.column)?;
            let to_row = view.columns[to_col].cards.len();
            Ok(BoardAction::MoveCard {
                card,
                to_col,
                to_row,
            })
        })
    }
}

/// `headway_set_title`: replace a card's title.
struct SetTitle;

/// Arguments for [`SetTitle`].
#[derive(Deserialize)]
struct SetTitleArgs {
    card: String,
    title: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for SetTitle {
    type Args = SetTitleArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_title",
            "Replace a headway card's title.",
            vec![
                card_arg(),
                ToolArg::new("title", ToolArgType::String, "The new title.").required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::EditTitle {
                card: resolve_card(view, &args.card)?,
                title: args.title.clone(),
            })
        })
    }
}

/// `headway_set_description`: replace a card's description.
struct SetDescription;

/// Arguments for [`SetDescription`].
#[derive(Deserialize)]
struct SetDescriptionArgs {
    card: String,
    description: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for SetDescription {
    type Args = SetDescriptionArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_description",
            "Replace a headway card's description (its body / cover note).",
            vec![
                card_arg(),
                ToolArg::new("description", ToolArgType::String, "The new description.")
                    .required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::EditDescription {
                card: resolve_card(view, &args.card)?,
                description: args.description.clone(),
            })
        })
    }
}

/// `headway_set_labels`: set a card's labels (additive union with existing).
struct SetLabels;

/// Arguments for [`SetLabels`].
#[derive(Deserialize)]
struct SetLabelsArgs {
    card: String,
    /// Comma-separated labels.
    labels: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for SetLabels {
    type Args = SetLabelsArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_labels",
            "Add labels to a headway card (a comma-separated set, unioned with any existing labels).",
            vec![
                card_arg(),
                ToolArg::new("labels", ToolArgType::String, "Comma-separated labels.")
                    .required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::SetLabels {
                card: resolve_card(view, &args.card)?,
                labels: split_labels(&args.labels),
            })
        })
    }
}

/// `headway_set_priority`: set a card's priority.
struct SetPriority;

/// Arguments for [`SetPriority`].
#[derive(Deserialize)]
struct SetPriorityArgs {
    card: String,
    /// One of `none`, `low`, `medium`, `high`, `urgent`.
    priority: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for SetPriority {
    type Args = SetPriorityArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_priority",
            "Set a headway card's priority. `none` clears it.",
            vec![
                card_arg(),
                ToolArg::new(
                    "priority",
                    ToolArgType::Enum(vec!["none", "low", "medium", "high", "urgent"]),
                    "The priority level.",
                )
                .required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::SetPriority {
                card: resolve_card(view, &args.card)?,
                priority: Priority::parse(&args.priority),
            })
        })
    }
}

/// `headway_set_parent`: make a card a subissue of another, or detach it.
struct SetParent;

/// Arguments for [`SetParent`].
#[derive(Deserialize)]
struct SetParentArgs {
    card: String,
    /// The parent card ref; omit to detach `card` from its parent.
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for SetParent {
    type Args = SetParentArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_set_parent",
            "Make a headway card a subissue of another card, or omit `parent` to detach it. Refused if it would create a parent cycle.",
            vec![
                card_arg(),
                ToolArg::new(
                    "parent",
                    ToolArgType::String,
                    "The parent card ref. Omit to detach the card from its parent.",
                ),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            let card = resolve_card(view, &args.card)?;
            let parent = match args.parent.as_deref() {
                Some(p) => Some(resolve_card(view, p)?),
                None => None,
            };
            Ok(BoardAction::SetParent { card, parent })
        })
    }
}

/// `headway_comment_card`: post a comment on a card.
struct CommentCard;

/// Arguments for [`CommentCard`].
#[derive(Deserialize)]
struct CommentCardArgs {
    card: String,
    body: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for CommentCard {
    type Args = CommentCardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_comment_card",
            "Post a comment on a headway card.",
            vec![
                card_arg(),
                ToolArg::new("body", ToolArgType::String, "The comment text.").required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::AddComment {
                card: resolve_card(view, &args.card)?,
                body: args.body.clone(),
                reply_to: None,
            })
        })
    }
}

/// `headway_archive_card`: take a card off the board, keeping it recoverable.
struct ArchiveCard;

/// Arguments for a card-only mutation (archive / restore / delete).
#[derive(Deserialize)]
struct CardArgs {
    card: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for ArchiveCard {
    type Args = CardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_archive_card",
            "Archive a headway card: take it off the board but keep it recoverable.",
            vec![card_arg(), board_arg()],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::ArchiveCard {
                card: resolve_card(view, &args.card)?,
            })
        })
    }
}

/// `headway_restore_card`: restore an archived card to its origin column.
struct RestoreCard;

impl AppTool for RestoreCard {
    type Args = CardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_restore_card",
            "Restore an archived headway card to the column it was archived from.",
            vec![card_arg(), board_arg()],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::RestoreCard {
                card: resolve_card(view, &args.card)?,
            })
        })
    }
}

/// `headway_delete_card`: remove a card from the board.
struct DeleteCard;

impl AppTool for DeleteCard {
    type Args = CardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_delete_card",
            "Remove a headway card from the board (tombstone its placement).",
            vec![card_arg(), board_arg()],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |view| {
            Ok(BoardAction::DeleteCard {
                card: resolve_card(view, &args.card)?,
            })
        })
    }
}

/// `headway_rename_board`: change the board's display title (slug unchanged).
struct RenameBoard;

/// Arguments for [`RenameBoard`].
#[derive(Deserialize)]
struct RenameBoardArgs {
    title: String,
    #[serde(default)]
    board: Option<String>,
}

impl AppTool for RenameBoard {
    type Args = RenameBoardArgs;
    type Output = Applied;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "headway_rename_board",
            "Rename a headway board's display title. Its slug is unchanged, so existing card refs keep working.",
            vec![
                ToolArg::new("title", ToolArgType::String, "The new board title.").required(true),
                board_arg(),
            ],
        )
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        mutate(cx, board_of(&args.board), |_view| {
            Ok(BoardAction::RenameBoard {
                title: args.title.clone(),
            })
        })
    }
}

/// The shared `card` argument: a card ref accepted by [`resolve_card`].
fn card_arg() -> ToolArg {
    ToolArg::new(
        "card",
        ToolArgType::String,
        "The card ref: a word-id or a hex id prefix.",
    )
    .required(true)
}

/// The shared optional `board` argument, defaulting to the primary board.
fn board_arg() -> ToolArg {
    ToolArg::new(
        "board",
        ToolArgType::String,
        "The board id (slug). Defaults to the primary board.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use headway::wordid;
    use nostrdb::{Config, Ndb};
    use notedeck::{Accounts, NoteCache, UnknownIds};
    use serde_json::json;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// A seeded demo board plus an `Accounts` whose selected pubkey authored it,
    /// so the tools fold the right board. The demo board lands 3 / 2 / 1 / 0 / 1
    /// cards across its five columns (7 total). Owned pieces are returned so they
    /// outlive the borrows a `ToolContext` takes.
    fn seeded_env() -> (TempDir, Ndb, Accounts, NoteCache) {
        let dir = TempDir::new().expect("tmp dir");
        let mut ndb = Ndb::new(dir.path().to_str().expect("path"), &Config::new()).expect("ndb");
        let kp = FullKeypair::generate();
        let secret = kp.secret_key.secret_bytes();
        store::seed_demo_board(
            &ndb,
            &kp.pubkey,
            &secret,
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        );

        // Ingest is async — wait for the board to materialise with all 7 cards.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let txn = Transaction::new(&ndb).expect("txn");
            let ready = event::load_board(&ndb, &txn, &kp.pubkey, store::BOARD_ID)
                .map(|v| v.columns.iter().map(|c| c.cards.len()).sum::<usize>() == 7)
                .unwrap_or(false);
            drop(txn);
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "seeded board never materialised");
            std::thread::sleep(Duration::from_millis(20));
        }

        let txn = Transaction::new(&ndb).expect("txn");
        let mut unknown_ids = UnknownIds::default();
        // `fallback` is the selected account when no keystore is given, so the
        // tools fold this keypair's board.
        let accounts = Accounts::new(
            None,
            Vec::new(),
            Vec::new(),
            kp.pubkey,
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );
        drop(txn);
        (dir, ndb, accounts, NoteCache::default())
    }

    fn tool_context<'a>(
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        accounts: &'a Accounts,
    ) -> ToolContext<'a, 'a> {
        ToolContext {
            ndb,
            note_cache,
            accounts,
            publish: None,
        }
    }

    #[test]
    fn list_boards_reports_the_seeded_board() {
        let (_dir, ndb, accounts, mut note_cache) = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let out = ListBoards.call(&mut cx, ListBoardsArgs {}).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, store::BOARD_ID);
        assert_eq!(out[0].card_count, 7);
    }

    #[test]
    fn show_board_returns_five_columns() {
        let (_dir, ndb, accounts, mut note_cache) = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let out = ShowBoard
            .call(&mut cx, ShowBoardArgs { board: None })
            .unwrap();
        assert_eq!(out["id"], store::BOARD_ID);
        assert_eq!(out["columns"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn show_board_errors_on_unknown_board() {
        let (_dir, ndb, accounts, mut note_cache) = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let err = ShowBoard
            .call(
                &mut cx,
                ShowBoardArgs {
                    board: Some("nope".to_string()),
                },
            )
            .unwrap_err();
        assert!(err.contains("no board 'nope'"));
    }

    #[test]
    fn show_card_resolves_by_word_id() {
        let (_dir, ndb, accounts, mut note_cache) = seeded_env();

        // Pull a real card's word-id out of the folded board.
        let words = {
            let txn = Transaction::new(&ndb).expect("txn");
            let author = *accounts.selected_account_pubkey();
            let view = event::load_board(&ndb, &txn, &author, store::BOARD_ID).expect("board");
            let card = view
                .columns
                .iter()
                .flat_map(|c| &c.cards)
                .next()
                .expect("card");
            wordid::encode(card.id.bytes())
        };

        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);
        let out = ShowCard
            .call(
                &mut cx,
                ShowCardArgs {
                    card: words.clone(),
                    board: None,
                },
            )
            .unwrap();
        assert_eq!(out["words"], json!(words));
        assert!(out["title"].is_string());
    }

    #[test]
    fn show_card_errors_on_unknown_card() {
        let (_dir, ndb, accounts, mut note_cache) = seeded_env();
        let mut cx = tool_context(&ndb, &mut note_cache, &accounts);

        let err = ShowCard
            .call(
                &mut cx,
                ShowCardArgs {
                    card: "no-such-card".to_string(),
                    board: None,
                },
            )
            .unwrap_err();
        assert!(err.contains("no card matching"));
    }

    fn first_card_words(ndb: &Ndb, accounts: &Accounts) -> String {
        let txn = Transaction::new(ndb).expect("txn");
        let author = *accounts.selected_account_pubkey();
        let view = event::load_board(ndb, &txn, &author, store::BOARD_ID).expect("board");
        let card = view
            .columns
            .iter()
            .flat_map(|c| &c.cards)
            .next()
            .expect("card");
        wordid::encode(card.id.bytes())
    }

    #[test]
    fn resolve_col_by_id_and_name() {
        let (_dir, ndb, accounts, _nc) = seeded_env();
        let txn = Transaction::new(&ndb).expect("txn");
        let author = *accounts.selected_account_pubkey();
        let view = event::load_board(&ndb, &txn, &author, store::BOARD_ID).expect("board");

        // The default board's first column is Backlog.
        assert_eq!(resolve_col(&view, "backlog").unwrap(), 0);
        assert_eq!(resolve_col(&view, "BACKLOG").unwrap(), 0);
        assert_eq!(resolve_col(&view, &view.columns[0].id).unwrap(), 0);
        assert!(resolve_col(&view, "nope").is_err());
    }

    #[test]
    fn split_labels_trims_and_drops_empties() {
        assert_eq!(split_labels("a, b ,,c "), vec!["a", "b", "c"]);
        assert!(split_labels("  ,  ").is_empty());
    }

    #[test]
    fn move_card_unknown_column_errors() {
        // The card resolves, so the error is specifically the column miss.
        let (_dir, ndb, accounts, mut nc) = seeded_env();
        let card = first_card_words(&ndb, &accounts);
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = MoveCard
            .call(
                &mut cx,
                MoveCardArgs {
                    card,
                    column: "nope".to_string(),
                    board: None,
                },
            )
            .unwrap_err();
        assert!(err.contains("no column matching"), "{err}");
    }

    #[test]
    fn set_priority_unknown_card_errors() {
        let (_dir, ndb, accounts, mut nc) = seeded_env();
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = SetPriority
            .call(
                &mut cx,
                SetPriorityArgs {
                    card: "no-such".to_string(),
                    priority: "high".to_string(),
                    board: None,
                },
            )
            .unwrap_err();
        assert!(err.contains("no card matching"), "{err}");
    }

    #[test]
    fn write_tool_rejects_watch_only_account() {
        // The seeded account is pubkey-only. A valid mutation folds and resolves
        // but must refuse to sign rather than silently drop the change.
        let (_dir, ndb, accounts, mut nc) = seeded_env();
        let card = first_card_words(&ndb, &accounts);
        let mut cx = tool_context(&ndb, &mut nc, &accounts);

        let err = SetPriority
            .call(
                &mut cx,
                SetPriorityArgs {
                    card,
                    priority: "high".to_string(),
                    board: None,
                },
            )
            .unwrap_err();
        assert!(err.contains("watch-only"), "{err}");
    }
}
