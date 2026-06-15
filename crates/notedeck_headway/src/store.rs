//! Local-only persistence for headway boards.
//!
//! This is the app-layer bridge between the pure schema in [`crate::event`] and
//! nostrdb. It seeds the default board and translates UI intents
//! ([`BoardAction`]) into signed nostr events that are **ingested into the local
//! nostrdb only** — there is deliberately no relay publishing yet (see the
//! `headway-local-only` constraint). When we "go remote" this is the single
//! place that grows a `publisher.publish_note(...)` call alongside each ingest.

use enostr::{NoteId, Pubkey};
use nostrdb::{IngestMetadata, Ndb, NoteBuilder};

use crate::event::{
    self, BoardView, COL_DELETED, CardView, ColumnDef, board_address, build_board,
    build_cover_note, build_issue, build_labels, build_placement, build_subject_edit, rank_between,
};

/// The single board headway manages for now. Multi-board support will turn this
/// into a per-board identifier carried on [`crate::Headway`].
pub const BOARD_ID: &str = "headway";

/// A UI intent to mutate the board. Collected during rendering and applied
/// afterwards by [`apply`], which turns each variant into one or more ingested
/// events.
pub enum BoardAction {
    /// Move `card` into `to_col` so it lands at display row `to_row`.
    MoveCard {
        card: NoteId,
        to_col: usize,
        to_row: usize,
    },
    /// Create a new card titled `title` at the end of column `col`.
    AddCard { col: usize, title: String },
    /// Replace a card's title (subject edit).
    EditTitle { card: NoteId, title: String },
    /// Replace a card's description (cover note).
    EditDescription { card: NoteId, description: String },
    /// Set a card's labels (additive union with any existing labels).
    SetLabels { card: NoteId, labels: Vec<String> },
    /// Remove a card from the board (tombstone placement).
    DeleteCard { card: NoteId },
    /// Append a new column named `name`.
    AddColumn { name: String },
    /// Rename the column at `col`.
    RenameColumn { col: usize, name: String },
    /// Remove the column at `col`. Its cards become unplaced and fall back to
    /// the first column on the next reduce.
    RemoveColumn { col: usize },
    /// Move the column at `from` to index `to`.
    MoveColumn { from: usize, to: usize },
}

/// Sign `builder` with `secret` and ingest the resulting note into the local
/// nostrdb. Returns the note id, or `None` if building/ingesting failed.
///
/// Local-only: this does *not* publish to relays.
pub fn ingest(ndb: &Ndb, builder: NoteBuilder, secret: &[u8; 32]) -> Option<NoteId> {
    let note = builder.sign(secret).build()?;
    let id = NoteId::new(*note.id());
    let json = enostr::ClientMessage::event(&note).ok()?.to_json().ok()?;
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .ok()?;
    Some(id)
}

/// The default columns a fresh board is seeded with.
fn default_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef::new("backlog", "Backlog"),
        ColumnDef::new("todo", "Todo"),
        ColumnDef::new("in-progress", "In Progress"),
        ColumnDef::new("done", "Done"),
    ]
}

/// The default cards, as `(column id, title, body, labels)`, in display order.
type SeedCard = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
);

fn default_cards() -> Vec<SeedCard> {
    vec![
        (
            "backlog",
            "Define nostr event model for boards",
            "Decide how boards, columns and cards map to nostr events. \
             Likely an addressable (NIP-33) board event plus per-card events.",
            &["nostr"],
        ),
        ("backlog", "Sync cards across relays", "", &["nostr"]),
        ("backlog", "Card detail / comments view", "", &["ui"]),
        ("todo", "Inline card creation", "", &["ui"]),
        ("todo", "Column reordering", "", &[]),
        (
            "in-progress",
            "Drag-and-drop between columns",
            "Reorder within a lane and move across lanes with a live insertion line.",
            &["ux"],
        ),
        ("done", "Scaffold the Headway app crate", "", &["chore"]),
    ]
}

