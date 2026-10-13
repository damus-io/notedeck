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
//!   done, not already taken (In Progress), not blocked, and not merely a parent
//!   whose real work is its children.
//!   `next` is `ready(..).first()`; `next -n k` is `ready(..).take(k)`;
//!   `next --label l` is [`ready_labelled`].
//! - [`place`] — where one card sits in that order, and how much of the
//!   frontier is ahead of it (`seq <card>` with no position flag).
//!
//! Ordering everywhere is the settled fallback `order = (seq if set, else
//! created_at)`: explicitly sequenced members lead, in rank order, and
//! unsequenced members follow in creation order. For a card container that order
//! is already baked into [`CardView::subissues`] by the reducer, so this module
//! reuses it rather than re-deriving the fallback (a card's subissues don't even
//! carry `created_at` here — the reducer is the single place that knows it).

use std::collections::HashSet;

use nostrdb_net::NoteId;

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
/// - not done ([`is_done`] — not in a terminal column, and not a parent whose
///   live subissues are all done: a finished epic is never handed out), and
/// - not In Progress ([`BoardView::card_is_in_progress`]) — someone is already
///   on it, so handing it out would start a second worker on the same card, and
/// - not blocked ([`open_blocker`] — no dependency edge points at an unfinished
///   blocker), and
/// - not a parent with unfinished subissues: its real work is those children,
///   which are already in the frontier, so the branch itself isn't dispatchable.
pub fn ready<'v>(view: &'v BoardView, container: &Container) -> Vec<&'v CardView> {
    work_order(view, container)
        .into_iter()
        .filter(|card| not_ready(view, card).is_none())
        .collect()
}

/// Why a card is left out of the [`ready`] frontier: the first of `ready`'s
/// rules it fails, checked in the same order `ready` lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReady {
    /// In a terminal column, or a parent whose live subissues are all done.
    Done,
    /// In the In Progress column: someone already has it.
    InProgress,
    /// Held back by an unfinished blocker; `by` is the first such edge.
    Blocked { by: NoteId },
    /// A parent with subissues still to do — its work is those children.
    OpenSubissues,
}

/// The one predicate behind [`ready`]: `None` when `card` is workable now,
/// else the reason it isn't. [`place`] counts with it too, so a card's
/// "ready ahead" count can never disagree with what `next` hands out.
pub fn not_ready(view: &BoardView, card: &CardView) -> Option<NotReady> {
    if is_done(view, card.id) {
        return Some(NotReady::Done);
    }
    if view.card_is_in_progress(card.id) {
        return Some(NotReady::InProgress);
    }
    if let Some(by) = open_blocker(view, card) {
        return Some(NotReady::Blocked { by });
    }
    if has_open_subissue(card) {
        return Some(NotReady::OpenSubissues);
    }
    None
}

/// Where a card sits in a container's [`work_order`]: the answer to `headway
/// seq <card>` with no position flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// 1-based position in the work-order walk.
    pub index: usize,
    /// How many cards the walk has in all.
    pub of: usize,
    /// The card carries an explicit rank within its own parent (the board root
    /// for a top-level card). `false` means it is placed by creation time,
    /// behind every sequenced sibling.
    pub sequenced: bool,
    /// The card just ahead of it in the walk, `None` when it is first.
    pub before: Option<NoteId>,
    /// The card just behind it in the walk, `None` when it is last.
    pub after: Option<NoteId>,
    /// How many cards ahead of it are in the [`ready`] frontier — what `next`
    /// would hand out before reaching it.
    pub ready_ahead: usize,
    /// Why the card itself is not ready, `None` when it is.
    pub not_ready: Option<NotReady>,
}

impl Place {
    /// The card is in the ready frontier itself.
    pub fn ready(&self) -> bool {
        self.not_ready.is_none()
    }
}

