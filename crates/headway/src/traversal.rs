//! Work-order traversal: the read-model an AI (or the GUI) consumes to decide
//! what to do next.
//!
//! Everything here is a pure function over the already-folded [`BoardView`], the
//! same shape the reducer in [`crate::event`] produces. Keeping the traversal
//! pure means the `next` CLI, the GUI, and unit tests all share one definition of
//! "work-order" with no persistence in the loop.
//!
//! Two views over a [`Container`] (the board root or a parent card):
//! - [`work_order`] — the full depth-first order of a container's members and
//!   their subtrees, the list you'd render to inspect "everything, in order".
//! - [`ready`] — the frontier of that order that is *workable right now*: not
//!   done, not blocked, and not merely a parent whose real work is its children.
//!   `next` is `ready(..).first()`; `next -n k` is `ready(..).take(k)`.
//!
//! Ordering everywhere is the settled fallback `order = (seq if set, else
//! created_at)`: explicitly sequenced members lead, in rank order, and
//! unsequenced members follow in creation order. For a card container that order
//! is already baked into [`CardView::subissues`] by the reducer, so this module
//! reuses it rather than re-deriving the fallback (a card's subissues don't even
//! carry `created_at` here — the reducer is the single place that knows it).

use std::collections::HashSet;

use enostr::NoteId;

use crate::event::{BoardView, CardView, Container};

/// The depth-first work-order for `container`: each member in `(seq else
/// created_at)` order, immediately followed by its own subtree (pre-order), for
/// leaves and branches alike. The container itself is not included — only its
/// members and their descendants.
///
/// Returns borrows into `view`, so callers that need owned cards clone at the
/// edge. A member that has no live [`CardView`] on the board (unplaced or
/// archived) is skipped, as is any card reached twice, so a subissue cycle that
/// slipped past the write-time guard terminates instead of looping forever.
pub fn work_order<'v>(view: &'v BoardView, container: &Container) -> Vec<&'v CardView> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk(view, container, &mut visited, &mut out);
    out
}

/// The ready frontier of `container`'s [`work_order`]: the members that can be
/// picked up *now*, in work-order. More than one card can be ready at once — that
/// is the parallel-dispatch signal an AI acts on. A card is ready when it is:
/// - not done (not sitting in its board's last column), and
/// - not blocked ([`is_blocked`] — no dependency edge points at an unfinished
///   blocker), and
/// - not a parent with unfinished subissues: its real work is those children,
///   which are already in the frontier, so the branch itself isn't dispatchable.
pub fn ready<'v>(view: &'v BoardView, container: &Container) -> Vec<&'v CardView> {
    work_order(view, container)
        .into_iter()
        .filter(|card| !is_done(view, card.id))
        .filter(|card| !is_blocked(view, card))
        .filter(|card| !has_open_subissue(card))
        .collect()
}

/// Depth-first pre-order walk shared by [`work_order`]. Pushes each member of
/// `container` (in work-order) then recurses into that member as its own card
/// container.
fn walk<'v>(
    view: &'v BoardView,
    container: &Container,
    visited: &mut HashSet<[u8; 32]>,
    out: &mut Vec<&'v CardView>,
) {
    for id in member_order(view, container) {
        if !visited.insert(*id.bytes()) {
            continue;
        }
        let Some(card) = view.card(id) else {
            continue;
        };
        out.push(card);
        walk(view, &Container::Card(*id.bytes()), visited, out);
    }
}

/// The ids of `container`'s direct members, in work-order.
///
/// A card container's members are its subissues, which the reducer has already
/// sorted into work-order (sequenced by rank, then unsequenced by created_at) —
/// reused verbatim. The board root's members are its top-level (non-subissue)
/// cards, which the reducer sorts spatially by column, so this applies the
/// `(seq else created_at)` fallback here, the one place it must.
fn member_order(view: &BoardView, container: &Container) -> Vec<NoteId> {
    match container {
        Container::Card(parent) => view
            .card(NoteId::new(*parent))
            .map(|c| c.subissues.iter().map(|s| s.id).collect())
            .unwrap_or_default(),
        Container::BoardRoot(_) => {
            let mut top: Vec<&CardView> = view
                .columns
                .iter()
                .flat_map(|col| col.cards.iter())
                .filter(|c| c.parent.is_none())
                .collect();
            top.sort_by_cached_key(|c| {
                (
                    c.seq.is_none(),
                    c.seq.clone().unwrap_or_default(),
                    c.created_at,
                    *c.id.bytes(),
                )
            });
            top.into_iter().map(|c| c.id).collect()
        }
    }
}