/// Seed a fresh default board for `author` into the local nostrdb: one board
/// event, plus an issue + placement per default card.
pub fn seed_default_board(ndb: &Ndb, author: &Pubkey, secret: &[u8; 32], board_id: &str) {
    let addr = board_address(author, board_id);
    let columns = default_columns();
    ingest(ndb, build_board(board_id, "Headway", "", &columns), secret);

    // Hand out increasing ranks per column so cards keep their seeded order.
    let mut last_rank: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for (col_id, title, body, labels) in default_cards() {
        let Some(id) = ingest(ndb, build_issue(&addr, title, body), secret) else {
            continue;
        };
        let rank = rank_between(last_rank.get(col_id).map(|s| s.as_str()), None);
        ingest(
            ndb,
            build_placement(board_id, &addr, &id, col_id, &rank),
            secret,
        );
        if !labels.is_empty() {
            ingest(ndb, build_labels(&id, labels), secret);
        }
        last_rank.insert(col_id, rank);
    }
}

/// Apply one [`BoardAction`] against the current `view`, ingesting the events it
/// implies. `view` is the pre-action snapshot, used to compute insertion ranks
/// and to reconstruct the column list for board-level edits.
pub fn apply(
    ndb: &Ndb,
    board_id: &str,
    view: &BoardView,
    author: &Pubkey,
    secret: &[u8; 32],
    action: BoardAction,
) {
    let addr = board_address(author, board_id);

    match action {
        BoardAction::MoveCard {
            card,
            to_col,
            to_row,
        } => {
            let Some(col) = view.columns.get(to_col) else {
                return;
            };
            let rank = rank_for_insert(&col.cards, Some(card), to_row);
            ingest(
                ndb,
                build_placement(board_id, &addr, &card, &col.id, &rank),
                secret,
            );
        }
        BoardAction::AddCard { col, title } => {
            let Some(c) = view.columns.get(col) else {
                return;
            };
            let Some(id) = ingest(ndb, build_issue(&addr, &title, ""), secret) else {
                return;
            };
            let rank = rank_for_insert(&c.cards, None, c.cards.len());
            ingest(
                ndb,
                build_placement(board_id, &addr, &id, &c.id, &rank),
                secret,
            );
        }
        BoardAction::EditTitle { card, title } => {
            ingest(ndb, build_subject_edit(&card, &title), secret);
        }
        BoardAction::EditDescription { card, description } => {
            ingest(ndb, build_cover_note(&card, author, &description), secret);
        }
        BoardAction::SetLabels { card, labels } => {
            ingest(ndb, build_labels(&card, &labels), secret);
        }
        BoardAction::DeleteCard { card } => {
            // build_placement needs a rank; reuse the card's current one (or a
            // midpoint) — the column is the tombstone sentinel either way.
            let rank = find_card(view, card)
                .map(|c| c.rank.clone())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "m".to_string());
            ingest(
                ndb,
                build_placement(board_id, &addr, &card, COL_DELETED, &rank),
                secret,
            );
        }
        BoardAction::AddColumn { name } => {
            let mut cols = column_defs(view);
            cols.push(ColumnDef::new(unique_col_id(&cols, &name), name));
            republish_board(ndb, board_id, view, secret, &cols);
        }
        BoardAction::RenameColumn { col, name } => {
            let mut cols = column_defs(view);
            let Some(def) = cols.get_mut(col) else {
                return;
            };
            def.name = name;
            republish_board(ndb, board_id, view, secret, &cols);
        }
        BoardAction::RemoveColumn { col } => {
            let mut cols = column_defs(view);
            if col >= cols.len() {
                return;
            }
            cols.remove(col);
            republish_board(ndb, board_id, view, secret, &cols);
        }
        BoardAction::MoveColumn { from, to } => {
            let mut cols = column_defs(view);
            if from >= cols.len() || to >= cols.len() || from == to {
                return;
            }
            let def = cols.remove(from);
            cols.insert(to, def);
            republish_board(ndb, board_id, view, secret, &cols);
        }
    }
}