/// Where `card` sits in `container`'s [`work_order`], or `None` when the walk
/// never reaches it (it isn't under that container, or isn't live on the
/// board). One walk, counted with the same [`not_ready`] predicate [`ready`]
/// filters on.
pub fn place(view: &BoardView, container: &Container, card: NoteId) -> Option<Place> {
    let order = work_order(view, container);
    let at = order.iter().position(|c| c.id == card)?;
    let target = order[at];
    let ready_ahead = order[..at]
        .iter()
        .filter(|c| not_ready(view, c).is_none())
        .count();
    Some(Place {
        index: at + 1,
        of: order.len(),
        sequenced: is_sequenced(view, target),
        before: at.checked_sub(1).map(|i| order[i].id),
        after: order.get(at + 1).map(|c| c.id),
        ready_ahead,
        not_ready: not_ready(view, target),
    })
}

/// Does `card` carry an explicit work-order rank within its own parent? A
/// subissue's rank lives on its parent's [`crate::event::SubissueView::seq`]; a
/// top-level card's on its own [`CardView::seq`] (the board root's order).
fn is_sequenced(view: &BoardView, card: &CardView) -> bool {
    let Some(parent) = card.parent else {
        return card.seq.is_some();
    };
    view.card(parent)
        .and_then(|p| p.subissues.iter().find(|s| s.id == card.id))
        .is_some_and(|s| s.seq.is_some())
}

