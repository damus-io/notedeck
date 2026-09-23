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

/// Which neighbouring column a keyboard card move (Shift+H/L) heads for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// The column to the left (Shift+H).
    Left,
    /// The column to the right (Shift+L).
    Right,
}

/// Which way a keyboard reorder (Shift+J/K) shifts a card within its column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vertical {
    /// Past the next visible card (Shift+J).
    Down,
    /// Past the previous visible card (Shift+K).
    Up,
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

/// Where Shift+H/L would drop `card`: `(to_col, to_row)` for
/// `BoardAction::MoveCard`, or `None` when the card isn't visible or its column
/// is already the outermost on that side.
///
/// Unlike the cursor's `h`/`l`, this never skips empty columns: moving a card
/// into an empty column is the common case. The card keeps its visual height,
/// landing just before the target's visible card at the same visible index —
/// so `to_row` is that card's *true* row (filtered-out cards are counted), or
/// the end of the column when the target shows fewer cards.
pub(crate) fn move_across(
    view: &BoardView,
    filter: &ViewFilter,
    card: NoteId,
    dir: Side,
) -> Option<(usize, usize)> {
    let pos = locate(view, filter, card)?;
    let to_col = match dir {
        Side::Left => pos.col.checked_sub(1)?,
        Side::Right => pos.col + 1,
    };
    let target = view.columns.get(to_col)?;
    let to_row = visible(target, filter)
        .nth(pos.vis)
        .map_or(target.cards.len(), |(row, _)| row);
    Some((to_col, to_row))
}

/// Where Shift+J/K would drop `card` within its own column: `(to_col, to_row)`
/// for `BoardAction::MoveCard`, or `None` when the card isn't visible or is
/// already at that end of the column.
///
/// The card hops its adjacent *visible* neighbour, not merely the next true
/// row (which may be a filtered-out card, making the move look like a no-op).
/// `to_row` indexes the column *including* the moving card, the way
/// `rank_for_insert` reads it: down lands just past the neighbour
/// (`row + 1`), up lands on the neighbour's row, i.e. just before it.
pub(crate) fn move_within(
    view: &BoardView,
    filter: &ViewFilter,
    card: NoteId,
    dir: Vertical,
) -> Option<(usize, usize)> {
    let pos = locate(view, filter, card)?;
    let column = &view.columns[pos.col];
    let to_row = match dir {
        Vertical::Down => visible(column, filter).nth(pos.vis + 1)?.0 + 1,
        Vertical::Up => visible(column, filter).nth(pos.vis.checked_sub(1)?)?.0,
    };
    Some((pos.col, to_row))
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
pub(crate) mod tests {
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

    /// The distinct card id `n` (all 32 bytes set to it). Shared with
    /// [`crate::keys`]'s tests.
    pub(crate) fn id(n: u8) -> NoteId {
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

    /// Three columns of three: `a` holds 1,2,3; `b` 4,5,6; `c` 7,8,9. Card 5
    /// sits in the middle, so every motion from it goes somewhere. Shared with
    /// [`crate::keys`]'s tests.
    pub(crate) fn square_grid() -> BoardView {
        board(vec![
            column(
                "a",
                vec![card_n(1, "one"), card_n(2, "two"), card_n(3, "three")],
            ),
            column(
                "b",
                vec![card_n(4, "four"), card_n(5, "five"), card_n(6, "six")],
            ),
            column(
                "c",
                vec![card_n(7, "seven"), card_n(8, "eight"), card_n(9, "nine")],
            ),
        ])
    }

    /// Three columns: `a` holds 1,2,3; `b` holds 4; `c` holds 5,6. Shared with
    /// [`crate::keys`]'s tests.
    pub(crate) fn grid() -> BoardView {
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

    #[test]
    fn across_into_an_empty_column_lands_at_row_zero() {
        let view = board(vec![
            column("a", vec![card_n(1, "one"), card_n(2, "two")]),
            column("b", vec![]),
        ]);
        with_filter("", |f| {
            assert_eq!(move_across(&view, f, id(2), Side::Right), Some((1, 0)));
        });
    }

    #[test]
    fn across_from_a_deep_row_into_a_shorter_column_appends() {
        let view = board(vec![
            column(
                "a",
                vec![
                    card_n(1, "one"),
                    card_n(2, "two"),
                    card_n(3, "three"),
                    card_n(4, "four"),
                ],
            ),
            column("b", vec![card_n(5, "five"), card_n(6, "six")]),
        ]);
        with_filter("", |f| {
            // Row 3 of `a`, but `b` only has two cards: land at its end.
            assert_eq!(move_across(&view, f, id(4), Side::Right), Some((1, 2)));
            // Row 1 keeps its height: just before `b`'s second card.
            assert_eq!(move_across(&view, f, id(2), Side::Right), Some((1, 1)));
        });
    }

    #[test]
    fn across_counts_hidden_cards_in_the_target_row() {
        let view = board(vec![
            column("a", vec![card_n(1, "keep"), card_n(2, "keep")]),
            column(
                "b",
                vec![
                    card_n(3, "drop"),
                    card_n(4, "keep"),
                    card_n(5, "drop"),
                    card_n(6, "keep"),
                ],
            ),
        ]);
        with_filter("keep", |f| {
            // Visible row 0 lands before `b`'s first visible card, 4, whose
            // true row is 1.
            assert_eq!(move_across(&view, f, id(1), Side::Right), Some((1, 1)));
            // Visible row 1 lands before 6, true row 3.
            assert_eq!(move_across(&view, f, id(2), Side::Right), Some((1, 3)));
            // Back left from 6 (visible row 1): `a` shows two cards, so before
            // its second.
            assert_eq!(move_across(&view, f, id(6), Side::Left), Some((0, 1)));
        });
    }

    #[test]
    fn across_is_a_no_op_at_both_edges() {
        let view = grid();
        with_filter("", |f| {
            assert_eq!(move_across(&view, f, id(1), Side::Left), None);
            assert_eq!(move_across(&view, f, id(5), Side::Right), None);
            // And a card that isn't on the board goes nowhere.
            assert_eq!(move_across(&view, f, id(9), Side::Right), None);
        });
    }

    #[test]
    fn within_down_hops_the_next_visible_card_past_a_hidden_one() {
        let view = board(vec![column(
            "a",
            vec![card_n(1, "keep"), card_n(2, "drop"), card_n(3, "keep")],
        )]);
        with_filter("keep", |f| {
            // Past 3 (true row 2), not merely past the hidden 2.
            assert_eq!(move_within(&view, f, id(1), Vertical::Down), Some((0, 3)));
            // And back up lands on 1's row, just before it.
            assert_eq!(move_within(&view, f, id(3), Vertical::Up), Some((0, 0)));
            // 3 is the last visible card.
            assert_eq!(move_within(&view, f, id(3), Vertical::Down), None);
        });
    }

    #[test]
    fn within_up_at_the_top_is_a_no_op() {
        let view = grid();
        with_filter("", |f| {
            assert_eq!(move_within(&view, f, id(1), Vertical::Up), None);
            assert_eq!(move_within(&view, f, id(2), Vertical::Up), Some((0, 0)));
        });
    }
}
