//! The chrome's navigation types and the app-facing request queue.
//!
//! The chrome shell owns one browser-style [`NavStack`](crate::NavStack) per
//! app: each app's back / forward history. Switching apps isn't an entry; it
//! lands on that app's stack top, where it was left. This module defines the
//! pieces that let an app drive its own stack without ever touching it:
//!
//! - [`AppId`] names an app slot.
//! - [`Navigator`] is a frame-local request queue apps push to; the chrome
//!   drains it after render and applies each request to the active app's
//!   stack, keeping the chrome the sole mutator of every history.
//!
//! A route token is an opaque, app-defined `Rc<dyn Any>`: the chrome never
//! inspects it, only hands it back to the app that pushed it to draw. Keeping
//! it an `Rc` keeps an entry cheaply `Clone` (egui-nav routes must be), and
//! lets the owning app downcast it back to its own route type.

use std::any::Any;
use std::rc::Rc;

/// A stable identifier for an app slot in the chrome's app roster.
///
/// The chrome identifies apps by their index into its `apps` vector (see
/// `Chrome::active` / `NotedeckApp`). `AppId` is a newtype over that slot index
/// so the chrome can name *which* app's stack, or which app opened another,
/// without this core layer knowing anything else about the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AppId(pub usize);

impl AppId {
    /// The app slot index this id refers to.
    pub fn slot(self) -> usize {
        self.0
    }
}

/// A single navigation request an app enqueues during a frame, drained and
/// applied by the chrome after render to the stack of the app that is
/// active then. Every request is in-app: an app can't reach another app's
/// stack, and a back never leaves the app (only the chrome's own back
/// returns to an opener app).
pub enum NavRequest {
    /// Push a route token onto the active app's stack.
    Push(Rc<dyn Any>),

    /// Replace the active app's stack with this one route, once its slide
    /// lands.
    Replace(Rc<dyn Any>),

    /// Drop every entry of the active app's stack whose token the predicate
    /// matches, from both the back and the forward history, in one step.
    /// See [`Navigator::remove`].
    Remove(RoutePredicate),

    /// Go back one step in the active app's stack. At its root, nothing.
    Back,

    /// Go forward one step in the active app's stack.
    Forward,
}

/// Picks out route tokens for [`NavRequest::Remove`]: true means the entry
/// holding the token is dead and should be removed.
pub type RoutePredicate = Box<dyn Fn(&dyn Any) -> bool>;

/// Which nav pass an app's `render_nav` is drawing.
///
/// While a slide or a drag-back moves an app's stack, egui-nav draws two
/// entries a frame: the one beneath the top first, then the top. Each goes
/// through the app's `render_nav`, so an app that keeps state across frames
/// (a notice, a tool queue, the card its keys act on) would otherwise see
/// two views a frame and have to guess which one is real. Of the two, one
/// is the view the transition **lands** on ([`lands`](Self::lands)): what
/// should retire overlays, run tools and latch what the keys read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavPass {
    /// No transition: the stack's top, drawn once. Also the top while a drag
    /// moves it, which stays where it is unless the drag is released far
    /// enough, and only lands beneath once the pop does.
    #[default]
    Settled,
    /// The entry beneath the top, drawn under a slide or a drag. It `lands`
    /// when the slide is a back taking the top off it; under a forward slide
    /// it is the view being left, and under a drag it may never be reached.
    Beneath {
        /// Whether a back is sliding the top off this entry.
        lands: bool,
    },
    /// The top while a push slides it in: the view the push lands on.
    IncomingTop,
    /// The top while a back slides it out: the view being left.
    OutgoingTop,
}

impl NavPass {
    /// Whether this pass draws the view the stack is at, or is landing on:
    /// the one pass a frame that should act on the view rather than just
    /// draw it.
    pub fn lands(self) -> bool {
        match self {
            NavPass::Settled | NavPass::IncomingTop => true,
            NavPass::Beneath { lands } => lands,
            NavPass::OutgoingTop => false,
        }
    }
}

