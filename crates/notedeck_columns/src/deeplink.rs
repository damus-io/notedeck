//! Columns' route tokens for the chrome-owned global navigation history.
//!
//! The chrome owns one browser-style [`NavStack`](notedeck::NavStack) spanning
//! every app; each entry carries an opaque `Rc<dyn Any>` route token the chrome
//! never inspects (see [`notedeck::ChromeNavEntry`]). [`ColumnsNavToken`] is the
//! concrete token Columns pushes: either its whole multi-column deck, or a single
//! thread/profile deep-linked in from another app.
//!
//! A deep-link is deliberately *not* a deck column: it renders as its own
//! transient global-nav pane via [`crate::app::Damus::render_nav`], and drilling
//! deeper inside it pushes another global deep-link rather than growing a private
//! stack. This keeps the deck's per-column in-pane navigation entirely private —
//! only entering Columns and cross-app opens are global-history events.

use crate::Route;
use notedeck::AppId;

/// A Columns entry in the chrome-owned global navigation history.
///
/// The chrome hands this back (as `Rc<dyn Any>`) to
/// [`Damus::render_nav`](crate::app::Damus), which downcasts it to pick a render
/// path. An unrecognized token (e.g. the `()` a plain app-switch entry carries)
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
/// [`Damus::cleanup_nav`](crate::app::Damus) when a global-back pops the entry.
pub struct DeepLink {
    /// The Columns app slot, carried so a drill-in from inside the pane can push
    /// a further deep-link onto the *same* app's global history without the app
    /// needing to know its own chrome slot.
    pub app: AppId,

    /// The route this pane renders (a [`Route::Thread`] or a profile
    /// [`Route::Timeline`]).
    pub route: Route,

    /// A dedicated subscription-scope key, distinct from every deck column index
    /// and from every other deep-link, so this pane's thread/timeline sub
    /// bookkeeping never collides with a real column's. Allocated at open time
    /// (see [`Damus::alloc_deeplink_col`](crate::app::Damus::alloc_deeplink_col))
    /// and stable for the entry's life so open and cleanup use the same key.
    pub col: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::AppId;

    /// `deeplink()` is the accessor `render_nav`/`cleanup_nav` use to pick the
    /// deep-link render path: it yields the payload only for the `DeepLink`
    /// variant, so a `Deck` token (and, by the same `None`, any unrecognized
    /// token) falls back to the deck.
    #[test]
    fn deeplink_accessor_only_matches_the_deeplink_variant() {
        assert!(ColumnsNavToken::Deck.deeplink().is_none());

        let token = ColumnsNavToken::DeepLink(DeepLink {
            app: AppId(3),
            route: Route::Relays,
            col: 42,
        });
        let dl = token
            .deeplink()
            .expect("deeplink variant yields its payload");
        assert_eq!(dl.app, AppId(3));
        assert_eq!(dl.col, 42);
    }
}