/// A card is done when it sits in its board's last column (the terminal
/// "Done"-style column). Positional, mirroring [`crate::event::SubissueView::done`]
/// — there is no stored done flag.
fn is_done(view: &BoardView, id: NoteId) -> bool {
    view.columns
        .last()
        .is_some_and(|last| last.cards.iter().any(|c| c.id == id))
}

/// Does `card` have at least one subissue still to do? Such a card is a branch
/// whose work lives in its children, so it is not itself part of the ready
/// frontier. Uses the reducer's positional [`crate::event::SubissueView::done`]
/// and skips archived children.
fn has_open_subissue(card: &CardView) -> bool {
    card.subissues.iter().any(|s| !s.done && !s.archived)
}

/// Blocking seam: is `card` held back by an unfinished blocker? A card is blocked
/// while any of its dependency edges points at a card that isn't cleared (done or
/// archived) — the reducer resolves that per-edge state, so this is a pure read
/// (see [`CardView::is_blocked`]). Inheriting an ancestor container's blockers is
/// a future refinement; today only a card's own edges hold it back.
fn is_blocked(_view: &BoardView, card: &CardView) -> bool {
    card.is_blocked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ArchivedCard, CardView, ColumnView, Priority, SubissueView};

    /// Build a minimal [`CardView`] carrying only the fields traversal reads
    /// (id, seq, created_at, parent, subissues); the rest get inert defaults so
    /// the tests stay focused on ordering and the ready frontier.
    fn card(n: u8, seq: Option<&str>, created_at: u64) -> CardView {
        CardView {
            id: NoteId::new([n; 32]),
            author: [0; 32],
            title: format!("card {n}"),
            description: String::new(),
            labels: vec![],
            priority: Priority::None,
            due: None,
            estimate: None,
            rank: "m".to_string(),
            seq: seq.map(str::to_string),
            placed_at: 0,
            created_at,
            updated_at: created_at,
            comments: vec![],
            activity: vec![],
            parent: None,
            subissues: vec![],
            blocked_by: vec![],
            blocks: vec![],
            related: vec![],
        }
    }

    /// A resolved blocker edge as the reducer would attach it: `done` marks the
    /// blocker cleared (so it no longer holds the card back).
    fn blocker(n: u8, done: bool) -> crate::event::EdgeRef {
        crate::event::EdgeRef {
            id: NoteId::new([n; 32]),
            title: format!("card {n}"),
            done,
        }
    }

    /// A subissue reference as the reducer would attach it to a parent, in the
    /// order given.
    fn sub(n: u8, done: bool) -> SubissueView {
        SubissueView {
            id: NoteId::new([n; 32]),
            title: format!("card {n}"),
            column: Some(if done { "done" } else { "backlog" }.to_string()),
            done,
            archived: false,
            seq: None,
        }
    }

    /// Assemble a board of `backlog` + `done` columns from the given cards; a
    /// card is placed in `done` (the last column) iff its id is in `done_ids`.
    fn board(cards: Vec<CardView>, done_ids: &[u8]) -> BoardView {
        let mut backlog = vec![];
        let mut done = vec![];
        for c in cards {
            if done_ids.contains(&c.id.bytes()[0]) {
                done.push(c);
            } else {
                backlog.push(c);
            }
        }
        BoardView {
            id: "b".to_string(),
            author: [0; 32],
            title: "b".to_string(),
            description: String::new(),
            created_at: 0,
            columns: vec![
                ColumnView {
                    id: "backlog".to_string(),
                    name: "Backlog".to_string(),
                    cards: backlog,
                },
                ColumnView {
                    id: "done".to_string(),
                    name: "Done".to_string(),
                    cards: done,
                },
            ],
            archived: Vec::<ArchivedCard>::new(),
        }
    }

    fn ids(cards: &[&CardView]) -> Vec<u8> {
        cards.iter().map(|c| c.id.bytes()[0]).collect()
    }

    #[test]
    fn unsequenced_falls_back_to_creation_order() {
        // No seq on any card: work-order is creation order, so a later-created
        // card sorts behind an earlier one regardless of insertion order.
        let view = board(
            vec![card(3, None, 30), card(1, None, 10), card(2, None, 20)],
            &[],
        );
        let order = work_order(&view, &Container::BoardRoot("b".to_string()));
        assert_eq!(ids(&order), [1, 2, 3]);
    }

    #[test]
    fn sequenced_lead_then_unsequenced_by_created_at() {
        // Two sequenced cards lead in rank order (b before c even though 'c' was
        // created first), then the unsequenced card by created_at.
        let view = board(
            vec![
                card(1, None, 5),
                card(2, Some("c"), 99),
                card(3, Some("b"), 1),
            ],
            &[],
        );
        let order = work_order(&view, &Container::BoardRoot("b".to_string()));
        assert_eq!(ids(&order), [3, 2, 1]);
    }

    #[test]
    fn dfs_descends_into_subissues_in_parent_order() {
        // Parent 1 owns children 2 then 3 (the reducer's subissue order); the
        // DFS emits the parent then its children before moving on to card 4.
        let mut parent = card(1, Some("a"), 1);
        parent.subissues = vec![sub(2, false), sub(3, false)];
        let child2 = {
            let mut c = card(2, None, 2);
            c.parent = Some(parent.id);
            c
        };
        let child3 = {
            let mut c = card(3, None, 3);
            c.parent = Some(parent.id);
            c
        };
        let other = card(4, Some("b"), 4);
        let view = board(vec![parent, child2, child3, other], &[]);

        let order = work_order(&view, &Container::BoardRoot("b".to_string()));
        assert_eq!(ids(&order), [1, 2, 3, 4]);
    }

    #[test]
    fn ready_excludes_done_and_parents_with_open_children() {
        // Card 1 is a parent with an open child (2); card 4 is done. Ready keeps
        // the atomic, not-done work: the open child and the standalone card 3,
        // and drops the branch parent and the done card.
        let mut parent = card(1, Some("a"), 1);
        parent.subissues = vec![sub(2, false)];
        let child = {
            let mut c = card(2, None, 2);
            c.parent = Some(parent.id);
            c
        };
        let standalone = card(3, Some("b"), 3);
        let finished = card(4, Some("c"), 4);
        let view = board(vec![parent, child, standalone, finished], &[4]);

        let root = Container::BoardRoot("b".to_string());
        assert_eq!(ids(&work_order(&view, &root)), [1, 2, 3, 4]);
        assert_eq!(ids(&ready(&view, &root)), [2, 3]);
    }

    #[test]
    fn ready_of_a_card_container_is_its_subtree() {
        // Traversing a parent container yields its subissues (not the parent),
        // and ready keeps the not-done ones.
        let mut parent = card(1, None, 1);
        parent.subissues = vec![sub(2, false), sub(3, true)];
        let child_open = {
            let mut c = card(2, None, 2);
            c.parent = Some(parent.id);
            c
        };
        let child_done = {
            let mut c = card(3, None, 3);
            c.parent = Some(parent.id);
            c
        };
        let view = board(vec![parent, child_open, child_done], &[3]);

        let container = Container::Card([1; 32]);
        assert_eq!(ids(&work_order(&view, &container)), [2, 3]);
        assert_eq!(ids(&ready(&view, &container)), [2]);
    }

    #[test]
    fn is_blocked_tracks_unfinished_blocker_edges() {
        // No edges: not blocked. An unfinished blocker edge holds the card back;
        // a cleared (done) one does not.
        let mut plain = card(1, None, 1);
        assert!(!is_blocked(&board(vec![plain.clone()], &[]), &plain));

        plain.blocked_by = vec![blocker(2, false)];
        assert!(is_blocked(&board(vec![plain.clone()], &[]), &plain));

        plain.blocked_by = vec![blocker(2, true)];
        assert!(!is_blocked(&board(vec![plain.clone()], &[]), &plain));

        // Mixed: one still-open blocker is enough to block.
        plain.blocked_by = vec![blocker(2, true), blocker(3, false)];
        assert!(is_blocked(&board(vec![plain.clone()], &[]), &plain));
    }

    #[test]
    fn ready_excludes_blocked_cards() {
        // Card 2 is blocked by an unfinished card 3; the ready frontier skips it
        // even though it isn't done and has no open subissues.
        let mut blocked = card(2, Some("b"), 2);
        blocked.blocked_by = vec![blocker(3, false)];
        let view = board(
            vec![card(1, Some("a"), 1), blocked, card(3, Some("c"), 3)],
            &[],
        );
        let root = Container::BoardRoot("b".to_string());
        assert_eq!(ids(&ready(&view, &root)), [1, 3]);
    }
}
