//! The board filter: parsing the header query into a [`CardFilter`], the
//! combined [`ViewFilter`] visibility predicate, and the cross-board reference
//! jump a pasted card ref triggers.

use super::{BoardNav, BoardUiState};
use crate::BoardSummary;
use crate::event::{self, BoardView, CardView};

/// A parsed board filter. Linear-inspired: whitespace splits the query into
/// AND-ed terms, a `label:` prefix scopes a term to labels, and everything else
/// is free text matched against a card's title, description, labels and its
/// word-id (`maple-river-canyon`), so a card can be pulled up by any of its id
/// words. A pasted reference to a card on *this* board
/// (`headway:headway/maple-river-canyon`, or the scheme-less
/// `headway/maple-river-canyon`) matches by its id words — the board segment
/// carries no signal within a single board. Other references are plain text: a
/// card citing an issue on another board stays searchable by that citation.
/// (Pasting another *known* board's reference switches to it — see
/// [`filter_ref_jump`].) All matching is case-insensitive substring; an empty
/// filter matches everything.
#[derive(Default)]
pub(crate) struct CardFilter {
    /// Free-text terms (lowercased); each must appear somewhere in the card.
    text: Vec<String>,
    /// `label:` terms (lowercased); each must match one of the card's labels.
    labels: Vec<String>,
}

impl CardFilter {
    /// Parse `query` into a filter for the board whose slug is `board` (needed
    /// to recognise pasted references to this board's own cards).
    pub(crate) fn parse(query: &str, board: &str) -> Self {
        let mut filter = CardFilter::default();
        for term in query.split_whitespace() {
            // Accept `label:` and `l:` as the label-scoping prefix.
            let label = term
                .strip_prefix("label:")
                .or_else(|| term.strip_prefix("l:"));
            match label {
                Some(value) if !value.is_empty() => filter.labels.push(value.to_lowercase()),
                // A bare `label:` with no value isn't a constraint; ignore it.
                Some(_) => {}
                None => match headway::wordid::parse_ref(term) {
                    // A reference to a card on *this* board — pasted in full
                    // (`headway:board/maple-river-canyon`) or as the scheme-less
                    // `board/maple-river-canyon` shorthand: the board segment is
                    // redundant here, so match on the id words alone.
                    Some((b, words)) if b.eq_ignore_ascii_case(board) => {
                        filter.text.push(words.to_lowercase());
                    }
                    // Anything else — a reference to another board, or plain text
                    // (including a bare word-id, which stays searchable via the
                    // haystack) — is search text.
                    _ => filter.text.push(term.to_lowercase()),
                },
            }
        }
        filter
    }

    /// Whether any constraint is set. An inactive filter shows the whole board.
    fn is_active(&self) -> bool {
        !self.text.is_empty() || !self.labels.is_empty()
    }

    /// Does `card` satisfy every term? Label terms must each match some label;
    /// text terms must each appear in the card's combined searchable text.
    fn matches(&self, card: &CardView) -> bool {
        let label_ok = self.labels.iter().all(|needle| {
            card.labels
                .iter()
                .any(|l| l.to_lowercase().contains(needle))
        });
        if !label_ok {
            return false;
        }
        if self.text.is_empty() {
            return true;
        }
        let mut haystack = card.title.to_lowercase();
        haystack.push('\n');
        haystack.push_str(&card.description.to_lowercase());
        for l in &card.labels {
            haystack.push('\n');
            haystack.push_str(&l.to_lowercase());
        }
        // The card's id words (`maple-river-canyon`), already lowercase BIP-39
        // words. No board slug: the filter is scoped to a single board, so the
        // slug carries no signal and would make its text match every card.
        haystack.push('\n');
        haystack.push_str(&headway::wordid::encode(card.id.bytes()));
        self.text.iter().all(|needle| haystack.contains(needle))
    }
}

/// The complete "what shows on the board grid" predicate: the text/label
/// [`CardFilter`] plus the board's view options (currently just
/// [`hide_subissues`](BoardUiState::hide_subissues)). Bundling them means every
/// visibility call site — the column drop zone, the per-column count badges and
/// the header summary — asks one question ([`shows`](Self::shows)) rather than
/// re-deriving the combination, and the "filtered" affordance keys off a single
/// [`is_active`](Self::is_active).
pub(crate) struct ViewFilter<'a> {
    /// The parsed text/label filter from the header field.
    pub(crate) filter: &'a CardFilter,
    /// Whether sub-issue cards are hidden from the grid.
    pub(crate) hide_subissues: bool,
}

