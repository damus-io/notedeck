//! Chrome-global navigation types and the app-facing request queue.
//!
//! The chrome shell owns a single, browser-style [`NavStack`](crate::NavStack)
//! spanning *all* apps — one global history where back/forward can cross app
//! boundaries. This module defines the pieces that let apps drive that history
//! without ever touching it directly:
//!
//! - [`AppId`] names an app slot so a route knows which app it belongs to.
//! - [`ChromeNavEntry`] is one entry in the global history: an [`AppId`] plus an
//!   opaque, app-defined route token the chrome never inspects.
//! - [`Navigator`] is a frame-local request queue apps push to; the chrome
//!   drains it after render and applies each request to the real stack, keeping
//!   the chrome the sole mutator of the authoritative history.

use std::any::Any;
use std::rc::Rc;

/// A stable identifier for an app slot in the chrome's app roster.
///
/// The chrome identifies apps by their index into its `apps` vector (see
/// `Chrome::active` / `NotedeckApp`). `AppId` is a newtype over that slot index
/// so a [`ChromeNavEntry`] can name *which* app a route belongs to without the
/// chrome — or this core layer — knowing anything else about the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AppId(pub usize);

impl AppId {
    /// The app slot index this id refers to.
    pub fn slot(self) -> usize {
        self.0
    }
}

/// A single entry in the chrome-owned global navigation history.
///
/// The chrome owns one browser-style [`NavStack<ChromeNavEntry>`](crate::NavStack)
/// spanning every app. Each entry names the app it belongs to (`app`) plus an
/// opaque, app-defined route payload (`token`). The chrome **never** inspects
/// `token` — it only routes back to `app` and hands the token to that app to
/// draw. Storing the token as `Rc<dyn Any>` keeps the entry cheaply `Clone`
/// (egui-nav routes must be `Clone`) and lets the owning app downcast it back to
/// its own route type.
#[derive(Clone)]
pub struct ChromeNavEntry {
    /// The app slot this route belongs to.
    pub app: AppId,

    /// Opaque, app-defined route data. The chrome never inspects this; the
    /// owning app downcasts it back to its route type when rendering.
    pub token: Rc<dyn Any>,
}

impl ChromeNavEntry {
    /// Build an entry tagging `token` with the app it belongs to.
    pub fn new(app: AppId, token: Rc<dyn Any>) -> Self {
        Self { app, token }
    }
}

/// A route owned by whichever app is active, still missing its [`AppId`].
///
/// The counterpart of [`ChromeNavEntry`] for an app navigating *within itself*:
/// such an app doesn't know its own slot, so it enqueues just the opaque route
/// token (via [`Navigator::push_active`]). The chrome completes it into a full
/// [`ChromeNavEntry`] by [`tag`](Self::tag)ging it with the active slot when it
/// drains the request. Kept a distinct type — rather than a bare `Rc<dyn Any>`
/// in the [`NavRequest`] variants — so the "token still awaiting its owning app"
/// state is named and self-documenting at the call sites.
#[derive(Clone)]
pub struct ActiveNavEntry {
    /// Opaque, app-defined route token, identical in contract to
    /// [`ChromeNavEntry::token`]; the owning app downcasts it back to its route
    /// type once the chrome hands the completed entry to `render_nav`.
    pub token: Rc<dyn Any>,
}

impl ActiveNavEntry {
    /// Wrap an untagged route `token` awaiting the active [`AppId`].
    pub fn new(token: Rc<dyn Any>) -> Self {
        Self { token }
    }

    /// Complete this active-owned entry into a full [`ChromeNavEntry`] by tagging
    /// its token with the app slot the chrome resolved as active.
    pub fn tag(self, app: AppId) -> ChromeNavEntry {
        ChromeNavEntry::new(app, self.token)
    }
}

/// A single navigation request an app enqueues during a frame, drained and
/// applied to the real [`NavStack`](crate::NavStack) by the chrome after render.
pub enum NavRequest {
    /// Push a new route onto the global history.
    Push(ChromeNavEntry),

    /// Replace the current top route with a new one.
    Replace(ChromeNavEntry),

