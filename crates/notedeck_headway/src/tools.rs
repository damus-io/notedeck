//! Agent tools exposing this app's headway boards to AI backends over the
//! shared per-app tool interface ([`notedeck::AppTool`]).
//!
//! Each tool is a read-only nostrdb query that reuses headway core's board
//! folding ([`event::fold_board`]) and JSON serializers ([`event::board_json`] /
//! [`event::card_json`]), so a backend (Dave, `notedeck --mcp`) sees exactly the
//! same curated schema the CLI emits with `--json`. Boards are folded for the
//! currently-selected account — the pubkey that authors this user's boards.

use enostr::NoteId;
use headway::event::{self, BoardView, CardView};
use headway::{store, wordid};
use nostrdb::Transaction;
use notedeck::{AppTool, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolSpec};
use serde::{Deserialize, Serialize};

/// Every agent tool the Headway app contributes, ready to register.
pub fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(ListBoards),
        RegisteredTool::new(ShowBoard),
        RegisteredTool::new(ShowCard),
    ]
}

/// Resolve a card ref — a hex id, a unique hex prefix, or a word-id (optionally
/// carrying a `<board>#` or bare `#` prefix) — to its note id within `view`,
/// mirroring the CLI's addressing.
fn resolve_card(view: &BoardView, sel: &str) -> Result<NoteId, String> {
    if let Ok(id) = NoteId::from_hex(sel) {
        return Ok(id);
    }
    let sel = sel.to_lowercase();

    let words = sel
        .strip_prefix(&format!("{}#", view.id.to_lowercase()))
        .or_else(|| sel.strip_prefix('#'))
        .unwrap_or(&sel);
    if let Some(card) = all_cards(view).find(|c| wordid::encode(c.id.bytes()) == words) {
        return Ok(card.id);
    }

    let mut hits = all_cards(view).filter(|c| c.id.hex().starts_with(&sel));
    match (hits.next(), hits.next()) {
        (Some(card), None) => Ok(card.id),
        (Some(_), Some(_)) => Err(format!("ambiguous card prefix '{sel}'")),
        _ => Err(format!("no card matching '{sel}'")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
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
    ) -> ToolContext<'a> {
        ToolContext {
            ndb,
            note_cache,
            accounts,
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
}