/// The transition an app's stack is in as a frame starts: what the chrome
/// reads off it before drawing, since the stack is lent to egui-nav for the
/// draw itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavTransition {
    /// How many entries the stack holds.
    pub depth: usize,
    /// Whether a push is sliding in.
    pub navigating: bool,
    /// Whether a back is sliding out.
    pub returning: bool,
}

impl NavTransition {
    /// The pass that draws `drawn` entries of the stack: egui-nav hands the
    /// entry beneath the top a stack one shorter than the whole.
    pub fn pass(self, drawn: usize) -> NavPass {
        if drawn < self.depth {
            return NavPass::Beneath {
                lands: self.returning,
            };
        }
        if self.returning {
            return NavPass::OutgoingTop;
        }
        if self.navigating {
            return NavPass::IncomingTop;
        }
        NavPass::Settled
    }
}

/// A frame-local queue of navigation requests raised by apps during rendering.
///
/// Apps never mutate their [`NavStack`](crate::NavStack) — it lives in the
/// chrome, which is its sole mutator. Instead an app enqueues a [`NavRequest`]
/// here (via [`push_route`](Self::push_route), [`replace_route`](Self::replace_route),
/// [`remove_routes`](Self::remove_routes), [`back`](Self::back) or
/// [`forward`](Self::forward)); the chrome drains the queue after each frame's
/// render (see [`take`](Self::take)) and applies each request to the active
/// app's stack. This keeps apps non-blocking and the history consistent,
/// exactly like [`AppActionQueue`](crate::AppActionQueue) does for app actions.
///
/// Raise requests while rendering as the active app: the chrome applies them
/// to whichever app is active when it drains the queue, so a request queued
/// from a background app's `update` would land on another app's stack.
///
/// It also says which [`NavPass`] the chrome is drawing, set around each
/// `render_nav` call ([`pass`](Self::pass)).
#[derive(Default)]
pub struct Navigator {
    requests: Vec<NavRequest>,
    pass: NavPass,
}

impl Navigator {
    /// Which nav pass the app is drawing: [`NavPass::Settled`] outside a
    /// slide, and outside the chrome's nav frame altogether.
    pub fn pass(&self) -> NavPass {
        self.pass
    }

    /// Say which nav pass the next `render_nav` draws. Set by the chrome
    /// before each call, and put back to [`NavPass::Settled`] after it.
    pub fn set_pass(&mut self, pass: NavPass) {
        self.pass = pass;
    }

    /// Enqueue a push of `token` onto the active app's stack.
    pub fn push(&mut self, token: Rc<dyn Any>) {
        self.requests.push(NavRequest::Push(token));
    }

    /// Enqueue a replacement of the active app's stack with `token`.
    pub fn replace(&mut self, token: Rc<dyn Any>) {
        self.requests.push(NavRequest::Replace(token));
    }

    /// Typed helper: enqueue a [`push`](Self::push) of an app's own `route`
    /// value, boxing it into `Rc<dyn Any>`. The app downcasts the token back
    /// to `R` when the chrome hands it to `render_nav`.
    pub fn push_route<R: Any>(&mut self, route: R) {
        self.push(Rc::new(route));
    }

    /// Typed helper: enqueue a [`replace`](Self::replace) with an app's own
    /// `route` value, boxed like [`push_route`](Self::push_route).
    pub fn replace_route<R: Any>(&mut self, route: R) {
        self.replace(Rc::new(route));
    }

    /// Enqueue a prune of the active app's dead entries: every entry whose
    /// token `is_dead` matches leaves its stack, back and forward, in one step
    /// (see [`NavRequest::Remove`]).
    ///
    /// When what an entry names goes away (a card leaves its board, say), the
    /// app removes the entry up front, rather than finding out by drawing it.
    /// The root entry always stays. If the current entry goes, the chrome
    /// lands on the entry beneath it at once, with no slide, and hands each
    /// removed entry to the app's `cleanup_nav`. A prune raised during a
    /// slide waits for it to end.
    pub fn remove(&mut self, is_dead: impl Fn(&dyn Any) -> bool + 'static) {
        self.requests.push(NavRequest::Remove(Box::new(is_dead)));
    }