    /// Push a new route owned by the *currently active* app onto the global
    /// history, letting the chrome fill in the [`AppId`].
    ///
    /// Unlike [`Push`](Self::Push), the app doesn't name the owning slot — it
    /// carries only its opaque route token. The chrome tags it with the active
    /// [`AppId`] when it drains the request (see
    /// [`Navigator::push_active`]). This is how an app navigates *within itself*:
    /// a plain app-switch entry carries a `()` token with no [`AppId`] to read,
    /// and `render_nav` isn't handed the app's own slot, so an app that
    /// originates its own pushes (rather than being deep-linked into from
    /// outside) has no other way to learn which slot to tag.
    PushToActive(ActiveNavEntry),

    /// Replace the current top route with a new route owned by the *currently
    /// active* app, letting the chrome fill in the [`AppId`]. The
    /// self-owned-push counterpart of [`Replace`](Self::Replace); see
    /// [`Navigator::replace_active`].
    ReplaceActive(ActiveNavEntry),

    /// Go back one step in the global history.
    Back,

    /// Go forward one step in the global history.
    Forward,
}

/// A frame-local queue of navigation requests raised by apps during rendering.
///
/// Apps never mutate the authoritative global
/// [`NavStack`](crate::NavStack) — that lives in the chrome, which is its sole
/// mutator. Instead an app enqueues a [`NavRequest`] here (via [`push`](Self::push),
/// [`replace`](Self::replace), [`back`](Self::back), [`forward`](Self::forward),
/// or the typed [`push_route`](Self::push_route) / [`replace_route`](Self::replace_route)
/// helpers); the chrome drains the queue after each frame's render (see
/// [`take`](Self::take)) and applies each request to the real stack. This keeps
/// apps non-blocking and the history consistent, exactly like
/// [`AppActionQueue`](crate::AppActionQueue) does for app actions.
#[derive(Default)]
pub struct Navigator {
    requests: Vec<NavRequest>,
}

impl Navigator {
    /// Enqueue a push of `token` (tagged with `app`) onto the global history.
    pub fn push(&mut self, app: AppId, token: Rc<dyn Any>) {
        self.requests
            .push(NavRequest::Push(ChromeNavEntry::new(app, token)));
    }

    /// Enqueue a replacement of the current top route with `token` (tagged with
    /// `app`).
    pub fn replace(&mut self, app: AppId, token: Rc<dyn Any>) {
        self.requests
            .push(NavRequest::Replace(ChromeNavEntry::new(app, token)));
    }

    /// Enqueue a push of `token` onto the global history, owned by whichever
    /// app is active when the chrome drains this request.
    ///
    /// Use this — not [`push`](Self::push) — when an app navigates *within
    /// itself* and doesn't know its own [`AppId`]. The chrome tags the token
    /// with the active slot on drain (see [`NavRequest::PushToActive`]), so the
    /// owning app downcasts the same token back to its route type when
    /// `render_nav` hands it back.
    pub fn push_active(&mut self, token: Rc<dyn Any>) {
        self.requests
            .push(NavRequest::PushToActive(ActiveNavEntry::new(token)));
    }

    /// Enqueue a replacement of the current top route with `token`, owned by the
    /// active app, with the chrome filling in the [`AppId`] on drain. The
    /// self-owned counterpart of [`replace`](Self::replace).
    pub fn replace_active(&mut self, token: Rc<dyn Any>) {
        self.requests
            .push(NavRequest::ReplaceActive(ActiveNavEntry::new(token)));
    }

    /// Typed helper: enqueue a [`push_active`](Self::push_active) of an app's
    /// own `route` value, boxing it into `Rc<dyn Any>`.
    pub fn push_active_route<R: Any>(&mut self, route: R) {
        self.push_active(Rc::new(route));
    }

    /// Typed helper: enqueue a [`replace_active`](Self::replace_active) of an
    /// app's own `route` value, boxed like [`push_active_route`](Self::push_active_route).
    pub fn replace_active_route<R: Any>(&mut self, route: R) {
        self.replace_active(Rc::new(route));
    }

