//! Headway's route tokens for the chrome-owned global navigation history.
//!
//! The chrome owns one browser-style [`NavStack`](notedeck::NavStack) spanning
//! every app; each entry carries an opaque `Rc<dyn Any>` route token the chrome
//! never inspects (see [`notedeck::ChromeNavEntry`]). [`HeadwayRoute`] is the
//! concrete token Headway pushes so that drilling from the board into a card's
//! detail joins the global back/forward stack instead of being invisible
//! view-state.
//!
//! Unlike Columns' deep-links — which a *different* app mints and tags with the
//! Columns [`AppId`](notedeck::AppId) — Headway originates its own board→card
//! pushes and doesn't know its own slot: `render_nav` hands it only an opaque
//! token. So it enqueues untagged via
//! [`Navigator::push_active_route`](notedeck::Navigator::push_active_route) and
//! the chrome stamps the active slot on drain (see the `push_active` primitive).
//!
//! The three view depths — board (root), a card's detail, and an epic's
//! dependency [`Graph`](HeadwayRoute::Graph) — each push a new entry, so the
//! global back/forward trail walks board → card → graph and back the same way a
//! browser does. Drilling deeper always pushes (a card→card jump too, so back
//! climbs from a sub-issue up to its parent); leaving a screen backs out one
//! entry. The graph is entered from its epic's detail and carries that epic id,
//! so a single global-back off the graph returns to the epic's card.

use nostrdb_net::NoteId;

/// A Headway entry in the chrome-owned global navigation history.
///
/// The chrome hands this back (as `Rc<dyn Any>`) to
/// [`Headway::render_nav`](crate::Headway), which downcasts it to pick a render
/// path: [`Board`](Self::Board) — or any unrecognized token, such as the `()` a
/// plain app-switch entry carries — draws the board grid (the root),
/// [`Card`](Self::Card) draws that card's full-pane detail, and
/// [`Graph`](Self::Graph) draws an epic's dependency-graph view.
pub enum HeadwayRoute {
    /// The board grid — the root view [`App::render`](notedeck::App::render)
    /// draws. A plain app-switch entry's `()` token renders identically.
    Board,

    /// A single card's full-pane detail, drilled into from the board.
    Card {
        /// The card whose detail this entry renders. `render_nav` resolves it
        /// live against the freshly-folded board each frame, so the detail always
        /// reflects the current card state.
        id: NoteId,

        /// The card's title *at the moment it was opened*, snapshotted so
        /// [`nav_title`](notedeck::App::nav_title) can name this history entry
        /// without an [`Ndb`](nostrdb::Ndb) handle — that hook is handed only the
        /// token, with no [`AppContext`](notedeck::AppContext) to re-resolve
        /// through. A browser-history-style snapshot: it can lag a later rename,
        /// which is fine for a back/forward label. `None` when the title couldn't
        /// be resolved at push time, so the dropdown falls back to the app label.
        title: Option<String>,
    },

    /// An epic's full-pane dependency-graph view, drilled into from that epic's
    /// card detail. `render_nav` seeds both the graph mode *and* the underlying
    /// card selection from `epic`, so a global-back off the graph lands on the
    /// epic's detail rather than skipping straight to the board.
    Graph {
        /// The epic whose dependency graph this entry renders. Resolved live
        /// against the freshly-folded board each frame, like a [`Card`](Self::Card).
        epic: NoteId,

        /// The epic's title *at the moment the graph was opened*, snapshotted for
        /// [`nav_title`](notedeck::App::nav_title) exactly as a card's is — the
        /// hook has no [`Ndb`](nostrdb::Ndb) handle to re-resolve through.
        title: Option<String>,
    },
}

impl HeadwayRoute {
    /// Build a [`Card`](Self::Card) route for `id`, snapshotting `title`.
    pub fn card(id: NoteId, title: Option<String>) -> Self {
        HeadwayRoute::Card { id, title }
    }

    /// Build a [`Graph`](Self::Graph) route for `epic`, snapshotting `title`.
    pub fn graph(epic: NoteId, title: Option<String>) -> Self {
        HeadwayRoute::Graph { epic, title }
    }

    /// The card whose detail this route seeds as selected: a [`Card`](Self::Card)'s
    /// own id, or a [`Graph`](Self::Graph)'s `epic` (so closing the graph returns to
    /// the epic's detail). `None` for the board.
    pub fn selected_card(&self) -> Option<NoteId> {
        match self {
            HeadwayRoute::Card { id, .. } => Some(*id),
            HeadwayRoute::Graph { epic, .. } => Some(*epic),
            HeadwayRoute::Board => None,
        }
    }

