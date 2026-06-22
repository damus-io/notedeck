//! Persistence for headway boards.
//!
//! This is the app-layer bridge between the pure schema in [`crate::event`] and
//! nostrdb. It seeds the default board and translates UI intents
//! ([`BoardAction`]) into signed nostr events that are ingested into a local
//! nostrdb. Every ingested event is also handed to a [`Publisher`], the single
//! seam for fanning changes outward to a relay: the egui app ingests straight
//! into the nostrdb its embedded relay serves and so uses [`NoPublish`], while
//! the CLI keeps its own nostrdb and publishes each event to the running app's
//! relay over its websocket.

use enostr::{NoteId, Pubkey};
use nostrdb::{IngestMetadata, Ndb, NoteBuilder};

use crate::event::{
    self, BoardView, COL_DELETED, CardView, ColumnDef, board_address, build_archive_placement,
    build_board, build_cover_note, build_issue, build_labels, build_placement, build_subject_edit,
    rank_between,
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
    /// Create a new card titled `title` at the end of column `col`, optionally
    /// tagging it with `labels`.
    AddCard {
        col: usize,
        title: String,
        labels: Vec<String>,
    },
    /// Replace a card's title (subject edit).
    EditTitle { card: NoteId, title: String },
    /// Replace a card's description (cover note).
    EditDescription { card: NoteId, description: String },
    /// Set a card's labels (additive union with any existing labels).
    SetLabels { card: NoteId, labels: Vec<String> },
    /// Remove a card from the board (tombstone placement).
    DeleteCard { card: NoteId },
    /// Archive a card: take it off the board but keep it recoverable, recording
    /// the column it came from so a restore can put it back.
    ArchiveCard { card: NoteId },
    /// Restore an archived card to the column it was archived from (or the first
    /// column if that column no longer exists).
    RestoreCard { card: NoteId },
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

/// A sink for events that have been ingested locally and should also be fanned
/// out — typically published to a relay. [`ingest`] hands every event it stores
/// to the publisher as a ready-to-send NIP-01 `["EVENT", {...}]` frame, in the
/// order they were ingested.
pub trait Publisher {
    /// Called once per successfully ingested event with its `["EVENT", {...}]`
    /// JSON frame, ready to write to a relay websocket.
    fn publish(&mut self, event_frame: &str);
}

/// A [`Publisher`] that drops everything: local ingest only, no fan-out. Used by
/// the egui app, whose embedded relay already serves the same nostrdb it ingests
/// into, so there is nothing to publish.
pub struct NoPublish;

impl Publisher for NoPublish {
    fn publish(&mut self, _event_frame: &str) {}
}

/// Sign `builder` with `secret` and ingest the resulting note into the local
/// nostrdb, then hand its `["EVENT", {...}]` frame to `publisher`. Returns the
/// note id, or `None` if building/ingesting failed (in which case nothing is
/// published).
pub fn ingest(
    ndb: &Ndb,
    builder: NoteBuilder,
    secret: &[u8; 32],
    publisher: &mut dyn Publisher,
) -> Option<NoteId> {
    let note = builder.sign(secret).build()?;
    let id = NoteId::new(*note.id());
    let json = enostr::ClientMessage::event(&note).ok()?.to_json().ok()?;
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .ok()?;
    publisher.publish(&json);
    Some(id)
}

/// The default columns a fresh board is seeded with.
fn default_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef::new("backlog", "Backlog"),
        ColumnDef::new("todo", "Todo"),
        ColumnDef::new("in-progress", "In Progress"),
        ColumnDef::new("in-review", "In Review"),
        ColumnDef::new("done", "Done"),
    ]
}

/// Seed a fresh default board for `author` into the local nostrdb: just the
/// board event with its columns, no cards. Cards are added later via
/// [`BoardAction::AddCard`].
pub fn seed_default_board(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    board_id: &str,
    publisher: &mut dyn Publisher,
) {
    let _ = author;
    let columns = default_columns();
    ingest(
        ndb,
        build_board(board_id, "Headway", "", &columns),
        secret,
        publisher,
    );
}

