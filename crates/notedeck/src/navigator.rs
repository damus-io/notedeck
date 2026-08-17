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

/// A single navigation request an app enqueues during a frame, drained and
/// applied to the real [`NavStack`](crate::NavStack) by the chrome after render.
pub enum NavRequest {
    /// Push a new route onto the global history.
    Push(ChromeNavEntry),

    /// Replace the current top route with a new one.
    Replace(ChromeNavEntry),

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
}
