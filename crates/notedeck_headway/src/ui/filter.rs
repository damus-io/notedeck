//! The board filter: parsing the header query into a
//! [`headway::query::IssueFilter`], the combined [`ViewFilter`] visibility
//! predicate, and the cross-board reference
//! jump a pasted card ref triggers.

use super::{BoardEffect, BoardNav, BoardUiState};
use crate::BoardSummary;
use crate::event::{self, BoardView, CardView, ColumnView};
use headway::query::{IssueFilter, ListRow};

/// The complete "what shows on the board grid" predicate: the board's parsed
/// header query — a [`headway::query::IssueFilter`], the same parser and
/// predicate the list view and `headway list` use, with the board's view
/// options ([`hide_subissues`](BoardUiState::hide_subissues)) folded in — and
/// the board it reads, which column-aware terms (`is:done`, `col:todo`) need.
/// Bundling them means every visibility call site — the column drop zone, the
/// per-column count badges and the header summary — asks one question
/// ([`shows`](Self::shows)) rather than re-deriving the combination, and the
/// "filtered" affordance keys off a single [`is_active`](Self::is_active).
pub(crate) struct ViewFilter<'a> {
    /// The board the grid draws.
    pub(crate) view: &'a BoardView,
    /// The parsed header query plus view options.
    pub(crate) filter: &'a IssueFilter,
}

impl ViewFilter<'_> {
    /// Whether the board is currently narrowing what it shows — a query term,
    /// or a view option hiding some cards. Drives the header's "Filtered"
    /// affordance and the switch to a matched/total count.
    pub(crate) fn is_active(&self) -> bool {
        self.filter.is_active()
    }

    /// Whether `card`, sitting in `column`, should be drawn on the board grid.
    pub(crate) fn shows(&self, column: &ColumnView, card: &CardView) -> bool {
        self.filter.matches(&ListRow {
            board: self.view,
            card,
            column,
        })
    }
}

/// Parse the board filter field's `query` for `view`, with the board's view
/// options folded in — the [`IssueFilter`] a [`ViewFilter`] borrows.
pub(crate) fn parse_board_filter(
    query: &str,
    view: &BoardView,
    hide_subissues: bool,
) -> IssueFilter {
    let mut filter = IssueFilter::parse(query, Some(&view.id));
    filter.hide_subissues = hide_subissues;
    filter
}

/// A full reference to a card on *another* board found in the filter query:
/// the raw term as typed, the id words after the board segment, and the
/// referenced board's slug. Borrowed from the query and the board list.
struct CrossBoardRef<'a> {
    /// The whole term as it appears in the query
    /// (`headway:otherboard/maple-river-canyon` or `otherboard/maple-river-canyon`).
    term: &'a str,
    /// The id words after the board segment (`maple-river-canyon`).
    words: &'a str,
    /// The referenced board's slug, in its canonical casing from the board list.
    board: &'a str,
    /// The referenced board's owner, so the jump targets its full coordinate.
    owner: [u8; 32],
}

/// Find the first term in `query` that is a reference to a card on another
/// known board (`headway:otherboard/maple-river-canyon`, or the scheme-less
/// `otherboard/maple-river-canyon` shorthand). Terms referencing `board` itself
/// are the filter's business ([`IssueFilter::parse`]), and board segments that
/// aren't a known board slug are plain search text; both return `None` here.
fn cross_board_ref<'a>(
    query: &'a str,
    board: &str,
    boards: &'a [BoardSummary],
) -> Option<CrossBoardRef<'a>> {
    query.split_whitespace().find_map(|term| {
        let headway::wordid::CardRef {
            board: prefix,
            words,
        } = headway::wordid::parse_ref(term)?;
        if prefix.eq_ignore_ascii_case(board) {
            return None;
        }
        let target = boards.iter().find(|b| b.id.eq_ignore_ascii_case(prefix))?;
        Some(CrossBoardRef {
            term,
            words,
            board: &target.id,
            owner: target.owner,
        })
    })
}

/// The header filter field's pinned id, shared by the field itself and the `/`
/// key that focuses it (see [`crate::keys`]).
pub(crate) fn filter_field_id() -> egui::Id {
    egui::Id::unique("headway-filter-field")
}

/// React to a full cross-board reference pasted into the filter field: a
/// `headway:otherboard/maple-river-canyon` term addresses a card on another
/// board, so the intuitive read is "take me there" — raise a switch to that board and
/// reduce the term to its id words, which then filter the target board down to
/// the referenced card. Runs only on an edit of the field, not per frame.
pub(super) fn filter_ref_jump(view: &BoardView, boards: &[BoardSummary], state: &mut BoardUiState) {
    let Some(r) = cross_board_ref(&state.filter, &view.id, boards) else {
        return;
    };
    let rewritten = state
        .filter
        .split_whitespace()
        .map(|t| if t == r.term { r.words } else { t })
        .collect::<Vec<_>>()
        .join(" ");
    state.raise(BoardEffect::Nav(BoardNav::Switch(event::BoardCoord::new(
        r.owner, r.board,
    ))));
    state.filter = rewritten;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cross_board_ref_finds_only_known_foreign_boards() {
        let boards = vec![
            BoardSummary {
                owner: [1u8; 32],
                id: "headway".into(),
                title: "Headway".into(),
            },
            BoardSummary {
                owner: [2u8; 32],
                id: "notebook".into(),
                title: "Notebook".into(),
            },
        ];
        // A known foreign board's reference is found, slug in canonical casing.
        let r = cross_board_ref("bug NOTEBOOK/maple-river-canyon", "headway", &boards)
            .expect("foreign ref");
        assert_eq!(r.term, "NOTEBOOK/maple-river-canyon");
        assert_eq!(r.words, "maple-river-canyon");
        assert_eq!(r.board, "notebook");
        // The full scheme form is found too.
        assert_eq!(
            cross_board_ref("headway:notebook/maple-river-canyon", "headway", &boards)
                .expect("scheme ref")
                .board,
            "notebook"
        );
        // Own-board and unknown-board terms are not jumps.
        assert!(cross_board_ref("headway/maple-river-canyon", "headway", &boards).is_none());
        assert!(cross_board_ref("other/maple-river-canyon c/x", "headway", &boards).is_none());
        assert!(cross_board_ref("no refs here", "headway", &boards).is_none());
    }
}
