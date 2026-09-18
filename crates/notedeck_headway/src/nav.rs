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

#[cfg(test)]
mod tests {
    use super::*;

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