/// [`ready`] narrowed to the cards that carry every label in `labels` (all of
/// them; an empty `labels` keeps the whole frontier). A filter over the frontier
/// only: an unlabelled card still blocks, and still counts toward its parent's
/// doneness, exactly as it would without the filter. Backs `next --label`, so a
/// role lane picks its next card without hand-filtering the board.
pub fn ready_labelled<'v>(
    view: &'v BoardView,
    container: &Container,
    labels: &[String],
) -> Vec<&'v CardView> {
    let mut frontier = ready(view, container);
    frontier.retain(|card| labels.iter().all(|l| card.labels.contains(l)));
    frontier
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
    for id in members(view, container) {
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
pub fn members(view: &BoardView, container: &Container) -> Vec<NoteId> {
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

/// A card is done when it sits in one of its board's *terminal* ("Done"-style)
/// columns, or when it has at least one live subissue and every one of them is
/// done — recursively, so an epic of finished sub-epics is done too. Mirrors
/// [`crate::event::SubissueView::done`]; there is no stored done flag. A board
/// that marks no terminal column falls back to its last column (see
/// [`crate::event::column_is_terminal`]).
fn is_done(view: &BoardView, id: NoteId) -> bool {
    view.card_is_done(id)
}

/// Does `card` have at least one subissue still to do? Such a card is a branch
/// whose work lives in its children, so it is not itself part of the ready
/// frontier. Uses the reducer's rolled-up [`crate::event::SubissueView::done`]
/// and skips archived children.
fn has_open_subissue(card: &CardView) -> bool {
    card.subissues.iter().any(|s| !s.done && !s.archived)
}

/// Blocking seam: the first unfinished blocker holding `card` back, if any. A
/// card is blocked while any of its dependency edges points at a card that isn't
/// cleared (done or archived) — the reducer resolves that per-edge state, so this
/// is a pure read (see [`CardView::is_blocked`]). Inheriting an ancestor
/// container's blockers is a future refinement; today only a card's own edges
/// hold it back.
fn open_blocker(_view: &BoardView, card: &CardView) -> Option<NoteId> {
    card.blocked_by.iter().find(|b| !b.done).map(|b| b.id)
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
            reviews: vec![],
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
                    terminal: false,
                    cards: backlog,
                },
                ColumnView {
                    id: "done".to_string(),
                    name: "Done".to_string(),
                    terminal: false,
                    cards: done,
                },
            ],
            archived: Vec::<ArchivedCard>::new(),
            deleted: false,
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
    fn ready_excludes_a_parent_whose_subissues_are_all_done() {
        // Card 1 sits in the non-terminal backlog, but both its subissues are
        // done: it is done itself, so it is never handed out as work — and with
        // its children done too, nothing in the subtree is ready.
        let mut parent = card(1, Some("a"), 1);
        parent.subissues = vec![sub(2, true), sub(3, true)];
        let children = [2, 3].map(|n| {
            let mut c = card(n, None, n as u64);
            c.parent = Some(parent.id);
            c
        });
        let standalone = card(4, Some("b"), 4);
        let [c2, c3] = children;
        let view = board(vec![parent, c2, c3, standalone], &[2, 3]);

        assert!(view.card_is_done(NoteId::new([1; 32])));
        let root = Container::BoardRoot("b".to_string());
        assert_eq!(ids(&ready(&view, &root)), [4]);
    }

    #[test]
    fn ready_keeps_leaves_with_no_live_subissues() {
        // Card 1 has no subissues and card 2 only an archived one: both are
        // leaves under the positional rule, so — sitting in backlog — both are
        // ready. "All subissues done" over an empty set must not finish a card.
        let mut only_archived = card(2, Some("b"), 2);
        only_archived.subissues = vec![SubissueView {
            archived: true,
            column: None,
            ..sub(3, true)
        }];
        let view = board(vec![card(1, Some("a"), 1), only_archived], &[]);

        assert!(!view.card_is_done(NoteId::new([1; 32])));
        assert!(!view.card_is_done(NoteId::new([2; 32])));
        let root = Container::BoardRoot("b".to_string());
        assert_eq!(ids(&ready(&view, &root)), [1, 2]);
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
    fn open_blocker_tracks_unfinished_blocker_edges() {
        // No edges: not blocked. An unfinished blocker edge holds the card back;
        // a cleared (done) one does not.
        let blocked = |edges: Vec<crate::event::EdgeRef>| {
            let mut plain = card(1, None, 1);
            plain.blocked_by = edges;
            let view = board(vec![plain], &[]);
            open_blocker(&view, &view.columns[0].cards[0]).is_some()
        };
        assert!(!blocked(vec![]));
        assert!(blocked(vec![blocker(2, false)]));
        assert!(!blocked(vec![blocker(2, true)]));

        // Mixed: one still-open blocker is enough to block.
        assert!(blocked(vec![blocker(2, true), blocker(3, false)]));
    }

    #[test]
    fn ready_excludes_in_progress_cards() {
        // Cards 1 and 2 sit side by side, neither done nor blocked; card 1 is in
        // the In Progress column, so someone already has it. Only card 2 (still
        // in backlog) is handed out. The column is matched by id, and a board
        // whose column id differs still matches it by name.
        for (id, name) in [("in-progress", "Doing"), ("wip", "In-Progress")] {
            let mut view = board(vec![card(1, Some("a"), 1), card(2, Some("b"), 2)], &[]);
            let taken = view.columns[0].cards.remove(0);
            view.columns.insert(
                1,
                ColumnView {
                    id: id.to_string(),
                    name: name.to_string(),
                    terminal: false,
                    cards: vec![taken],
                },
            );
            let root = Container::BoardRoot("b".to_string());
            assert_eq!(ids(&work_order(&view, &root)), [1, 2]);
            assert_eq!(ids(&ready(&view, &root)), [2], "column {id}/{name}");
        }
    }

    #[test]
    fn ready_labelled_keeps_only_cards_with_every_label() {
        // Cards 1 and 3 carry `role:model`, card 3 also `ui`; card 2 carries
        // neither. Card 4 has the label but is blocked by unlabelled card 2: the
        // filter narrows the frontier, it doesn't make 2 stop blocking.
        let labelled = |n, labels: &[&str]| {
            let mut c = card(n, None, n as u64);
            c.labels = labels.iter().map(|l| l.to_string()).collect();
            c
        };
        let mut blocked = labelled(4, &["role:model"]);
        blocked.blocked_by = vec![blocker(2, false)];
        let view = board(
            vec![
                labelled(1, &["role:model"]),
                labelled(2, &[]),
                labelled(3, &["role:model", "ui"]),
                blocked,
            ],
            &[],
        );
        let root = Container::BoardRoot("b".to_string());
        let want = |ls: &[&str]| -> Vec<String> { ls.iter().map(|l| l.to_string()).collect() };

        assert_eq!(ids(&ready_labelled(&view, &root, &[])), [1, 2, 3]);
        assert_eq!(
            ids(&ready_labelled(&view, &root, &want(&["role:model"]))),
            [1, 3]
        );
        assert_eq!(
            ids(&ready_labelled(&view, &root, &want(&["role:model", "ui"]))),
            [3]
        );
        assert!(ready_labelled(&view, &root, &want(&["nope"])).is_empty());
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

    /// A board root walk of six top-level cards plus one subissue, with a
    /// blocked card and an In Progress card ahead of the unsequenced card 5:
    /// `1 2(blocked) 3(in progress) 4 5 6 8(child of 6)`.
    fn place_fixture() -> BoardView {
        let mut blocked = card(2, Some("b"), 2);
        blocked.blocked_by = vec![blocker(9, false)];
        let mut parent = card(6, None, 60);
        parent.subissues = vec![SubissueView {
            seq: Some("a".to_string()),
            ..sub(8, false)
        }];
        let mut child = card(8, None, 80);
        child.parent = Some(parent.id);
        let mut view = board(
            vec![
                card(1, Some("a"), 1),
                blocked,
                card(3, Some("c"), 3),
                card(4, Some("d"), 4),
                card(5, None, 50),
                parent,
                child,
            ],
            &[],
        );
        let at = view.columns[0]
            .cards
            .iter()
            .position(|c| c.id.bytes()[0] == 3)
            .unwrap();
        let taken = view.columns[0].cards.remove(at);
        view.columns.insert(
            1,
            ColumnView {
                id: "in-progress".to_string(),
                name: "In Progress".to_string(),
                terminal: false,
                cards: vec![taken],
            },
        );
        view
    }

    #[test]
    fn place_agrees_with_work_order_for_every_card() {
        let view = place_fixture();
        let root = Container::BoardRoot("b".to_string());
        let order = work_order(&view, &root);
        assert_eq!(ids(&order), [1, 2, 3, 4, 5, 6, 8]);
        for (i, c) in order.iter().enumerate() {
            let place = place(&view, &root, c.id).expect("card is in the walk");
            assert_eq!(place.index, i + 1, "card {}", c.id.bytes()[0]);
            assert_eq!(place.of, order.len());
            assert_eq!(place.before, i.checked_sub(1).map(|j| order[j].id));
            assert_eq!(place.after, order.get(i + 1).map(|n| n.id));
        }
    }

    #[test]
    fn place_counts_only_ready_cards_ahead() {
        // Four cards are ahead of card 5, but 2 is blocked and 3 is In
        // Progress: `next` would hand out only 1 and 4 before it.
        let view = place_fixture();
        let root = Container::BoardRoot("b".to_string());
        let id = |n: u8| NoteId::new([n; 32]);

        let five = place(&view, &root, id(5)).unwrap();
        assert_eq!(five.index, 5);
        assert_eq!(five.ready_ahead, 2);
        assert!(five.ready());
        assert_eq!(five.ready_ahead, {
            let frontier = ready(&view, &root);
            frontier.iter().position(|c| c.id == id(5)).unwrap()
        });

        let two = place(&view, &root, id(2)).unwrap();
        assert_eq!(two.not_ready, Some(NotReady::Blocked { by: id(9) }));
        let three = place(&view, &root, id(3)).unwrap();
        assert_eq!(three.not_ready, Some(NotReady::InProgress));
        assert_eq!(
            place(&view, &root, id(6)).unwrap().not_ready,
            Some(NotReady::OpenSubissues)
        );
    }

    #[test]
    fn place_reports_whether_the_card_is_sequenced() {
        // Card 1 has a board-root rank, card 5 none; subissue 8 is ranked within
        // its parent 6 even though its own board-root `seq` is unset.
        let view = place_fixture();
        let root = Container::BoardRoot("b".to_string());
        let id = |n: u8| NoteId::new([n; 32]);
        assert!(place(&view, &root, id(1)).unwrap().sequenced);
        assert!(!place(&view, &root, id(5)).unwrap().sequenced);
        assert!(place(&view, &root, id(8)).unwrap().sequenced);

        let under_six = place(&view, &Container::Card([6; 32]), id(8)).unwrap();
        assert_eq!((under_six.index, under_six.of), (1, 1));
        assert_eq!((under_six.before, under_six.after), (None, None));
    }

    #[test]
    fn place_is_none_outside_the_container() {
        let view = place_fixture();
        let id = |n: u8| NoteId::new([n; 32]);
        assert_eq!(place(&view, &Container::Card([6; 32]), id(5)), None);
        let root = Container::BoardRoot("b".to_string());
        assert_eq!(place(&view, &root, id(42)), None);
    }
}