impl ViewFilter<'_> {
    /// Whether the board is currently narrowing what it shows — a text/label
    /// filter, or a view option hiding some cards. Drives the header's
    /// "Filtered" affordance and the switch to a matched/total count.
    pub(crate) fn is_active(&self) -> bool {
        self.filter.is_active() || self.hide_subissues
    }

    /// Whether `card` should be drawn on the board grid: not hidden by a view
    /// option, and either the filter is inactive or the card matches it.
    pub(crate) fn shows(&self, card: &CardView) -> bool {
        if self.hide_subissues && card.parent.is_some() {
            return false;
        }
        !self.filter.is_active() || self.filter.matches(card)
    }
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
/// are the filter's business ([`CardFilter::parse`]), and board segments that
/// aren't a known board slug are plain search text; both return `None` here.
fn cross_board_ref<'a>(
    query: &'a str,
    board: &str,
    boards: &'a [BoardSummary],
) -> Option<CrossBoardRef<'a>> {
    query.split_whitespace().find_map(|term| {
        let (prefix, words) = headway::wordid::parse_ref(term)?;
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
    egui::Id::new("headway-filter-field")
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
    state.nav = Some(BoardNav::Switch(event::BoardCoord::new(r.owner, r.board)));
    state.filter = rewritten;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{BOARD, card};

    #[test]
    fn empty_filter_is_inactive_and_matches_all() {
        let f = CardFilter::parse("   ", BOARD);
        assert!(!f.is_active());
        assert!(f.matches(&card("anything", "", &[])));
    }

    #[test]
    fn text_terms_match_title_description_and_labels() {
        let c = card("Fix the bar", "wobbles on resize", &["ui"]);
        assert!(CardFilter::parse("bar", BOARD).matches(&c));
        assert!(CardFilter::parse("WOBBLES", BOARD).matches(&c)); // case-insensitive
        assert!(CardFilter::parse("ui", BOARD).matches(&c)); // also searches labels
        assert!(!CardFilter::parse("missing", BOARD).matches(&c));
    }

    #[test]
    fn multiple_text_terms_are_anded() {
        let c = card("Fix the bar", "wobbles on resize", &[]);
        assert!(CardFilter::parse("fix wobbles", BOARD).matches(&c));
        assert!(!CardFilter::parse("fix nope", BOARD).matches(&c));
    }

    #[test]
    fn label_token_scopes_to_labels_only() {
        let c = card("perf work", "", &["bug", "headway"]);
        assert!(CardFilter::parse("label:bug", BOARD).matches(&c));
        assert!(CardFilter::parse("l:head", BOARD).matches(&c)); // short prefix, substring
        // `perf` is in the title but not a label, so a label: term rejects it.
        assert!(!CardFilter::parse("label:perf", BOARD).matches(&c));
    }

    #[test]
    fn label_and_text_terms_combine() {
        let c = card("perf work", "", &["bug"]);
        assert!(CardFilter::parse("label:bug perf", BOARD).matches(&c));
        assert!(!CardFilter::parse("label:bug missing", BOARD).matches(&c));
    }

    #[test]
    fn bare_label_prefix_is_not_a_constraint() {
        let f = CardFilter::parse("label:", BOARD);
        assert!(!f.is_active());
        assert!(f.matches(&card("whatever", "", &[])));
    }

    /// The test card's id is all zeroes, which encodes to
    /// `abandon-abandon-abandon` (see [`headway::wordid`]).
    #[test]
    fn text_terms_match_word_id() {
        let c = card("Fix the bar", "", &[]);
        assert!(CardFilter::parse("abandon", BOARD).matches(&c));
        assert!(CardFilter::parse("abandon-abandon-abandon", BOARD).matches(&c));
        assert!(!CardFilter::parse("zoo", BOARD).matches(&c));
    }

    /// A pasted reference to a card on this board matches by its id words,
    /// whether it carries the `headway:` scheme (any casing) or is the scheme-less
    /// `board/word-id` shorthand. A bare word-id is plain search text, not a
    /// reference, but still matches via the haystack.
    #[test]
    fn own_board_reference_matches_by_id_words() {
        let c = card("Fix the bar", "", &[]);
        assert!(CardFilter::parse("headway:headway/abandon-abandon-abandon", BOARD).matches(&c));
        assert!(CardFilter::parse("headway/abandon-abandon-abandon", BOARD).matches(&c));
        assert!(CardFilter::parse("HEADWAY/abandon-abandon-abandon", BOARD).matches(&c));
        assert!(!CardFilter::parse("headway/zoo-zoo-zoo", BOARD).matches(&c));
        // A bare word-id is search text, and matches this card's id words.
        assert!(CardFilter::parse("abandon-abandon-abandon", BOARD).matches(&c));
    }

    /// A reference whose board isn't this board is plain search text: it finds
    /// cards that cite that reference, not this board's ids.
    #[test]
    fn foreign_reference_is_plain_text() {
        let citing = card("dup", "see other/abandon-abandon-abandon", &[]);
        let plain = card("dup", "", &[]);
        assert!(CardFilter::parse("other/abandon-abandon-abandon", BOARD).matches(&citing));
        assert!(!CardFilter::parse("other/abandon-abandon-abandon", BOARD).matches(&plain));
    }

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