    /// Typed helper: [`remove`](Self::remove) over an app's own route type. A
    /// token that isn't an `R` (such as the chrome's `()` root) is never
    /// matched.
    pub fn remove_routes<R: Any>(&mut self, is_dead: impl Fn(&R) -> bool + 'static) {
        self.remove(move |token| token.downcast_ref::<R>().is_some_and(&is_dead));
    }

    /// Enqueue a back navigation in the active app.
    pub fn back(&mut self) {
        self.requests.push(NavRequest::Back);
    }

    /// Enqueue a forward navigation in the active app.
    pub fn forward(&mut self) {
        self.requests.push(NavRequest::Forward);
    }

    /// Whether no request is queued.
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
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

        // Enqueue one of each request via the typed and raw helpers.
        nav.push_route(TestRoute::Home);
        nav.push(Rc::new(TestRoute::Thread(7)));
        nav.replace_route(TestRoute::Thread(9));
        nav.back();
        nav.forward();

        let requests = nav.take();
        assert_eq!(requests.len(), 5);

        // take() drained the queue: the next frame starts empty.
        assert!(nav.take().is_empty());

        // Requests preserve enqueue order and carry the token.
        assert!(matches!(
            &requests[0],
            NavRequest::Push(token) if token.downcast_ref::<TestRoute>() == Some(&TestRoute::Home)
        ));
        assert!(matches!(
            &requests[1],
            NavRequest::Push(token)
                if token.downcast_ref::<TestRoute>() == Some(&TestRoute::Thread(7))
        ));
        assert!(matches!(
            &requests[2],
            NavRequest::Replace(token)
                if token.downcast_ref::<TestRoute>() == Some(&TestRoute::Thread(9))
        ));
        assert!(matches!(requests[3], NavRequest::Back));
        assert!(matches!(requests[4], NavRequest::Forward));
    }

    /// egui-nav draws the entry beneath first, with one entry fewer, then
    /// the top; which of the two lands follows the slide's direction.
    #[test]
    fn transition_names_each_draw() {
        let at = |navigating, returning| NavTransition {
            depth: 3,
            navigating,
            returning,
        };

        let still = at(false, false);
        assert_eq!(still.pass(3), NavPass::Settled);
        // A drag: neither flag, the top drawn over the entry beneath.
        assert_eq!(still.pass(2), NavPass::Beneath { lands: false });

        let push = at(true, false);
        assert_eq!(push.pass(2), NavPass::Beneath { lands: false });
        assert_eq!(push.pass(3), NavPass::IncomingTop);

        let back = at(false, true);
        assert_eq!(back.pass(2), NavPass::Beneath { lands: true });
        assert_eq!(back.pass(3), NavPass::OutgoingTop);

        let landing: Vec<bool> = [push.pass(2), push.pass(3), back.pass(2), back.pass(3)]
            .into_iter()
            .map(NavPass::lands)
            .collect();
        assert_eq!(landing, [false, true, true, false]);
        assert!(still.pass(3).lands());
        assert!(!still.pass(2).lands());
    }

    #[test]
    fn typed_prune_matches_only_its_own_route_type() {
        let mut nav = Navigator::default();
        nav.remove_routes(|r: &TestRoute| *r == TestRoute::Thread(7));

        let requests = nav.take();
        let [NavRequest::Remove(is_dead)] = &requests[..] else {
            panic!("expected one prune request");
        };
        assert!(is_dead(&TestRoute::Thread(7)));
        assert!(!is_dead(&TestRoute::Thread(8)));
        assert!(!is_dead(&TestRoute::Home));
        // An app's root token is a `()`, never an app route.
        assert!(!is_dead(&()));
    }
}
