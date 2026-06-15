//! In-memory mock data model for the Headway kanban board.
//!
//! This is deliberately decoupled from any persistence/nostr representation —
//! we're iterating on the board UX first. The eventual event model (addressable
//! events, NIP-34 issues, or a custom kind set) will replace this layer once the
//! UI shape is proven.

/// A single card / issue on the board.
#[derive(Clone, Debug)]
pub struct Card {
    /// Stable identifier, used for drag-and-drop payloads and egui ids.
    pub id: u64,
    pub title: String,
    /// Free-form body shown in the card detail view.
    pub description: String,
    /// Optional colored label; index into [`notedeck::tokens::PALETTE`].
    pub label: Option<usize>,
}

impl Card {
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            description: String::new(),
            label: None,
        }
    }

    pub fn with_label(mut self, label: usize) -> Self {
        self.label = Some(label);
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// A vertical column (status lane) holding an ordered list of cards.
#[derive(Clone, Debug)]
pub struct Column {
    pub title: String,
    pub cards: Vec<Card>,
}

impl Column {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            cards: Vec::new(),
        }
    }
}

/// The whole board: an ordered set of columns.
#[derive(Clone, Debug)]
pub struct Board {
    pub title: String,
    pub columns: Vec<Column>,
    /// Monotonic counter for minting new card ids.
    next_id: u64,
}

impl Board {
    /// Mint the next unique card id.
    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Move a card (identified by `card_id`) into `to_col` at `to_row`,
    /// clamping the row into range. Re-indexing within the same column is
    /// handled so the drop lands where the insertion line indicated.
    pub fn move_card(&mut self, card_id: u64, to_col: usize, to_row: usize) {
        // Locate the card's current position.
        let Some((from_col, from_row)) = self.locate(card_id) else {
            return;
        };
        if to_col >= self.columns.len() {
            return;
        }

        let card = self.columns[from_col].cards.remove(from_row);

        // Removing an earlier card in the same column shifts later indices down.
        let mut to_row = to_row;
        if from_col == to_col && from_row < to_row {
            to_row = to_row.saturating_sub(1);
        }
        let dest = &mut self.columns[to_col].cards;
        let to_row = to_row.min(dest.len());
        dest.insert(to_row, card);
    }

    /// Append a new, empty column to the right of the board.
    pub fn add_column(&mut self, title: impl Into<String>) {
        self.columns.push(Column::new(title));
    }

    /// Rename the column at `idx`, if it exists.
    pub fn rename_column(&mut self, idx: usize, title: impl Into<String>) {
        if let Some(col) = self.columns.get_mut(idx) {
            col.title = title.into();
        }
    }

    /// Remove the column at `idx` (and all of its cards).
    pub fn remove_column(&mut self, idx: usize) {
        if idx < self.columns.len() {
            self.columns.remove(idx);
        }
    }

    /// Move the column at `from` to index `to`, shifting the others. Out-of-range
    /// or no-op moves are ignored.
    pub fn move_column(&mut self, from: usize, to: usize) {
        if from == to || from >= self.columns.len() || to >= self.columns.len() {
            return;
        }
        let col = self.columns.remove(from);
        self.columns.insert(to, col);
    }

    /// Borrow a card mutably by id, searching every column.
    pub fn card_mut(&mut self, id: u64) -> Option<&mut Card> {
        self.columns
            .iter_mut()
            .find_map(|col| col.cards.iter_mut().find(|card| card.id == id))
    }

    /// Remove a card by id from whichever column holds it.
    pub fn remove_card(&mut self, id: u64) {
        for col in &mut self.columns {
            if let Some(pos) = col.cards.iter().position(|card| card.id == id) {
                col.cards.remove(pos);
                return;
            }
        }
    }

    fn locate(&self, card_id: u64) -> Option<(usize, usize)> {
        for (c, col) in self.columns.iter().enumerate() {
            if let Some(r) = col.cards.iter().position(|card| card.id == card_id) {
                return Some((c, r));
            }
        }
        None
    }
}

impl Default for Board {
    /// A seeded demo board so there's something to look at while we iterate.
    fn default() -> Self {
        let mut next_id = 0;
        let mut mint = || {
            let id = next_id;
            next_id += 1;
            id
        };

        let backlog = Column {
            title: "Backlog".to_string(),
            cards: vec![
                Card::new(mint(), "Define nostr event model for boards")
                    .with_label(3)
                    .with_description(
                        "Decide how boards, columns and cards map to nostr events. \
                         Likely an addressable (NIP-33) board event plus per-card events.",
                    ),
                Card::new(mint(), "Sync cards across relays"),
                Card::new(mint(), "Card detail / comments view").with_label(5),
            ],
        };

        let todo = Column {
            title: "Todo".to_string(),
            cards: vec![
                Card::new(mint(), "Inline card creation").with_label(2),
                Card::new(mint(), "Column reordering"),
            ],
        };

        let in_progress = Column {
            title: "In Progress".to_string(),
            cards: vec![
                Card::new(mint(), "Drag-and-drop between columns")
                    .with_label(0)
                    .with_description(
                        "Reorder within a lane and move across lanes with a live insertion line.",
                    ),
            ],
        };

        let done = Column {
            title: "Done".to_string(),
            cards: vec![Card::new(mint(), "Scaffold the Headway app crate").with_label(7)],
        };

        Self {
            title: "Headway".to_string(),
            columns: vec![backlog, todo, in_progress, done],
            next_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(board: &Board) -> Vec<String> {
        board.columns.iter().map(|c| c.title.clone()).collect()
    }

    #[test]
    fn add_column_appends() {
        let mut board = Board::default();
        let before = board.columns.len();
        board.add_column("Review");
        assert_eq!(board.columns.len(), before + 1);
        assert_eq!(board.columns.last().unwrap().title, "Review");
        assert!(board.columns.last().unwrap().cards.is_empty());
    }

    #[test]
    fn rename_column_changes_title_and_ignores_out_of_range() {
        let mut board = Board::default();
        board.rename_column(0, "Inbox");
        assert_eq!(board.columns[0].title, "Inbox");

        // Out-of-range rename is a no-op, not a panic.
        let before = titles(&board);
        board.rename_column(99, "Nope");
        assert_eq!(titles(&board), before);
    }

    #[test]
    fn remove_column_removes_the_right_one_and_ignores_out_of_range() {
        let mut board = Board::default();
        board.remove_column(1); // drop "Todo"
        assert_eq!(titles(&board), ["Backlog", "In Progress", "Done"]);

        let before = titles(&board);
        board.remove_column(99);
        assert_eq!(titles(&board), before);
    }

    #[test]
    fn move_column_reorders() {
        let mut board = Board::default();
        // Backlog (0) to index 2.
        board.move_column(0, 2);
        assert_eq!(titles(&board), ["Todo", "In Progress", "Backlog", "Done"]);
    }

    #[test]
    fn move_column_carries_its_cards() {
        let mut board = Board::default();
        let backlog_cards = board.columns[0].cards.len();
        assert!(backlog_cards > 0);
        board.move_column(0, 3);
        let moved = &board.columns[3];
        assert_eq!(moved.title, "Backlog");
        assert_eq!(moved.cards.len(), backlog_cards);
    }

    #[test]
    fn move_column_ignores_noop_and_out_of_range() {
        let mut board = Board::default();
        let before = titles(&board);
        board.move_column(1, 1); // no-op
        board.move_column(0, 99); // out of range
        board.move_column(99, 0); // out of range
        assert_eq!(titles(&board), before);
    }
}