    /// The epic whose dependency graph this route opens, if it is a
    /// [`Graph`](Self::Graph).
    pub fn graph_epic(&self) -> Option<NoteId> {
        match self {
            HeadwayRoute::Graph { epic, .. } => Some(*epic),
            HeadwayRoute::Board | HeadwayRoute::Card { .. } => None,
        }
    }

    /// The history-dropdown title for this entry: a card's or graph's snapshotted
    /// title, or `None` for the board (so the chrome falls back to the "Headway"
    /// app label).
    pub fn title(&self) -> Option<&str> {
        match self {
            HeadwayRoute::Card { title, .. } | HeadwayRoute::Graph { title, .. } => {
                title.as_deref()
            }
            HeadwayRoute::Board => None,
        }
    }
}

/// Which of Headway's three view depths a frame is showing, derived from the two
/// [`BoardUiState`](crate::ui::BoardUiState) fields the nav stack seeds: the open-graph
/// epic and the selected card. The graph wins over the card when both are set (an
/// epic's graph is entered from — and drawn over — its own detail), matching the
/// order [`ui::board_ui`](crate::ui::board_ui) renders them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavPos {
    /// The board grid (root) — nothing selected, no graph open.
    Board,
    /// A card's full-pane detail.
    Card(NoteId),
    /// An epic's dependency-graph view.
    Graph(NoteId),
}

impl NavPos {
    /// Fold the `(selected, graph_epic)` state pair into the view position. The
    /// graph takes precedence, mirroring the branch order in [`ui::board_ui`](crate::ui::board_ui).
    pub(crate) fn of(selected: Option<NoteId>, graph_epic: Option<NoteId>) -> Self {
        match (graph_epic, selected) {
            (Some(epic), _) => NavPos::Graph(epic),
            (None, Some(card)) => NavPos::Card(card),
            (None, None) => NavPos::Board,
        }
    }

    /// Nesting depth: board (root) `0`, a card's detail `1`, an epic's graph `2`.
    /// Drilling to a strictly greater depth pushes; stepping to a lesser one backs.
    fn depth(&self) -> u8 {
        match self {
            NavPos::Board => 0,
            NavPos::Card(_) => 1,
            NavPos::Graph(_) => 2,
        }
    }
}

/// The chrome global-history request a board↔card↔graph transition calls for,
/// decided by [`reconcile_nav`] and dispatched onto the [`Navigator`](notedeck::Navigator)
/// in [`Headway::render_board`](crate::Headway::render_board).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NavReconcile {
    /// A card was opened — from the board or drilled into from another card (a
    /// subissue/parent/blocker jump). Both push a new detail entry: the `replace`
    /// primitive collapses the whole history rather than swapping the top, so a
    /// card→card drill pushes to keep a walkable back trail.
    PushCard(NoteId),
    /// An epic's dependency graph was opened from its detail: push a graph entry
    /// one level deeper than the card.
    PushGraph(NoteId),
    /// The open screen was dismissed (a card close/delete/vanish, or the graph
    /// closing back to its epic): step one entry back in the global history.
    Back,
}