/// Seed a default board *and* a fixed set of demo cards. The product seed
/// ([`seed_default_board`]) is deliberately card-less; this is the populated
/// board used by tests and demos. Cards land 3 / 2 / 1 / 0 / 1 across the
/// columns, in seeded order (increasing ranks per column).
pub fn seed_demo_board(
    ndb: &Ndb,
    author: &Pubkey,
    secret: &[u8; 32],
    board_id: &str,
    publisher: &mut dyn Publisher,
) {
    seed_default_board(ndb, author, secret, board_id, publisher);

    let addr = board_address(author, board_id);
    let cards: [(&str, &str, &str, &[&str]); 7] = [
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
    ];

    // Hand out increasing ranks per column so cards keep their seeded order.
    let mut last_rank: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for (col_id, title, body, labels) in cards {
        let Some(id) = ingest(ndb, build_issue(&addr, title, body), secret, publisher) else {
            continue;
        };
        let rank = rank_between(last_rank.get(col_id).map(|s| s.as_str()), None);
        ingest(
            ndb,
            build_placement(board_id, &addr, &id, col_id, &rank),
            secret,
            publisher,
        );
        if !labels.is_empty() {
            ingest(ndb, build_labels(&id, labels), secret, publisher);
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
    publisher: &mut dyn Publisher,
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
            let after = find_card(view, card).map_or(0, |c| c.placed_at);
            ingest(
                ndb,
                build_placement(board_id, &addr, &card, &col.id, &rank)
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        BoardAction::AddCard { col, title, labels } => {
            let Some(c) = view.columns.get(col) else {
                return;
            };
            let Some(id) = ingest(ndb, build_issue(&addr, &title, ""), secret, publisher) else {
                return;
            };
            let rank = rank_for_insert(&c.cards, None, c.cards.len());
            ingest(
                ndb,
                build_placement(board_id, &addr, &id, &c.id, &rank),
                secret,
                publisher,
            );
            if !labels.is_empty() {
                ingest(ndb, build_labels(&id, &labels), secret, publisher);
            }
        }
        BoardAction::EditTitle { card, title } => {
            ingest(ndb, build_subject_edit(&card, &title), secret, publisher);
        }
        BoardAction::EditDescription { card, description } => {
            ingest(
                ndb,
                build_cover_note(&card, author, &description),
                secret,
                publisher,
            );
        }
        BoardAction::SetLabels { card, labels } => {
            ingest(ndb, build_labels(&card, &labels), secret, publisher);
        }
        BoardAction::DeleteCard { card } => {
            // build_placement needs a rank; reuse the card's current one (or a
            // midpoint) — the column is the tombstone sentinel either way.
            let c = find_card(view, card);
            let rank = non_empty_rank(c.map_or("", |c| c.rank.as_str()));
            let after = c.map_or(0, |c| c.placed_at);
            ingest(
                ndb,
                build_placement(board_id, &addr, &card, COL_DELETED, &rank)
                    .created_at(next_after(after)),
                secret,
                publisher,
            );
        }
        BoardAction::ArchiveCard { card } => {
            // Capture the card's current column so a restore can return it there.
            let Some((from_col, c)) = find_card_col(view, card) else {
                return;
            };
            let rank = non_empty_rank(&c.rank);
            ingest(
                ndb,
                build_archive_placement(board_id, &addr, &card, from_col, &rank)
                    .created_at(next_after(c.placed_at)),
                secret,
                publisher,
            );
        }
        BoardAction::RestoreCard { card } => {
            let Some(entry) = view.archived.iter().find(|a| a.card.id == card) else {
                return;
            };
            // Restore to the origin column, falling back to the first column if
            // that column is gone (the reducer would reflow it there anyway).
            let to_col = entry
                .from
                .as_deref()
                .filter(|id| view.columns.iter().any(|c| c.id == *id))
                .or_else(|| view.columns.first().map(|c| c.id.as_str()));
            let Some(to_col) = to_col else {
                return;
            };
            let rank = non_empty_rank(&entry.card.rank);
            ingest(
                ndb,
                build_placement(board_id, &addr, &card, to_col, &rank)
                    .created_at(next_after(entry.card.placed_at)),
                secret,
                publisher,
            );
        }
        BoardAction::AddColumn { name } => {
            let mut cols = column_defs(view);
            cols.push(ColumnDef::new(unique_col_id(&cols, &name), name));
            republish_board(ndb, board_id, view, secret, &cols, publisher);
        }
        BoardAction::RenameColumn { col, name } => {
            let mut cols = column_defs(view);
            let Some(def) = cols.get_mut(col) else {
                return;
            };
            def.name = name;
            republish_board(ndb, board_id, view, secret, &cols, publisher);
        }
        BoardAction::RemoveColumn { col } => {
            let mut cols = column_defs(view);
            if col >= cols.len() {
                return;
            }
            cols.remove(col);
            republish_board(ndb, board_id, view, secret, &cols, publisher);
        }
        BoardAction::MoveColumn { from, to } => {
            let mut cols = column_defs(view);
            if from >= cols.len() || to >= cols.len() || from == to {
                return;
            }
            let def = cols.remove(from);
            cols.insert(to, def);
            republish_board(ndb, board_id, view, secret, &cols, publisher);
        }
    }
}

/// Republish the board event with a new column list, preserving title/description.
///
/// The board is an addressable event, so a republish supersedes the prior one by
/// `created_at`. Nostr timestamps are whole seconds, so a quick succession of
/// edits (or our own seed-then-edit in tests) would tie and the reducer would
/// keep the *old* board — dropping the edit. Stamp a timestamp strictly greater
/// than the version we're editing so the new board always wins.
fn republish_board(
    ndb: &Ndb,
    board_id: &str,
    view: &BoardView,
    secret: &[u8; 32],
    columns: &[ColumnDef],
    publisher: &mut dyn Publisher,
) {
    let created_at = now_secs().max(view.created_at + 1);
    ingest(
        ndb,
        build_board(board_id, &view.title, &view.description, columns).created_at(created_at),
        secret,
        publisher,
    );
}

/// The `created_at` to stamp on a re-placement that must supersede a prior
/// placement made at `prev`. Nostr timestamps are whole seconds, so a card
/// moved/deleted/archived in the same second it was last placed would *tie* the
/// reducer's latest-wins and silently no-op; stamp strictly past `prev` so the
/// new placement always wins (mirrors [`republish_board`]).
fn next_after(prev: u64) -> u64 {
    now_secs().max(prev + 1)
}

/// Current wall-clock time in whole seconds since the Unix epoch (nostr's
/// `created_at` unit). Falls back to 0 if the clock is before the epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// Find a card and the id of the column it currently sits in.
fn find_card_col(view: &BoardView, card: NoteId) -> Option<(&str, &CardView)> {
    view.columns.iter().find_map(|col| {
        col.cards
            .iter()
            .find(|c| c.id == card)
            .map(|c| (col.id.as_str(), c))
    })
}

/// A placement needs a rank; fall back to a midpoint when the card has none
/// (e.g. it was sitting unplaced in the fallback column).
fn non_empty_rank(rank: &str) -> String {
    if rank.is_empty() {
        "m".to_string()
    } else {
        rank.to_string()
    }
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
                &mut NoPublish,
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

    /// Seed the populated demo board for the card-operation tests to act on.
    /// Columns: Backlog, Todo, In Progress, In Review, Done; cards 3 / 2 / 1 / 0 / 1.
    fn seed_demo(t: &TestNdb) {
        seed_demo_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID, &mut NoPublish);
    }

    #[test]
    fn seed_materialises_default_board() {
        let t = TestNdb::new();
        seed_default_board(&t.ndb, &t.kp.pubkey, &t.secret(), BOARD_ID, &mut NoPublish);

        // The default board is card-less: just the five columns.
        let view = t.wait(|v| v.columns.len() == 5);
        assert_eq!(
            col_titles(&view),
            ["Backlog", "Todo", "In Progress", "In Review", "Done"]
        );
        assert!(view.columns.iter().all(|c| c.cards.is_empty()));
    }

    #[test]
    fn seed_demo_materialises_cards() {
        let t = TestNdb::new();
        seed_demo(&t);

        let view = t.wait(|v| v.columns.iter().map(|c| c.cards.len()).sum::<usize>() == 7);
        assert_eq!(view.columns[0].cards.len(), 3);
        // Done is the last column; the seeded "done" card lands there.
        assert_eq!(view.columns.last().unwrap().cards.len(), 1);
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
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2);

        t.apply(
            &view,
            BoardAction::AddCard {
                col: 1,
                title: "New idea".to_string(),
                labels: vec![],
            },
        );

        let view = t.wait(|v| v.columns[1].cards.len() == 3);
        assert_eq!(card_titles(&view, 1).last().unwrap(), "New idea");
    }

    #[test]
    fn add_card_with_labels_tags_the_new_card() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2);

        t.apply(
            &view,
            BoardAction::AddCard {
                col: 1,
                title: "Tagged idea".to_string(),
                labels: vec!["bug".to_string(), "ux".to_string()],
            },
        );

        let view = t.wait(|v| {
            v.columns[1]
                .cards
                .iter()
                .any(|c| c.title == "Tagged idea" && c.labels.len() == 2)
        });
        let card = view.columns[1]
            .cards
            .iter()
            .find(|c| c.title == "Tagged idea")
            .unwrap();
        assert_eq!(card.labels, vec!["bug".to_string(), "ux".to_string()]);
    }

    #[test]
    fn publisher_receives_a_frame_per_ingested_event() {
        #[derive(Default)]
        struct Collect(Vec<String>);
        impl Publisher for Collect {
            fn publish(&mut self, frame: &str) {
                self.0.push(frame.to_string());
            }
        }

        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[1].cards.len() == 2);

        // AddCard ingests two events — the issue and its placement — so the
        // publisher should see exactly two ready-to-send EVENT frames.
        let mut sink = Collect::default();
        super::apply(
            &t.ndb,
            BOARD_ID,
            &view,
            &t.kp.pubkey,
            &t.secret(),
            BoardAction::AddCard {
                col: 1,
                title: "Tracked".to_string(),
                labels: vec![],
            },
            &mut sink,
        );

        assert_eq!(sink.0.len(), 2, "issue + placement each publish a frame");
        for frame in &sink.0 {
            assert!(
                frame.starts_with("[\"EVENT\","),
                "frame is a NIP-01 EVENT message: {frame}"
            );
        }
    }

    #[test]
    fn move_card_changes_column() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns[0].cards.len() == 3);

        // Move a Backlog card into Done (the last column, which seeds one card).
        let done = view.columns.len() - 1;
        let card = view.columns[0].cards[0].id;
        t.apply(
            &view,
            BoardAction::MoveCard {
                card,
                to_col: done,
                to_row: view.columns[done].cards.len(),
            },
        );

        let view = t.wait(|v| v.columns[done].cards.len() == 2);
        assert_eq!(view.columns[0].cards.len(), 2);
        assert!(view.columns[done].cards.iter().any(|c| c.id == card));
    }

    #[test]
    fn edit_title_description_and_labels() {
        let t = TestNdb::new();
        seed_demo(&t);
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
        seed_demo(&t);
        let view = t.wait(|v| v.columns[0].cards.len() == 3);
        let card = view.columns[0].cards[0].id;

        t.apply(&view, BoardAction::DeleteCard { card });

        let view = t.wait(|v| v.columns[0].cards.len() == 2);
        assert!(!view.columns[0].cards.iter().any(|c| c.id == card));
    }

    #[test]
    fn archive_then_restore_round_trips_to_origin() {
        let t = TestNdb::new();
        seed_demo(&t);
        // Pick a card out of "In Progress" (column 2), not the first column, so a
        // restore that ignored the origin would land it somewhere else.
        let view = t.wait(|v| v.columns[2].cards.len() == 1);
        let card = view.columns[2].cards[0].id;

        t.apply(&view, BoardAction::ArchiveCard { card });

        // It leaves the columns and shows up in the archived list, with origin.
        let view = t.wait(|v| !v.archived.is_empty());
        assert!(
            view.columns
                .iter()
                .all(|c| c.cards.iter().all(|c| c.id != card))
        );
        assert_eq!(view.archived.len(), 1);
        assert_eq!(view.archived[0].card.id, card);
        assert_eq!(view.archived[0].from.as_deref(), Some("in-progress"));

        t.apply(&view, BoardAction::RestoreCard { card });

        // Restored back into the exact column it came from, and unarchived.
        let view = t.wait(|v| v.archived.is_empty() && v.columns[2].cards.len() == 1);
        assert_eq!(view.columns[2].cards[0].id, card);
    }

    #[test]
    fn column_ops_round_trip() {
        let t = TestNdb::new();
        seed_demo(&t);
        let view = t.wait(|v| v.columns.len() == 5);

        t.apply(
            &view,
            BoardAction::AddColumn {
                name: "Review".to_string(),
            },
        );
        let view = t.wait(|v| v.columns.len() == 6);
        assert_eq!(view.columns[5].name, "Review");

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