    /// Enqueue a back navigation.
    pub fn back(&mut self) {
        self.requests.push(NavRequest::Back);
    }

    /// Enqueue a forward navigation.
    pub fn forward(&mut self) {
        self.requests.push(NavRequest::Forward);
    }

    /// Typed helper: enqueue a push of an app's own `route` value, boxing it
    /// into `Rc<dyn Any>` tagged with the app's [`AppId`]. The owning app
    /// downcasts the token back to `R` when the chrome hands it back to render.
    pub fn push_route<R: Any>(&mut self, app: AppId, route: R) {
        self.push(app, Rc::new(route));
    }

    /// Typed helper: enqueue a replacement of the current top route with an
    /// app's own `route` value, boxed like [`push_route`](Self::push_route).
    pub fn replace_route<R: Any>(&mut self, app: AppId, route: R) {
        self.replace(app, Rc::new(route));
    }

    /// Take the queued requests, leaving the queue empty. Called by the chrome
    /// once per frame after render; the moved-out `Vec` hands off this frame's
    /// allocation.
    pub fn take(&mut self) -> Vec<NavRequest> {
        std::mem::take(&mut self.requests)
    }
}

#[cfg(test)]
mod navigator_tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum TestRoute {
        Home,
        Thread(u64),
    }

    #[test]
    fn queue_drains_requests_in_order() {
        let mut nav = Navigator::default();
        let app = AppId(2);

        // Enqueue one of each request via the typed and raw helpers.
        nav.push_route(app, TestRoute::Home);
        nav.push(app, Rc::new(TestRoute::Thread(7)));
        nav.replace_route(app, TestRoute::Thread(9));
        nav.back();
        nav.forward();

        let requests = nav.take();
        assert_eq!(requests.len(), 5);

        // take() drained the queue: the next frame starts empty.
        assert!(nav.take().is_empty());

        // Requests preserve enqueue order and carry the tagged app + token.
        match &requests[0] {
            NavRequest::Push(entry) => {
                assert_eq!(entry.app, app);
                assert_eq!(entry.app.slot(), 2);
                assert_eq!(
                    entry.token.downcast_ref::<TestRoute>(),
                    Some(&TestRoute::Home)
                );
            }
            _ => panic!("expected the first request to be a push"),
        }
        match &requests[1] {
            NavRequest::Push(entry) => assert_eq!(
                entry.token.downcast_ref::<TestRoute>(),
                Some(&TestRoute::Thread(7))
            ),
            _ => panic!("expected the second request to be a push"),
        }
        assert!(matches!(
            &requests[2],
            NavRequest::Replace(entry)
                if entry.token.downcast_ref::<TestRoute>() == Some(&TestRoute::Thread(9))
        ));
        assert!(matches!(requests[3], NavRequest::Back));
        assert!(matches!(requests[4], NavRequest::Forward));
    }

    #[test]
    fn active_helpers_enqueue_untagged_requests() {
        let mut nav = Navigator::default();

        // The active-owned helpers carry only the route token — no AppId, since
        // the chrome fills in the active slot on drain.
        nav.push_active_route(TestRoute::Home);
        nav.replace_active(Rc::new(TestRoute::Thread(3)));

        let requests = nav.take();
        assert_eq!(requests.len(), 2);

        assert!(matches!(
            &requests[0],
            NavRequest::PushToActive(entry)
                if entry.token.downcast_ref::<TestRoute>() == Some(&TestRoute::Home)
        ));
        assert!(matches!(
            &requests[1],
            NavRequest::ReplaceActive(entry)
                if entry.token.downcast_ref::<TestRoute>() == Some(&TestRoute::Thread(3))
        ));

        // `tag` completes an untagged entry into a full ChromeNavEntry.
        let tagged = ActiveNavEntry::new(Rc::new(TestRoute::Home)).tag(AppId(4));
        assert_eq!(tagged.app, AppId(4));
        assert_eq!(
            tagged.token.downcast_ref::<TestRoute>(),
            Some(&TestRoute::Home)
        );
    }
}