/// Republish the board event with a new column list, preserving title/description.
fn republish_board(
    ndb: &Ndb,
    board_id: &str,
    view: &BoardView,
    secret: &[u8; 32],
    columns: &[ColumnDef],
) {
    ingest(
        ndb,
        build_board(board_id, &view.title, &view.description, columns),
        secret,
    );
}

/// The current column definitions carried by `view`, ready to be edited and
/// republished.
fn column_defs(view: &BoardView) -> Vec<ColumnDef> {
    view.columns
        .iter()
        .map(|c| ColumnDef::new(c.id.clone(), c.name.clone()))
        .collect()
}

/// Find a card anywhere on the board by id.
fn find_card(view: &BoardView, card: NoteId) -> Option<&CardView> {
    view.columns
        .iter()
        .flat_map(|c| c.cards.iter())
        .find(|c| c.id == card)
}

/// Compute a fractional rank that lands a card at display index `to_row` in a
/// column whose current cards are `cards` (sorted by rank). `moving` excludes
/// the dragged card from the neighbour search so an in-column move doesn't
/// fence itself.
fn rank_for_insert(cards: &[CardView], moving: Option<NoteId>, to_row: usize) -> String {
    let others: Vec<&CardView> = cards.iter().filter(|c| Some(c.id) != moving).collect();

    // `to_row` indexes the displayed list (which still includes the moved card);
    // translate it into an index among `others`.
    let pos = match moving.and_then(|m| cards.iter().position(|c| c.id == m)) {
        Some(cur) if cur < to_row => to_row - 1,
        _ => to_row,
    };
    let pos = pos.min(others.len());

    let left = pos
        .checked_sub(1)
        .and_then(|i| others.get(i))
        .map(|c| c.rank.as_str());
    let right = others.get(pos).map(|c| c.rank.as_str());
    rank_between(left, right)
}

