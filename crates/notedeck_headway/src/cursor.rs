//! The board's keyboard **card cursor**: pure grid math over the folded
//! [`BoardView`], no egui.
//!
//! Unlike Dave's per-frame block registry, the cursor needs no layout pass to
//! know where it is. Cards carry stable [`NoteId`]s and each column's cards are
//! already rank-sorted by the reducer, so the visible order is just
//! `view.columns[c].cards` filtered by [`ViewFilter::shows`]. The cursor is
//! therefore a bare `Option<NoteId>`, which survives reorders, remote edits and
//! the async ingest after a keyboard move; everything here re-derives its
//! position from the current view.
//!
//! These run on key presses inside a per-frame UI function, so nothing here
//! collects: every walk is a lazy iterator (`nth`, `last`, `take`).

use nostrdb_net::NoteId;

use crate::event::{BoardView, CardView, ColumnView};
use crate::ui::ViewFilter;

/// Where a card sits on the rendered grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorPos {
    /// Index of the card's column in `view.columns`.
    pub col: usize,
    /// True index in `view.columns[col].cards` (what `BoardAction::MoveCard`
    /// and `rank_for_insert` speak).
    pub row: usize,
    /// Index among the column's *visible* cards (what the eye sees).
    pub vis: usize,
}

/// A cursor movement over the board grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// The bindings that issue these land with the board keymap
// (headway:headway/oil-nasty-icon); until then only the tests step the cursor.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum CursorMove {
    /// Next visible card in the column, clamping at the bottom.
    Down,
    /// Previous visible card in the column, clamping at the top.
    Up,
    /// Nearest column to the left that shows any card.
    Left,
    /// Nearest column to the right that shows any card.
    Right,
    /// First visible card of the cursor's column.
    First,
    /// Last visible card of the cursor's column.
    Last,
}

/// The visible cards of a column, lazily, paired with their true row in
/// `column.cards`.
pub(crate) fn visible<'a>(
    column: &'a ColumnView,
    filter: &'a ViewFilter,
) -> impl Iterator<Item = (usize, &'a CardView)> + 'a {
    column
        .cards
        .iter()
        .enumerate()
        .filter(move |(_, card)| filter.shows(card))
}

/// Locate `cursor` on the grid; `None` if it's absent from the board or
/// filtered out of view.
pub(crate) fn locate(view: &BoardView, filter: &ViewFilter, cursor: NoteId) -> Option<CursorPos> {
    view.columns.iter().enumerate().find_map(|(col, column)| {
        visible(column, filter)
            .enumerate()
            .find(|(_, (_, card))| card.id == cursor)
            .map(|(vis, (row, _))| CursorPos { col, row, vis })
    })
}

/// Where the cursor lands after `mv`. Returns the new card id, or `None` only
/// when no card is visible at all.
///
/// A missing cursor, or one that no longer [`locate`]s (archived, moved to
/// another board, filtered out), lands on the first visible card of the first
/// column that has one, whatever the move. Otherwise:
/// - `Down`/`Up` clamp at the column's ends; there is no wrap.
/// - `Left`/`Right` skip columns with no visible cards and land at the same
///   visible row, clamped to the target's length; at the outermost column the
///   cursor stays put.
/// - `First`/`Last` stay within the cursor's column.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn step(
    view: &BoardView,
    filter: &ViewFilter,
    cursor: Option<NoteId>,
    mv: CursorMove,
) -> Option<NoteId> {
    let Some((id, pos)) = cursor.and_then(|id| Some((id, locate(view, filter, id)?))) else {
        return view
            .columns
            .iter()
            .find_map(|column| nth_visible(column, filter, 0));
    };
    let column = &view.columns[pos.col];

    let landed = match mv {
        CursorMove::Down => nth_visible(column, filter, pos.vis + 1),
        CursorMove::Up => pos
            .vis
            .checked_sub(1)
            .and_then(|vis| nth_visible(column, filter, vis)),
        CursorMove::First => nth_visible(column, filter, 0),
        CursorMove::Last => visible(column, filter).last().map(|(_, card)| card.id),
        CursorMove::Left => view.columns[..pos.col]
            .iter()
            .rev()
            .find_map(|target| clamped_visible(target, filter, pos.vis)),
        CursorMove::Right => view.columns[pos.col + 1..]
            .iter()
            .find_map(|target| clamped_visible(target, filter, pos.vis)),
    };
    // Every `None` above is a clamp: the edge of a column or of the board.
    Some(landed.unwrap_or(id))
}

/// The `vis`-th visible card of `column`, if it has that many.
fn nth_visible(column: &ColumnView, filter: &ViewFilter, vis: usize) -> Option<NoteId> {
    visible(column, filter).nth(vis).map(|(_, card)| card.id)
}

