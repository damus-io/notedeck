//! Headway's answer to the chrome's universal search (the Cmd-K bar, see
//! [`notedeck::search`]): boards by title or slug, and cards across every
//! board the switcher lists, own and shared.
//!
//! Cards are matched by the issue list's own query model,
//! [`IssueFilter::parse`] unscoped, so a query means what it means in the
//! list: `p:high label:ui relay` narrows by priority and label and finds
//! `relay` in a title, description, label or word-id, a pasted
//! `headway:<board>/<word-id>` finds exactly that card, and archived cards
//! come only for `is:archived`. Boards match a query of free text alone, by
//! title or slug.
//!
//! Hits rank by the shared [`fuzzy_match`] of the free text on the title, a
//! hit matched only by its body or a keyed term scoring zero, then by how
//! recently the thing changed. Each opens through a `nostr:` reference to its
//! event (an `nevent` for a card, an `naddr` for a board), which the built-in
//! nostr parser resolves for a shared board's card as readily as an own one's;
//! the chrome then routes the note to Headway the way a click on its inline
//! chip is routed.

use headway::query::{self, IssueFilter, ListQuery};
use notedeck::fuzzy::fuzzy_match;
use notedeck::{OpenUri, SearchHit, SearchHits, SearchQuery};

use crate::cache::BoardRef;
use crate::ui::{board_nostr_uri, issue_nostr_uri};

/// What a board hit is marked as in the bar.
const KIND_BOARD: &str = "Board";

/// What a card hit is marked as in the bar.
const KIND_CARD: &str = "Card";

/// What a hit is, by index into the boards searched: lifetime-free, so every
/// match can be ranked before the few that make the cap are built.
#[derive(Clone, Copy)]
enum Found {
    /// The board at this index.
    Board(usize),
    /// This card.
    Card(query::RowPos),
}

/// One match before the cut to the provider's cap, and what it ranks by.
struct Ranked {
    /// What matched.
    found: Found,
    /// How well, on [`fuzzy_match`]'s scale (see the module docs).
    score: i32,
    /// When it last changed, breaking ties in `score`, newest first.
    updated: u64,
}

/// Answer `query` from `boards`, the folded boards the switcher lists
/// (deleted ones already dropped), into `out`, best first, up to its cap.
#[profiling::function]
pub(crate) fn search(boards: &[BoardRef], query: &SearchQuery, out: &mut SearchHits) {
    let filter = IssueFilter::parse(query.text, None);
    // The fuzzy half reads only the free text: a keyed term (`p:high`) is a
    // constraint, not letters a title should hold.
    let text = filter.text.join(" ");
    let mut positions = Vec::new();
    let mut ranked: Vec<Ranked> = Vec::new();

    // A keyed term or a pasted reference is about cards, so it keeps boards
    // out.
    if !text.is_empty() && filter.is_text_only() {
        for (at, board) in boards.iter().enumerate() {
            let title = fuzzy_match(&text, &board.title, &mut positions);
            let slug = fuzzy_match(&text, &board.id, &mut positions);
            let Some(score) = title.max(slug) else {
                continue;
            };
            ranked.push(Ranked {
                found: Found::Board(at),
                score,
                updated: board.created_at,
            });
        }
    }

    let list = ListQuery {
        filter,
        ..ListQuery::default()
    };
    for pos in query::list_positions(boards.iter().map(|b| &**b), &list) {
        let card = pos.slot.card(&boards[pos.board]);
        ranked.push(Ranked {
            found: Found::Card(pos),
            score: fuzzy_match(&text, &card.title, &mut positions).unwrap_or(0),
            updated: card.updated_at,
        });
    }

    ranked.sort_by(|a, b| b.score.cmp(&a.score).then(b.updated.cmp(&a.updated)));
    for r in ranked {
        if out.is_full() {
            break;
        }
        if let Some(hit) = hit(boards, r) {
            out.push(hit);
        }
    }
}

/// The bar's row for `r`: titled as the thing is, marked with what it is,
/// and opening its event. `None` only when its id won't encode as a `nostr:`
/// reference.
fn hit(boards: &[BoardRef], r: Ranked) -> Option<SearchHit> {
    match r.found {
        Found::Board(at) => {
            let board = &boards[at];
            let uri = board_nostr_uri(&board.author, &board.id)?;
            Some(
                SearchHit::new(board.title.clone(), OpenUri::new(uri), r.score)
                    .kind(KIND_BOARD)
                    .subtitled(board.id.clone()),
            )
        }
        Found::Card(pos) => {
            let row = pos.row(&boards[pos.board]);
            let uri = issue_nostr_uri(&row.card.id)?;
            Some(
                SearchHit::new(row.card.title.clone(), OpenUri::new(uri), r.score)
                    .kind(KIND_CARD)
                    .subtitled(format!("{} · {}", row.board.title, row.place_name())),
            )
        }
    }
}
