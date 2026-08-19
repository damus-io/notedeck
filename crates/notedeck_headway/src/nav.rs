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
//! Board↔card depth never exceeds one — a card→card swap
//! [`replace`](notedeck::Navigator::replace_active_route)s in place rather than
//! growing the stack — so a single global-back always returns to the board.

use enostr::NoteId;

/// A Headway entry in the chrome-owned global navigation history.
///
/// The chrome hands this back (as `Rc<dyn Any>`) to
/// [`Headway::render_nav`](crate::Headway), which downcasts it to pick a render
/// path: [`Board`](Self::Board) — or any unrecognized token, such as the `()` a
/// plain app-switch entry carries — draws the board grid (the root), while
/// [`Card`](Self::Card) draws that card's full-pane detail.
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
}

impl HeadwayRoute {
    /// Build a [`Card`](Self::Card) route for `id`, snapshotting `title`.
    pub fn card(id: NoteId, title: Option<String>) -> Self {
        HeadwayRoute::Card { id, title }
    }

    /// The card this route drills into, if it is a [`Card`](Self::Card).
    pub fn card_id(&self) -> Option<NoteId> {
        match self {
            HeadwayRoute::Card { id, .. } => Some(*id),
            HeadwayRoute::Board => None,
        }
    }

    /// The history-dropdown title for this entry: a card's snapshotted title, or
    /// `None` for the board (so the chrome falls back to the "Headway" app label).
    pub fn title(&self) -> Option<&str> {
        match self {
            HeadwayRoute::Card { title, .. } => title.as_deref(),
            HeadwayRoute::Board => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `card_id` yields the id only for the `Card` variant, so `Board` (and, by
    /// the same `None`, any unrecognized token) drives the board-grid render path.
    #[test]
    fn card_id_only_matches_the_card_variant() {
        assert!(HeadwayRoute::Board.card_id().is_none());

        let id = NoteId::new([7u8; 32]);
        let route = HeadwayRoute::card(id, Some("Fix the thing".to_string()));
        assert_eq!(route.card_id(), Some(id));
        assert_eq!(route.title(), Some("Fix the thing"));
    }

    /// The board carries no per-entry title, so the chrome falls back to the app
    /// label for it.
    #[test]
    fn board_has_no_title() {
        assert_eq!(HeadwayRoute::Board.title(), None);
    }
}