/// The `vis`-th visible card of `column`, or its last visible card when it is
/// shorter than that; `None` only when the column shows nothing.
fn clamped_visible(column: &ColumnView, filter: &ViewFilter, vis: usize) -> Option<NoteId> {
    visible(column, filter)
        .take(vis + 1)
        .last()
        .map(|(_, card)| card.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::CardFilter;
    use crate::ui::tests::card;

    /// A card with a distinct id `n` whose title is `title`. Tests filter on
    /// the title, so `"keep"`/`"drop"` decide visibility under a `keep` query.
    fn card_n(n: u8, title: &str) -> CardView {
        CardView {
            id: id(n),
            ..card(title, "", &[])
        }
    }

    fn id(n: u8) -> NoteId {
        NoteId::new([n; 32])
    }

    fn column(name: &str, cards: Vec<CardView>) -> ColumnView {
        ColumnView {
            id: name.to_string(),
            name: name.to_string(),
            terminal: false,
            cards,
        }
    }

    fn board(columns: Vec<ColumnView>) -> BoardView {
        BoardView {
            id: "headway".to_string(),
            author: [0u8; 32],
            title: "Headway".to_string(),
            description: String::new(),
            created_at: 0,
            columns,
            archived: vec![],
        }
    }

    /// Run `f` with a [`ViewFilter`] for `query` (empty shows everything).
    fn with_filter<R>(query: &str, f: impl FnOnce(&ViewFilter) -> R) -> R {
        let parsed = CardFilter::parse(query, "headway");
        f(&ViewFilter {
            filter: &parsed,
            hide_subissues: false,
        })
    }

    /// Three columns: `a` holds 1,2,3; `b` holds 4; `c` holds 5,6.
    fn grid() -> BoardView {
        board(vec![
            column(
                "a",
                vec![card_n(1, "one"), card_n(2, "two"), card_n(3, "three")],
            ),
            column("b", vec![card_n(4, "four")]),
            column("c", vec![card_n(5, "five"), card_n(6, "six")]),
        ])
    }

    #[test]
    fn down_and_up_clamp_at_the_ends() {
        let view = grid();
        with_filter("", |f| {
            assert_eq!(step(&view, f, Some(id(1)), CursorMove::Down), Some(id(2)));
            assert_eq!(step(&view, f, Some(id(2)), CursorMove::Down), Some(id(3)));
            assert_eq!(step(&view, f, Some(id(3)), CursorMove::Down), Some(id(3)));
            assert_eq!(step(&view, f, Some(id(3)), CursorMove::Up), Some(id(2)));
            assert_eq!(step(&view, f, Some(id(1)), CursorMove::Up), Some(id(1)));
        });
    }

    #[test]
    fn filtered_out_card_is_skipped_and_row_stays_true() {
        let view = board(vec![column(
            "a",
            vec![card_n(1, "keep"), card_n(2, "drop"), card_n(3, "keep")],
        )]);
        with_filter("keep", |f| {
            assert_eq!(step(&view, f, Some(id(1)), CursorMove::Down), Some(id(3)));
            assert_eq!(step(&view, f, Some(id(3)), CursorMove::Up), Some(id(1)));
            assert_eq!(
                locate(&view, f, id(3)),
                Some(CursorPos {
                    col: 0,
                    row: 2,
                    vis: 1
                })
            );
        });
    }

    #[test]
    fn left_right_skip_empty_columns_and_clamp_the_row() {
        let view = board(vec![
            column(
                "a",
                vec![card_n(1, "keep"), card_n(2, "keep"), card_n(3, "keep")],
            ),
            // Empty once filtered: holds only a card the filter hides.
            column("b", vec![card_n(4, "drop")]),
            column("c", vec![card_n(5, "keep")]),
            // Truly empty.
            column("d", vec![]),
        ]);
        with_filter("keep", |f| {
            // From the third row of `a`, `b` is skipped and `c`'s single card
            // takes the clamped row.
            assert_eq!(step(&view, f, Some(id(3)), CursorMove::Right), Some(id(5)));
            // Back left keeps the visible row (0), skipping `b` again.
            assert_eq!(step(&view, f, Some(id(5)), CursorMove::Left), Some(id(1)));
            // Outermost columns hold: `d` is empty, so `c` is the rightmost.
            assert_eq!(step(&view, f, Some(id(5)), CursorMove::Right), Some(id(5)));
            assert_eq!(step(&view, f, Some(id(2)), CursorMove::Left), Some(id(2)));
        });
    }

    #[test]
    fn missing_or_hidden_cursor_lands_on_first_visible_card() {
        let view = board(vec![
            column("a", vec![card_n(1, "drop")]),
            column("b", vec![card_n(2, "drop"), card_n(3, "keep")]),
        ]);
        with_filter("keep", |f| {
            for mv in [CursorMove::Down, CursorMove::Left, CursorMove::Last] {
                assert_eq!(step(&view, f, None, mv), Some(id(3)));
                // Absent from the board entirely.
                assert_eq!(step(&view, f, Some(id(9)), mv), Some(id(3)));
                // On the board but filtered out.
                assert_eq!(step(&view, f, Some(id(1)), mv), Some(id(3)));
            }
        });
    }

    #[test]
    fn nothing_visible_yields_none() {
        let view = board(vec![
            column("a", vec![card_n(1, "drop")]),
            column("b", vec![]),
        ]);
        with_filter("keep", |f| {
            assert_eq!(step(&view, f, None, CursorMove::Down), None);
            assert_eq!(step(&view, f, Some(id(1)), CursorMove::Right), None);
        });
    }

    #[test]
    fn first_and_last_stay_in_the_column() {
        let view = grid();
        with_filter("", |f| {
            assert_eq!(step(&view, f, Some(id(2)), CursorMove::Last), Some(id(3)));
            assert_eq!(step(&view, f, Some(id(2)), CursorMove::First), Some(id(1)));
            assert_eq!(step(&view, f, Some(id(5)), CursorMove::Last), Some(id(6)));
            assert_eq!(step(&view, f, Some(id(6)), CursorMove::First), Some(id(5)));
        });
    }
}