/// Slugify `name` into a column id not already present in `existing`.
fn unique_col_id(existing: &[ColumnDef], name: &str) -> String {
    let mut base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Collapse runs of '-' and trim them from the ends.
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() {
        "col".to_string()
    } else {
        base
    };

    let taken = |id: &str| existing.iter().any(|c| c.id == id);
    if !taken(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Convenience re-export so the app layer can load a board without naming the
/// event module directly.
pub use event::load_board;

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use nostrdb::{Config, Ndb, Transaction};
    use std::time::{Duration, Instant};

    struct TestNdb {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
    }

    impl TestNdb {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// Poll the board out of ndb until `pred` holds (ingest is async).
        fn wait<F>(&self, pred: F) -> BoardView
        where
            F: Fn(&BoardView) -> bool,
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let txn = Transaction::new(&self.ndb).unwrap();
                if let Some(view) = load_board(&self.ndb, &txn, &self.kp.pubkey, BOARD_ID)
                    && pred(&view)
                {
                    return view;
                }
                assert!(Instant::now() < deadline, "board predicate never held");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn apply(&self, view: &BoardView, action: BoardAction) {
            super::apply(
                &self.ndb,
                BOARD_ID,
                view,
                &self.kp.pubkey,
                &self.secret(),
                action,
            );
        }
    }

    fn col_titles(view: &BoardView) -> Vec<String> {
        view.columns.iter().map(|c| c.name.clone()).collect()
    }

    fn card_titles(view: &BoardView, col: usize) -> Vec<String> {
        view.columns[col]
            .cards
            .iter()
            .map(|c| c.title.clone())
            .collect()
    }

    #[test]
    fn seed_materialises_default_board() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID);

        let view = t.wait(|v| v.columns.iter().map(|c| c.cards.len()).sum::<usize>() == 7);
        assert_eq!(
            col_titles(&view),
            ["Backlog", "Todo", "In Progress", "Done"]
        );
        assert_eq!(view.columns[0].cards.len(), 3);
        assert_eq!(view.columns[3].cards.len(), 1);
        // Seeded order is preserved by increasing ranks.
        assert_eq!(
            view.columns[0].cards[0].title,
            "Define nostr event model for boards"
        );
        assert!(!view.columns[0].cards[0].description.is_empty());
    }

    #[test]
    fn add_card_appends_to_column() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID);
        let view = t.wait(|v| v.columns[1].cards.len() == 2);

        t.apply(
            &view,
            BoardAction::AddCard {
                col: 1,
                title: "New idea".to_string(),
            },
        );

        let view = t.wait(|v| v.columns[1].cards.len() == 3);
        assert_eq!(card_titles(&view, 1).last().unwrap(), "New idea");
    }

    #[test]
    fn move_card_changes_column() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID);
        let view = t.wait(|v| v.columns[0].cards.len() == 3);

        let card = view.columns[0].cards[0].id;
        t.apply(
            &view,
            BoardAction::MoveCard {
                card,
                to_col: 3,
                to_row: view.columns[3].cards.len(),
            },
        );

        let view = t.wait(|v| v.columns[3].cards.len() == 2);
        assert_eq!(view.columns[0].cards.len(), 2);
        assert!(view.columns[3].cards.iter().any(|c| c.id == card));
    }

    #[test]
    fn edit_title_description_and_labels() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID);
        let view = t.wait(|v| v.columns[1].cards.len() == 2);
        // The second Todo card ("Column reordering") is seeded without labels,
        // so the SetLabels union below is exactly the two we add.
        let card = view.columns[1].cards[1].id;

        t.apply(
            &view,
            BoardAction::EditTitle {
                card,
                title: "Renamed".to_string(),
            },
        );
        t.apply(
            &view,
            BoardAction::EditDescription {
                card,
                description: "the details".to_string(),
            },
        );
        t.apply(
            &view,
            BoardAction::SetLabels {
                card,
                labels: vec!["bug".to_string(), "ux".to_string()],
            },
        );

        let view = t.wait(|v| {
            v.columns[1].cards.iter().any(|c| {
                c.id == card
                    && c.title == "Renamed"
                    && c.description == "the details"
                    && c.labels.len() == 2
            })
        });
        let edited = view.columns[1].cards.iter().find(|c| c.id == card).unwrap();
        assert_eq!(edited.title, "Renamed");
        assert_eq!(edited.description, "the details");
        assert_eq!(edited.labels, vec!["bug".to_string(), "ux".to_string()]);
    }

    #[test]
    fn delete_card_removes_it() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID);
        let view = t.wait(|v| v.columns[0].cards.len() == 3);
        let card = view.columns[0].cards[0].id;

        t.apply(&view, BoardAction::DeleteCard { card });

        let view = t.wait(|v| v.columns[0].cards.len() == 2);
        assert!(!view.columns[0].cards.iter().any(|c| c.id == card));
    }

    #[test]
    fn column_ops_round_trip() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID);
        let view = t.wait(|v| v.columns.len() == 4);

        t.apply(
            &view,
            BoardAction::AddColumn {
                name: "Review".to_string(),
            },
        );
        let view = t.wait(|v| v.columns.len() == 5);
        assert_eq!(view.columns[4].name, "Review");

        t.apply(
            &view,
            BoardAction::RenameColumn {
                col: 0,
                name: "Inbox".to_string(),
            },
        );
        let view = t.wait(|v| v.columns[0].name == "Inbox");

        t.apply(&view, BoardAction::MoveColumn { from: 0, to: 1 });
        let view = t.wait(|v| v.columns[1].name == "Inbox");

        t.apply(&view, BoardAction::RemoveColumn { col: 1 });
        let view = t.wait(|v| !v.columns.iter().any(|c| c.name == "Inbox"));
        // The removed column's cards aren't lost; they fall back to column 0.
        assert!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>() >= 7);
    }
}