/// Map a board↔card↔graph transition to its chrome global-history request.
///
/// `before` is the view position seeded from the entry's route this frame; `after`
/// is what the board UI left after the user interacted. A frame that changed
/// nothing yields `None`, so a steady view enqueues no request and the nav stack
/// doesn't spin. Landing deeper (board→card, card→graph) — or drilling across at
/// the same card depth (card→other-card), or clicking a node in an epic's graph
/// (graph→another-card) — pushes a walkable entry; stepping shallower (card→board,
/// or the graph closing back to its own epic) backs out one. Kept a pure function
/// (no `egui`/`Ndb`) so the mapping is unit-tested on its own.
pub(crate) fn reconcile_nav(before: NavPos, after: NavPos) -> Option<NavReconcile> {
    // A steady frame (same screen still showing) moves nothing.
    if before == after {
        return None;
    }
    match after {
        // The graph is only ever reachable from its epic's detail (one level
        // deeper), so landing on it always pushes.
        NavPos::Graph(epic) => Some(NavReconcile::PushGraph(epic)),
        // Push a walkable card entry when opening a card that sits deeper than or
        // level with where we started (board→card, or a card→card drill), OR when a
        // node was clicked inside an epic's graph. That last step reads as *shallower*
        // (graph→card) yet still pushes, because the graph entry stays on the stack
        // beneath the opened card — a back returns to the graph, not past it. Closing
        // the graph is the other graph→card step: it re-selects the epic itself
        // (card == epic), the genuine back handled below.
        NavPos::Card(card)
            if before.depth() <= after.depth()
                || matches!(before, NavPos::Graph(epic) if epic != card) =>
        {
            Some(NavReconcile::PushCard(card))
        }
        // Stepped to a shallower screen (card→board, or the graph closing back to its
        // own epic): back out one.
        NavPos::Card(_) | NavPos::Board => Some(NavReconcile::Back),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board↔card↔graph transition → global-history request mapping (see
    /// [`reconcile_nav`]): opening a card from the board pushes, drilling from one
    /// card into another pushes too (a walkable trail — not a stack-collapsing
    /// replace), opening an epic's graph pushes one level deeper, and stepping to a
    /// shallower screen backs out — while a frame that left the position unchanged
    /// enqueues nothing.
    #[test]
    fn reconcile_nav_maps_board_card_graph_transitions() {
        let a = NoteId::new([1u8; 32]);
        let b = NoteId::new([2u8; 32]);
        let board = NavPos::Board;
        let card_a = NavPos::Card(a);
        let card_b = NavPos::Card(b);
        let graph_a = NavPos::Graph(a);

        // Steady frames — nothing moved — enqueue no request, so the stack doesn't
        // spin while a board, detail, or graph sits open.
        assert_eq!(reconcile_nav(board, board), None);
        assert_eq!(reconcile_nav(card_a, card_a), None);
        assert_eq!(reconcile_nav(graph_a, graph_a), None);

        // Board → card and card → other-card both push a detail entry.
        assert_eq!(
            reconcile_nav(board, card_a),
            Some(NavReconcile::PushCard(a))
        );
        assert_eq!(
            reconcile_nav(card_a, card_b),
            Some(NavReconcile::PushCard(b))
        );

        // A card → its graph pushes a graph entry one level deeper.
        assert_eq!(
            reconcile_nav(card_a, graph_a),
            Some(NavReconcile::PushGraph(a))
        );

        // Clicking a node in an epic's graph opens *another* card: even though the
        // card sits shallower than the graph, it pushes (the graph entry stays
        // beneath, so a back returns to it).
        assert_eq!(
            reconcile_nav(graph_a, card_b),
            Some(NavReconcile::PushCard(b))
        );

        // Closing the graph re-selects the epic's *own* card (graph_a → card_a), a
        // genuine back; closing the card steps back to the board.
        assert_eq!(reconcile_nav(graph_a, card_a), Some(NavReconcile::Back));
        assert_eq!(reconcile_nav(card_a, board), Some(NavReconcile::Back));
    }

    /// A `Card` route seeds its own id as the selection and opens no graph, so
    /// `Board` (and, by the same `None`, any unrecognized token) drives the
    /// board-grid render path with nothing selected.
    #[test]
    fn card_route_seeds_its_selection_only() {
        assert!(HeadwayRoute::Board.selected_card().is_none());
        assert!(HeadwayRoute::Board.graph_epic().is_none());

        let id = NoteId::new([7u8; 32]);
        let route = HeadwayRoute::card(id, Some("Fix the thing".to_string()));
        assert_eq!(route.selected_card(), Some(id));
        assert!(route.graph_epic().is_none());
        assert_eq!(route.title(), Some("Fix the thing"));
    }

    /// A `Graph` route opens the epic's graph *and* seeds the epic as the selected
    /// card, so a global-back off the graph returns to the epic's detail.
    #[test]
    fn graph_route_seeds_both_graph_and_selection() {
        let epic = NoteId::new([9u8; 32]);
        let route = HeadwayRoute::graph(epic, Some("The epic".to_string()));
        assert_eq!(route.graph_epic(), Some(epic));
        assert_eq!(route.selected_card(), Some(epic));
        assert_eq!(route.title(), Some("The epic"));
    }

    /// The board carries no per-entry title, so the chrome falls back to the app
    /// label for it.
    #[test]
    fn board_has_no_title() {
        assert_eq!(HeadwayRoute::Board.title(), None);
    }
}
