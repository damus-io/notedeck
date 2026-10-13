//! Columns' route tokens for its chrome-owned navigation stack.
//!
//! The chrome owns one browser-style [`NavStack`](notedeck::NavStack) per app;
//! each entry carries an opaque `Rc<dyn Any>` route token the chrome never
//! inspects (see [`notedeck::Navigator`]). [`ColumnsNavToken`] is the
//! concrete token Columns pushes: either its whole multi-column deck, or a single
//! thread/profile deep-linked in from another app.
//!
//! A deep-link is deliberately *not* a deck column: it renders as its own
//! transient global-nav pane via [`App::render_nav`](notedeck::App::render_nav), and drilling
//! deeper inside it pushes another global deep-link rather than growing a private
//! stack. This keeps the deck's per-column in-pane navigation entirely private —
//! only deep-links are entries on Columns' chrome-owned stack.

use crate::Route;

/// Stable identity of one transient global deep-link entry.
///
/// The id is allocated before the entry opens its route, then carried through
/// rendering and cleanup. It is not a deck column index.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeepLinkId(u64);

impl DeepLinkId {
    /// Build a deep-link identity from the app's monotonic allocator.
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }
}

/// A Columns entry in its chrome-owned navigation stack.
///
/// The chrome hands this back (as `Rc<dyn Any>`) to
/// [`Damus::render_nav`](crate::app::Damus), which downcasts it to pick a render
/// path. An unrecognized token (e.g. the `()` the stack's root carries)
/// falls back to the deck, so `Deck` and a missing token render identically.
pub enum ColumnsNavToken {
    /// The normal multi-column deck workspace — the same view
    /// [`App::render`](notedeck::App::render) draws.
    Deck,

    /// A single thread or profile opened from another app, rendered as its own
    /// transient global-nav pane rather than a deck column.
    DeepLink(DeepLink),
}

impl ColumnsNavToken {
    /// The deep-link this token names, if it is one.
    pub fn deeplink(&self) -> Option<&DeepLink> {
        match self {
            ColumnsNavToken::DeepLink(dl) => Some(dl),
            ColumnsNavToken::Deck => None,
        }
    }
}

/// A deep-linked Columns route shown as a single transient global-nav pane.
///
/// Created by [`Damus::open_deeplink`](crate::app::Damus::open_deeplink), which
/// opens the route's subscription before the entry is pushed, and torn down by
/// [`App::cleanup_nav`](notedeck::App::cleanup_nav) when a global-back pops the
/// entry.
pub struct DeepLink {
    /// The route this pane renders (a [`Route::Thread`] or a profile
    /// [`Route::Timeline`]).
    pub route: Route,

    /// Stable identity for this global-nav entry.
    ///
    /// Allocated once when the entry opens and never reused during the `Damus`
    /// session. It keeps entry-owned thread state and UI state distinct from deck
    /// columns and other deep links through rendering and cleanup.
    pub id: DeepLinkId,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `deeplink()` is the accessor `render_nav`/`cleanup_nav` use to pick the
    /// deep-link render path: it yields the payload only for the `DeepLink`
    /// variant, so a `Deck` token (and, by the same `None`, any unrecognized
    /// token) falls back to the deck.
    #[test]
    fn deeplink_accessor_only_matches_the_deeplink_variant() {
        assert!(ColumnsNavToken::Deck.deeplink().is_none());

        let token = ColumnsNavToken::DeepLink(DeepLink {
            route: Route::Relays,
            id: DeepLinkId::new(42),
        });
        let dl = token
            .deeplink()
            .expect("deeplink variant yields its payload");
        assert_eq!(dl.id, DeepLinkId::new(42));
    }
}
