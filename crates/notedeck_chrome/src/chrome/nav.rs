//! The chrome's per-app navigation: one [`AppNav`] stack per app slot, app
//! switches that land where an app was left, the opener hop a cross-app open
//! leaves behind, applying the requests apps queue through the
//! [`Navigator`](notedeck::Navigator), back / forward / jump, and handing
//! every entry they take off a stack to its app's `cleanup_nav`.
//!
//! The model, settled by headway:dave/fresh-champion-crucial:
//!
//! - Each app owns a stack. An app switch (sidebar, tab strip, Ctrl-Tab) is
//!   not an entry: it lands on that app's stack top, where it was left.
//! - A cross-app open (a chip, Dave's `s`) may push onto the target's stack,
//!   and remembers the app it came from as the target's [`Opener`]. The
//!   chrome's back (Alt-←, the header chevron, the mouse's back button)
//!   returns there while the target's stack is still at the depth the open
//!   left it. Switching away forgets it.
//! - An app's own back ([`NavRequest::Back`]) is always in-app, and so is
//!   every other request.
//! - Closing an app (Ctrl-W) drops its stack.

use super::Chrome;
use crate::app::NotedeckApp;
use egui_nav::{NavAction, NavUiType, ReturnType, RouteResponse};
use notedeck::{nav_frame, App, AppContext, NavPass, NavStackEvent};
use notedeck::{
    AppId, NavRequest, NavStack, RoutePredicate, ToolInvocation, ToolOutcome, ToolSpec,
};
use std::any::Any;
use std::rc::Rc;

/// The chrome's back, as a UI tool: what Alt-← does. Bound to Ctrl-O after
/// the jump list's [`JUMP_BACK`](super::JUMP_BACK), so Ctrl-O walks the
/// app's jumps first and then steps back, as vim's Ctrl-O goes on across
/// files.
pub const NAV_BACK: &str = "chrome_nav_back";

/// The chrome's forward, as a UI tool: what Alt-→ does. Bound to Ctrl-I
/// after [`JUMP_FORWARD`](super::JUMP_FORWARD).
pub const NAV_FORWARD: &str = "chrome_nav_forward";

/// The nav stack's UI tools, for the host's tool registry.
pub(super) fn nav_tools() -> [ToolSpec; 2] {
    [
        ToolSpec::new(
            NAV_BACK,
            "Go back in the active app's history, or to the app a cross-app open came from (Alt-←).",
            Vec::new(),
        )
        .titled("Back"),
        ToolSpec::new(
            NAV_FORWARD,
            "Go forward again in the active app's history (Alt-→).",
            Vec::new(),
        )
        .titled("Forward"),
    ]
}

/// One app's navigation: its own back / forward stack, and the app that
/// opened it, while a cross-app open's return still applies.
pub(super) struct AppNav {
    /// The app's history. Its root token is a `()`, which an app draws as its
    /// root view.
    pub(super) stack: NavStack<Rc<dyn Any>>,

    /// The app a cross-app open came from, for the chrome's back to return
    /// to (see [`AppNav::hop`]). Only set on the active app: a switch away
    /// clears it.
    pub(super) opener: Option<Opener>,

    /// Prunes ([`NavRequest::Remove`]) raised while a slide was in flight,
    /// applied in order once it lands (see [`Chrome::apply_nav_requests`]).
    /// egui-nav indexes the stack's routes during the animation.
    pending_prunes: Vec<RoutePredicate>,

    /// True when this stack's egui-nav transition was still moving at the end
    /// of the last frame that drew it: a slide, a drag, or a released drag
    /// springing back (see
    /// [`NavFrameResponse::in_flight`](notedeck::NavFrameResponse)). A
    /// drag-back sets neither of the stack's own transition flags, so this is
    /// what holds a prune over one.
    pub(super) in_flight: bool,
}

/// The app a cross-app open came from, held on the target's [`AppNav`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Opener {
    /// The app the open came from, where the chrome's back returns.
    pub(super) app: AppId,
    /// The target's stack depth right after the open. The return applies only
    /// at this depth: deeper, the chrome's back steps back in-app first.
    depth: usize,
    /// Whether the open pushed an entry onto the target's stack, which the
    /// return then pops. An open that pushes nothing (Dave opening a session,
    /// a note shown in a Columns column) only switches back.
    pushed: bool,
}

/// What one [`Chrome::app_nav_pass`] drew and took off the stack.
pub(super) struct AppNavPass<A> {
    /// What the body returned for the top route.
    pub(super) response: A,
    /// egui ids the drawn routes will take a drag-back from.
    pub(super) can_take_drag_from: Vec<egui::Id>,
    /// The entries a back popped this frame, for their app's `cleanup_nav`
    /// (see [`Chrome::cleanup_entries`]): one a completed back popped, and in
    /// [`settle_nav`](Chrome::settle_nav) mode one the settle landed before
    /// the draw. Empty, without allocating, on a frame that pops nothing.
    pub(super) popped: Vec<TakenEntry>,
}

/// An entry taken off an app's stack, for that app's `cleanup_nav`.
pub(super) struct TakenEntry {
    /// The app whose stack held the entry.
    pub(super) app: AppId,
    /// The entry's route token.
    pub(super) token: Rc<dyn Any>,
}

impl AppNav {
    /// A stack at the app's root, with no opener.
    pub(super) fn new() -> Self {
        AppNav {
            stack: NavStack::new(vec![Rc::new(()) as Rc<dyn Any>]),
            opener: None,
            pending_prunes: Vec::new(),
            in_flight: false,
        }
    }

    /// True while the stack is animating a slide or a drag, when its routes
    /// must not move.
    ///
    /// The stack's own flags cover a slide the chrome started; `in_flight` is
    /// what egui-nav reported at the end of the last [`nav_frame`](notedeck::nav_frame).
    /// That second half is the one a drag-back needs: a drag sets neither
    /// flag, so a prune applied under it would remove the dragged top at once,
    /// and the drag's `Returned` would then pop the entry beneath it too.
    pub(super) fn sliding(&self) -> bool {
        self.stack.navigating() || self.stack.returning() || self.in_flight
    }

    /// The app the chrome's back returns to from here: the opener, while the
    /// stack is at the depth its open left it.
    pub(super) fn hop(&self) -> Option<AppId> {
        self.opener
            .filter(|opener| opener.depth == self.stack.len())
            .map(|opener| opener.app)
    }

    /// Forget an opener the stack has backed out below. Called after anything
    /// that removes entries: a later push back up to its depth must not bring
    /// the return back.
    pub(super) fn forget_stale_opener(&mut self) {
        if self
            .opener
            .is_some_and(|opener| self.stack.len() < opener.depth)
        {
            self.opener = None;
        }
    }

    /// Land a flagged transition at once, as egui-nav would once its slide
    /// ended, without drawing it: a push is navigated, a back pops. Returns
    /// what the back popped, for its app's `cleanup_nav`. Both flags are
    /// landed in egui-nav's order, push first, should one frame set both.
    ///
    /// For [`Chrome::settle_nav`]: egui-nav draws a flagged transition's two
    /// routes on two layers for a frame even with animations off.
    fn settle(&mut self) -> Option<Rc<dyn Any>> {
        if self.stack.navigating() {
            self.stack.reconcile(NavAction::Navigated);
        }
        if !self.stack.returning() {
            return None;
        }
        let popped = match self.stack.reconcile(NavAction::Returned(ReturnType::Click)) {
            Some(NavStackEvent::Popped { route, .. }) => route,
            _ => None,
        };
        self.forget_stale_opener();
        popped
    }

    /// Step back in-app. The pop lands when the slide does, so an opener the
    /// back leaves behind is forgotten now, not then: a chrome back pressed
    /// during the slide must not take the return.
    fn back(&mut self) {
        if !self.stack.can_go_back() {
            return;
        }
        self.stack.go_back();
        if self
            .opener
            .is_some_and(|opener| self.stack.len() - 1 < opener.depth)
        {
            self.opener = None;
        }
    }

    /// Apply this frame's `requests`, after any prune held over a slide that
    /// has since landed. Returns the tokens the prunes took off the back
    /// stack, oldest first.
    fn apply(&mut self, requests: Vec<NavRequest>) -> Vec<Rc<dyn Any>> {
        let mut removed = Vec::new();

        // Prunes held over a slide that has since landed go first: they were
        // raised before anything in this frame's batch.
        if !self.sliding() {
            for is_dead in self.pending_prunes.drain(..) {
                removed.extend(prune(&mut self.stack, &is_dead));
            }
        }

        for request in requests {
            match request {
                NavRequest::Push(token) => self.stack.route_to(token),
                NavRequest::Replace(token) => self.stack.route_to_replaced(token),
                NavRequest::Remove(is_dead) => {
                    if self.sliding() {
                        self.pending_prunes.push(is_dead);
                    } else {
                        removed.extend(prune(&mut self.stack, &is_dead));
                    }
                }
                NavRequest::Back => self.back(),
                NavRequest::Forward => {
                    self.stack.go_forward();
                }
            }
        }

        self.forget_stale_opener();
        removed
    }
}

/// Remove `stack`'s dead entries, returning the removed back-stack tokens
/// (oldest first) for their app's `cleanup_nav`.
fn prune(stack: &mut NavStack<Rc<dyn Any>>, is_dead: &RoutePredicate) -> Vec<Rc<dyn Any>> {
    // `as_ref` hands the predicate the token itself: `&token` would coerce
    // the `Rc` to a `dyn Any`, which no route type downcasts from.
    stack.retain_routes(|token| !is_dead(token.as_ref()))
}

impl Chrome {
    /// Run [`NAV_BACK`] or [`NAV_FORWARD`], or `None` for another tool.
    pub(super) fn call_nav_tool(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> Option<ToolOutcome> {
        let back = match invocation.name.as_str() {
            NAV_BACK => true,
            NAV_FORWARD => false,
            _ => return None,
        };
        let moved = self.nav_tool_available(invocation.name.as_str()) == Some(true);
        if back {
            self.go_back(ctx);
        } else {
            self.go_forward();
        }
        Some(ToolOutcome::Data(serde_json::json!({ "moved": moved })))
    }

    /// Whether [`NAV_BACK`] or [`NAV_FORWARD`] has anywhere to go in the
    /// app in front, or `None` for another tool, so their keys fall through
    /// when they don't. Back is live while the active app's stack can go
    /// back or its opener return applies, as the header chevron is. Doesn't
    /// allocate.
    pub(super) fn nav_tool_available(&self, name: &str) -> Option<bool> {
        let nav = self.active_nav();
        match name {
            NAV_BACK => Some(nav.is_some_and(|nav| nav.stack.can_go_back() || nav.hop().is_some())),
            NAV_FORWARD => Some(nav.is_some_and(|nav| nav.stack.can_go_forward())),
            _ => None,
        }
    }

    /// The navigation of the app in `slot`, if there is one.
    pub(super) fn app_nav(&self, slot: usize) -> Option<&AppNav> {
        self.app_navs.get(slot)
    }

    /// The active app's navigation.
    pub(super) fn active_nav(&self) -> Option<&AppNav> {
        self.app_nav(self.active as usize)
    }

    /// The active app's history: its back / forward stack, the top the
    /// entry on screen. `None` only when no app is active.
    pub fn history(&self) -> Option<&NavStack<Rc<dyn Any>>> {
        self.active_nav().map(|nav| &nav.stack)
    }

    /// The active app's slot.
    pub fn active_app(&self) -> usize {
        self.active as usize
    }

    /// The active app's navigation, mutably.
    fn active_nav_mut(&mut self) -> Option<&mut AppNav> {
        self.app_navs.get_mut(self.active as usize)
    }

    /// Switch the active app to `app`.
    ///
    /// Every app-switch path funnels through here. A switch is not a history
    /// entry: `app` shows its stack top, where it was left. The app being
    /// left forgets its opener, so the chrome's back never leaves an app
    /// reached by a switch. `opened` is updated to match, for
    /// `update()`-gating, the tab strip, and focus-restore.
    pub fn set_active(&mut self, app: i32) {
        if app != self.active {
            if let Some(nav) = self.active_nav_mut() {
                nav.opener = None;
            }
        }
        self.active = app;
        self.set_opened(app as usize);
    }

    /// Land a cross-app open: push `token` onto the stack of the app in
    /// `slot`, switch to it, and remember the app it came from as its
    /// [`Opener`], so one chrome back pops the entry and returns there.
    ///
    /// An open from another app lands at once, with no slide: like an app
    /// switch, it comes from a different app's screen, so there is nothing of
    /// the target's to slide from. A slide would also draw the entry beneath
    /// the opened one, and the target's render of that entry can act on the
    /// open a second time (Headway's pending open then pushed the card again,
    /// leaving the stack one past the opener's depth, so the return took two
    /// backs). An open from the app itself is a plain in-app push, which
    /// slides, with no opener.
    ///
    /// Split out from [`open_note_in_app`](Chrome::open_note_in_app) so the
    /// open is testable without an [`AppContext`].
    #[cfg(any(feature = "headway", test))]
    pub(super) fn push_app_route(&mut self, slot: usize, token: Rc<dyn Any>) {
        let from = self.active;
        let Some(nav) = self.app_navs.get_mut(slot) else {
            return;
        };
        nav.stack.route_to(token);
        let depth = nav.stack.len();
        if from as usize == slot {
            return;
        }
        nav.stack.navigating_mut(false);
        self.set_active(slot as i32);
        self.app_navs[slot].opener = Some(Opener {
            app: AppId(from as usize),
            depth,
            pushed: true,
        });
    }

    /// Remember `from` as the active app's [`Opener`] after a cross-app open
    /// that pushed nothing onto its stack (Dave opening a session, Notebook a
    /// node, Columns a note in a column), so the chrome's back still returns
    /// there. Does nothing when the open stayed in `from`, or when this open
    /// already pushed and recorded its opener (see
    /// [`push_app_route`](Chrome::push_app_route)): only the active app holds
    /// an opener, so one set here is this open's.
    pub(super) fn remember_opener(&mut self, from: i32) {
        if from == self.active {
            return;
        }
        let Some(nav) = self.active_nav_mut() else {
            return;
        };
        if nav.opener.is_some() {
            return;
        }
        nav.opener = Some(Opener {
            app: AppId(from as usize),
            depth: nav.stack.len(),
            pushed: false,
        });
    }

    /// Ask the app in `slot` for the route that opening `note_id` should land
    /// on (see [`notedeck::App::open_note_route`]) and push it via
    /// [`push_app_route`](Chrome::push_app_route).
    ///
    /// Returns `false` — pushing nothing — when the slot is empty or the app
    /// has no route for the note, so the caller can fall back to a plain
    /// [`set_active`](Chrome::set_active).
    #[cfg(feature = "headway")]
    pub(super) fn open_note_in_app(
        &mut self,
        ctx: &mut AppContext,
        slot: usize,
        note_id: nostrdb_net::NoteId,
    ) -> bool {
        let Some(token) = self
            .apps
            .get_mut(slot)
            .and_then(|app| app.open_note_route(ctx, note_id))
        else {
            return false;
        };
        self.push_app_route(slot, token);
        true
    }

    /// Apply the navigation `requests` the active app enqueued this frame
    /// (drained from [`AppContext::navigator`](notedeck::AppContext)) to its
    /// stack.
    ///
    /// This is the chrome's half of the [`Navigator`](notedeck::Navigator)
    /// contract: apps never touch a stack, they queue intent and the chrome
    /// applies it here, to whichever app is active.
    ///
    /// Returns the entries a prune ([`NavRequest::Remove`]) took off the back
    /// stack, oldest first. The caller hands each to its app's `cleanup_nav`,
    /// as a popped entry is: [`drain_nav_requests`](Chrome::drain_nav_requests)
    /// does both halves, and is what the frame calls. A prune that meets a
    /// slide or drag in flight is held and applied by the first call after it
    /// lands, so this runs every frame even with no new requests.
    #[must_use = "a removed entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn apply_nav_requests(&mut self, requests: Vec<NavRequest>) -> Vec<TakenEntry> {
        let app = AppId(self.active as usize);
        let Some(nav) = self.active_nav_mut() else {
            return Vec::new();
        };
        if requests.is_empty() && nav.pending_prunes.is_empty() {
            return Vec::new();
        }
        nav.apply(requests)
            .into_iter()
            .map(|token| TakenEntry { app, token })
            .collect()
    }

    /// Test-only: with `on`, every push and back lands at once instead of
    /// sliding, so a test can drive a full chrome through them under a debug
    /// build.
    ///
    /// egui-nav draws a flagged transition's outgoing and incoming routes on
    /// two layers for one frame, even with animations off, and a widget that
    /// both draw with one fixed id (an explicit `Id`, not one egui derives from
    /// the route's `Ui`) trips egui's debug "changed layer_id" assert. In
    /// this mode the chrome lands the transition before the app's pass draws
    /// it, and again at the end of each frame, so egui-nav only ever sees a
    /// stack at rest. A drag still slides: it never sets the stack's flags.
    pub fn settle_nav(&mut self, on: bool) {
        self.settle_nav = on;
    }

    /// In [`settle_nav`](Chrome::settle_nav) mode, land the active app's
    /// flagged transition (see [`AppNav::settle`]), returning what a back
    /// popped for [`cleanup_entries`](Chrome::cleanup_entries). Otherwise
    /// nothing.
    #[must_use = "a popped entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn settle_active_nav(&mut self) -> Option<TakenEntry> {
        if !self.settle_nav {
            return None;
        }
        let app = AppId(self.active as usize);
        let token = self.active_nav_mut()?.settle()?;
        Some(TakenEntry { app, token })
    }

    /// True while the active app's stack is still sliding (or being dragged)
    /// between entries: until the slide lands, egui-nav renders both the
    /// outgoing and the incoming route, so an app seeding its view from each
    /// route's token sees both. A macro step waits this out (see
    /// [`Settle::nav_sliding`](crate::keymap::macros::Settle::nav_sliding)).
    pub(super) fn nav_sliding(&self) -> bool {
        self.active_nav().is_some_and(AppNav::sliding)
    }

    /// Drain the navigation requests apps queued this frame on
    /// [`AppContext::navigator`](notedeck::AppContext), apply them (see
    /// [`apply_nav_requests`](Chrome::apply_nav_requests)), and hand every
    /// entry a prune removed to its app's `cleanup_nav`.
    pub(super) fn drain_nav_requests(&mut self, ctx: &mut AppContext) {
        let requests = ctx.navigator.take();
        let removed = self.apply_nav_requests(requests);
        self.cleanup_entries(ctx, removed);
    }

    /// Hand each of `entries`, taken off an app's stack, to the `cleanup_nav`
    /// of the app that owns it, so the app can free what the entry's route
    /// opened (e.g. Columns closes a deep-linked thread's subscription).
    ///
    /// Every path that removes entries funnels here — a completed back's pop
    /// in the frame, a prune, a history-dropdown jump, the opener return,
    /// closing an app — so each removed entry is cleaned exactly once. A redo
    /// ([`go_forward`](NavStack::go_forward)) can replay an entry that was
    /// cleaned when it was popped; the app renders it again from its token, as
    /// after any back then forward.
    pub(super) fn cleanup_entries(
        &mut self,
        ctx: &mut AppContext,
        entries: impl IntoIterator<Item = TakenEntry>,
    ) {
        for entry in entries {
            if let Some(app) = self.apps.get_mut(entry.app.slot()) {
                app.cleanup_nav(ctx, &entry.token);
            }
        }
    }

    /// The chrome's back, without the cleanup: returns what it took off a
    /// stack for [`cleanup_entries`](Chrome::cleanup_entries).
    /// [`go_back`](Chrome::go_back) does both halves.
    ///
    /// Where the active app's opener return applies (see [`AppNav::hop`]), it
    /// pops the entry the open pushed, if it pushed one, and switches to the
    /// opener app. Otherwise it is an in-app back, which is deferred and
    /// animated: [`NavStack::go_back`] only flags the transition, and
    /// [`nav_frame`](notedeck::nav_frame) reconciles the pop once the slide
    /// completes. At the app's root with no opener, nothing.
    #[must_use = "an entry the opener return pops must reach its app's cleanup_nav"]
    pub(super) fn step_back(&mut self) -> Vec<TakenEntry> {
        let app = AppId(self.active as usize);
        let Some(nav) = self.active_nav_mut() else {
            return Vec::new();
        };
        let Some(opener) = nav.opener.filter(|o| o.depth == nav.stack.len()) else {
            nav.back();
            return Vec::new();
        };
        nav.opener = None;
        let popped = if opener.pushed {
            nav.stack.remove_top_route_for_disposal()
        } else {
            None
        };
        self.set_active(opener.app.slot() as i32);
        popped
            .map(|token| TakenEntry { app, token })
            .into_iter()
            .collect()
    }

    /// The chrome's back (see [`step_back`](Chrome::step_back)), handing what
    /// it pops to its app's `cleanup_nav`. Shared by the header chevron,
    /// `Alt+←`, the back mouse button, and the history dropdown's return row.
    pub(super) fn go_back(&mut self, ctx: &mut AppContext) {
        let popped = self.step_back();
        self.cleanup_entries(ctx, popped);
    }

    /// Step one entry forward in the active app's stack, replaying the most
    /// recently popped route. Unlike a back step the push lands immediately.
    /// No-op with an empty forward stack. Shared by the header chevron,
    /// `Alt+→`, and the forward mouse button.
    pub(super) fn go_forward(&mut self) {
        if let Some(nav) = self.active_nav_mut() {
            nav.stack.go_forward();
        }
    }

    /// Jump straight to back-stack `index` in the active app's stack (used by
    /// the header history dropdown). Instant, with the skipped-over routes
    /// preserved on the forward stack for redo.
    ///
    /// Every skipped-over entry was popped, so it goes to the app's
    /// `cleanup_nav` just as a completed back's pop does. That is also what
    /// lets a later prune drop forward-stack entries without cleaning them
    /// ([`NavStack::retain_routes`]): they were cleaned on the way there.
    pub(super) fn go_to(&mut self, ctx: &mut AppContext, index: usize) {
        let app = AppId(self.active as usize);
        let Some(nav) = self.active_nav_mut() else {
            return;
        };
        let popped = nav.stack.go_to_route(index);
        nav.forget_stale_opener();
        let popped = popped.into_iter().map(|token| TakenEntry { app, token });
        self.cleanup_entries(ctx, popped);
    }

    /// Drop the stack of the app in `slot`, as closing it does: reset it to
    /// its root and forget its opener and every opener that names it.
    /// Returns its back-stack entries for [`cleanup_entries`](Chrome::cleanup_entries);
    /// its forward entries were cleaned when they were popped.
    #[must_use = "a dropped entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn drop_app_nav(&mut self, slot: usize) -> Vec<TakenEntry> {
        let app = AppId(slot);
        for nav in &mut self.app_navs {
            if nav.opener.is_some_and(|opener| opener.app == app) {
                nav.opener = None;
            }
        }
        let Some(nav) = self.app_navs.get_mut(slot) else {
            return Vec::new();
        };
        let dropped = std::mem::replace(nav, AppNav::new());
        dropped
            .stack
            .routes()
            .iter()
            .map(|token| TakenEntry {
                app,
                token: token.clone(),
            })
            .collect()
    }
}

impl Chrome {
    /// Draw the active app's stack through egui-nav for one frame, and keep
    /// the stack's books on what egui-nav reported: whether the transition is
    /// still in flight (which holds prunes over it), the entry a completed
    /// back popped, and an opener that pop left stale.
    ///
    /// `body` draws one route: it gets the active app, the route's token, and
    /// which [`NavPass`] this draw is (egui-nav draws the entry beneath a
    /// slide first, then the top). The title row draws nothing yet.
    ///
    /// The chrome's frame and the slide test rig both call this, so a test
    /// that drags the stack keeps the same books the app does. It takes the
    /// app and its stack out of `self` together, which is why a caller can't
    /// spell this out itself without copying it. What it pops still has to
    /// reach [`cleanup_entries`](Chrome::cleanup_entries).
    #[must_use = "a popped entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn app_nav_pass<A: Default>(
        &mut self,
        ui: &mut egui::Ui,
        animate: bool,
        mut body: impl FnMut(&mut NotedeckApp, &mut egui::Ui, &Rc<dyn Any>, NavPass) -> RouteResponse<A>,
    ) -> AppNavPass<A> {
        // Settled before the stack is read, so egui-nav never draws the
        // transition in settle-nav mode.
        let settled = self.settle_active_nav();

        let slot = self.active as usize;
        let Chrome { app_navs, apps, .. } = self;
        let (Some(nav), Some(app)) = (app_navs.get_mut(slot), apps.get_mut(slot)) else {
            return AppNavPass {
                response: A::default(),
                can_take_drag_from: Vec::new(),
                popped: settled.into_iter().collect(),
            };
        };
        // Read before the stack is lent to egui-nav, so each draw can tell
        // the app which pass it is.
        let transition = nav.stack.transition();

        // Each app's stack keeps its own egui-nav state, so a slide inside
        // one app survives a switch to another.
        let frame = nav_frame(
            ui,
            egui::Id::unique(("chrome_app_nav", slot)),
            &mut nav.stack,
            animate,
            |ui, ui_type, frame_nav| match ui_type {
                NavUiType::Body => {
                    let routes = frame_nav.routes();
                    let token = routes.last().expect("stack always has a top");
                    body(app, ui, token, transition.pass(routes.len()))
                }

                // The header (back button, breadcrumb) is drawn by the
                // chrome's tab strip, not per route.
                NavUiType::Title => RouteResponse {
                    response: A::default(),
                    can_take_drag_from: Vec::new(),
                },
            },
        );

        // Whether egui-nav is still moving the routes (a drag included),
        // which holds prunes over until it lands.
        nav.in_flight = frame.in_flight;

        // A completed back popped the top entry inside `nav_frame`, after
        // any the settle above popped. The chrome itself never inspects the
        // token. A pop below the opener's depth (a drag-back included)
        // forgets it.
        let drawn_pop = match frame.event {
            Some(NavStackEvent::Popped {
                route: Some(token), ..
            }) => Some(TakenEntry {
                app: AppId(slot),
                token,
            }),
            _ => None,
        };
        let popped = settled.into_iter().chain(drawn_pop).collect();
        nav.forget_stale_opener();

        AppNavPass {
            response: frame.response,
            can_take_drag_from: frame.can_take_drag_from,
            popped,
        }
    }
}

/// A bare chrome over `apps`, each at its root, and nothing else: no egui or
/// ndb context, so tests can drive the nav machinery (`set_active`, the
/// `Navigator` drain, back / forward) directly.
#[cfg(all(test, not(feature = "auto-update")))]
impl Chrome {
    pub(super) fn without_context(apps: Vec<crate::app::NotedeckApp>) -> Self {
        use super::MAX_APPS;
        let app_navs = apps.iter().map(|_| AppNav::new()).collect();
        Chrome {
            active: 0,
            options: crate::ChromeOptions::default(),
            apps,
            opened: [0u16; MAX_APPS / 16],
            app_focus: Default::default(),
            prev_active: 0,
            tools_snapshot: [0u16; MAX_APPS / 16],
            soft_kb_anim_state: Default::default(),
            repaint_causes: Default::default(),
            nav: Default::default(),
            app_navs,
            settle_nav: false,
            pending_open: None,
            keys: Default::default(),
            key_hints: [0u16; MAX_APPS / 16],
            jumps: Default::default(),
            macro_run: None,
            command_bar: Default::default(),
            user_keymap: Default::default(),
            ipc: Default::default(),
        }
    }
}

/// An app that draws nothing, for tests that only read the stacks.
#[cfg(all(test, not(feature = "auto-update")))]
pub(super) struct Blank;

#[cfg(all(test, not(feature = "auto-update")))]
impl App for Blank {
    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> notedeck::AppResponse {
        notedeck::AppResponse::none()
    }
}

// The nav wiring exercised here is independent of the auto-update `updater`
// field, which needs a live egui/ndb context to build. Gating on
// `not(auto-update)` lets these tests construct a bare `Chrome` with no
// context (the field is compiled out) while still running under the default
// feature set CI uses for `notedeck_chrome`. Most drive the stack's reconcile
// by hand; `slide_tests` draws it through the real egui-nav, which a drag
// needs.
#[cfg(all(test, not(feature = "auto-update")))]
mod app_nav_tests {
    use super::*;
    use crate::app::NotedeckApp;
    use egui_nav::{NavAction, ReturnType};

    /// A bare chrome over four blank apps, on slot 0.
    fn nav_test_chrome() -> Chrome {
        let apps = (0..4)
            .map(|i| NotedeckApp::Other(format!("app{i}"), Box::new(Blank)))
            .collect();
        Chrome::without_context(apps)
    }

    fn stack(chrome: &Chrome, slot: usize) -> &NavStack<Rc<dyn Any>> {
        &chrome.app_nav(slot).unwrap().stack
    }

    fn active_stack_mut(chrome: &mut Chrome) -> &mut NavStack<Rc<dyn Any>> {
        &mut chrome.active_nav_mut().unwrap().stack
    }

    /// Land the active app's in-flight forward slide, as `nav_frame` does
    /// once egui-nav reports it placed.
    fn land_slide(chrome: &mut Chrome) {
        active_stack_mut(chrome).reconcile(NavAction::Navigated);
    }

    /// Land the active app's in-flight back slide.
    fn land_back(chrome: &mut Chrome) {
        active_stack_mut(chrome).reconcile(NavAction::Returned(ReturnType::Click));
    }

    /// The `u32` tokens on `slot`'s stack, oldest first (`None` for the root).
    fn history(chrome: &Chrome, slot: usize) -> Vec<Option<u32>> {
        stack(chrome, slot)
            .routes()
            .iter()
            .map(|token| token.downcast_ref::<u32>().copied())
            .collect()
    }

    /// Apply `requests` that remove nothing (no prune among them, none held),
    /// as a frame's drain does.
    fn navigate(chrome: &mut Chrome, requests: Vec<NavRequest>) {
        let removed = chrome.apply_nav_requests(requests);
        assert!(removed.is_empty(), "nothing here removes an entry");
    }

    fn push(token: u32) -> NavRequest {
        NavRequest::Push(Rc::new(token))
    }

    /// A prune request for the active app's `u32` tokens equal to `dead`.
    fn prune(dead: u32) -> NavRequest {
        let mut navigator = notedeck::Navigator::default();
        navigator.remove_routes(move |t: &u32| *t == dead);
        navigator.take().pop().unwrap()
    }

    #[test]
    fn every_app_starts_at_its_root() {
        let chrome = nav_test_chrome();
        for slot in 0..4 {
            assert_eq!(history(&chrome, slot), vec![None]);
        }
        assert_eq!(chrome.active, 0);
    }

    #[test]
    fn set_active_pushes_nothing_and_keeps_each_apps_stack() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);

        chrome.set_active(2);
        assert_eq!(chrome.active, 2);
        assert!(chrome.is_opened(2), "the switched-to app is marked opened");
        assert_eq!(history(&chrome, 2), vec![None], "a switch is no entry");
        assert!(!stack(&chrome, 2).navigating(), "and no slide");

        chrome.set_active(1);
        assert_eq!(
            history(&chrome, 1),
            vec![None, Some(7)],
            "where it was left"
        );
    }

    #[test]
    fn drained_push_request_advances_only_the_active_apps_stack() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(2);

        navigate(&mut chrome, vec![push(7)]);

        assert_eq!(history(&chrome, 2), vec![None, Some(7)]);
        assert_eq!(history(&chrome, 0), vec![None]);
        assert_eq!(chrome.active, 2);
    }

    #[test]
    fn drained_replace_request_collapses_only_the_active_apps_stack() {
        let mut chrome = nav_test_chrome();
        navigate(&mut chrome, vec![push(5)]);
        land_slide(&mut chrome);
        chrome.set_active(2);
        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);

        navigate(&mut chrome, vec![NavRequest::Replace(Rc::new(9u32))]);
        // route_to_replaced defers the drop until the transition completes
        land_slide(&mut chrome);

        assert_eq!(history(&chrome, 2), vec![Some(9)]);
        assert_eq!(history(&chrome, 0), vec![None, Some(5)]);
    }

    #[test]
    fn a_cross_app_open_is_one_entry_and_one_back_returns_to_the_opener() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // the source app, e.g. Dave

        chrome.push_app_route(2, Rc::new(7u32));

        assert_eq!(history(&chrome, 2), vec![None, Some(7)], "one entry");
        assert!(!stack(&chrome, 2).navigating(), "landed at once, no slide");
        assert_eq!(history(&chrome, 1), vec![None], "none on the source");
        assert_eq!(chrome.active, 2);
        assert!(chrome.is_opened(2), "the opened app is marked opened");
        assert_eq!(chrome.active_nav().unwrap().hop(), Some(AppId(1)));

        // ONE chrome back pops the entry and returns to the source.
        let popped = chrome.step_back();
        assert_eq!(chrome.active, 1);
        assert_eq!(history(&chrome, 2), vec![None], "the open's entry went");
        assert_eq!(popped.len(), 1, "and comes back for cleanup");
        assert_eq!(popped[0].app, AppId(2));
        assert_eq!(popped[0].token.downcast_ref::<u32>(), Some(&7));
        assert_eq!(chrome.app_nav(2).unwrap().opener, None);
    }

    #[test]
    fn an_open_that_pushes_nothing_still_returns_to_the_opener() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(2);
        navigate(&mut chrome, vec![push(7)]); // a card, say
        land_slide(&mut chrome);

        // Dave's `s` on the card: Dave opens a session, pushing nothing.
        chrome.set_active(1);
        chrome.remember_opener(2);
        assert_eq!(chrome.active_nav().unwrap().hop(), Some(AppId(2)));

        assert!(chrome.step_back().is_empty(), "nothing to pop");
        assert_eq!(chrome.active, 2);
        assert_eq!(history(&chrome, 2), vec![None, Some(7)], "on the card");
        assert_eq!(history(&chrome, 1), vec![None]);
    }

    #[test]
    fn the_chrome_back_steps_in_app_until_the_opener_depth() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        chrome.push_app_route(2, Rc::new(7u32));
        land_slide(&mut chrome);
        navigate(&mut chrome, vec![push(8)]);
        land_slide(&mut chrome);
        assert_eq!(
            chrome.active_nav().unwrap().hop(),
            None,
            "deeper than the open"
        );

        assert!(chrome.step_back().is_empty());
        land_back(&mut chrome);
        assert_eq!(chrome.active, 2, "an in-app back");
        assert_eq!(history(&chrome, 2), vec![None, Some(7)]);

        let popped = chrome.step_back();
        assert_eq!(popped.len(), 1);
        assert_eq!(chrome.active, 1, "then the return");
    }

    #[test]
    fn an_apps_own_back_below_the_open_forgets_the_opener() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        chrome.push_app_route(2, Rc::new(7u32));
        land_slide(&mut chrome);

        navigate(&mut chrome, vec![NavRequest::Back]);
        assert_eq!(chrome.app_nav(2).unwrap().opener, None, "forgotten at once");
        land_back(&mut chrome);

        // Pushing back up to the open's depth doesn't bring the return back.
        navigate(&mut chrome, vec![push(8)]);
        land_slide(&mut chrome);
        assert_eq!(chrome.active_nav().unwrap().hop(), None);
        assert!(chrome.step_back().is_empty());
        land_back(&mut chrome);
        assert_eq!(chrome.active, 2);
    }

    #[test]
    fn back_and_forward_stay_in_the_app() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);

        // A back only pops once the transition completes; the chrome
        // reconciles that in `nav_frame`, so drive the same reconcile here.
        navigate(&mut chrome, vec![NavRequest::Back]);
        land_back(&mut chrome);
        assert_eq!(history(&chrome, 1), vec![None]);
        assert_eq!(chrome.active, 1);

        // At the root, a back goes nowhere: never into the app before.
        navigate(&mut chrome, vec![NavRequest::Back]);
        assert!(!stack(&chrome, 1).returning());
        assert_eq!(chrome.active, 1);

        // Forward replays 7, in the same app.
        navigate(&mut chrome, vec![NavRequest::Forward]);
        assert_eq!(history(&chrome, 1), vec![None, Some(7)]);
        assert_eq!(chrome.active, 1);
    }

    #[test]
    fn a_drained_prune_touches_only_the_active_apps_stack() {
        let mut chrome = nav_test_chrome();
        // The same token value under two apps.
        chrome.set_active(2);
        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);
        chrome.set_active(1);
        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);

        let removed = chrome.apply_nav_requests(vec![prune(7)]);

        assert_eq!(history(&chrome, 1), vec![None]);
        assert_eq!(history(&chrome, 2), vec![None, Some(7)]);
        assert_eq!(removed.len(), 1, "the removed entry comes back for cleanup");
        assert_eq!(removed[0].app, AppId(1));
        assert_eq!(removed[0].token.downcast_ref::<u32>(), Some(&7));

        // The top was removed, so the landing is instant.
        let nav = stack(&chrome, 1);
        assert!(!nav.navigating() && !nav.returning());
        assert_eq!(chrome.active, 1);
    }

    #[test]
    fn prune_during_a_forward_slide_waits_for_it_to_land() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);

        // A push starts a slide, and the prune in the same batch must wait.
        let removed = chrome.apply_nav_requests(vec![push(7), prune(7)]);
        assert!(removed.is_empty());
        assert_eq!(history(&chrome, 1), vec![None, Some(7)]);
        assert_eq!(chrome.active_nav().unwrap().pending_prunes.len(), 1);

        // A frame with no requests while still sliding changes nothing.
        assert!(chrome.apply_nav_requests(Vec::new()).is_empty());
        assert_eq!(chrome.active_nav().unwrap().pending_prunes.len(), 1);

        // Once the slide lands, the next drain applies the held prune.
        land_slide(&mut chrome);
        let removed = chrome.apply_nav_requests(Vec::new());
        assert_eq!(removed.len(), 1);
        assert_eq!(history(&chrome, 1), vec![None]);
        assert!(chrome.active_nav().unwrap().pending_prunes.is_empty());
    }

    #[test]
    fn prune_during_a_back_slide_waits_for_the_pop() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        navigate(&mut chrome, vec![push(7), push(8)]);
        land_slide(&mut chrome);

        // Back off 8 and prune 7 in one batch: the prune waits for the pop.
        navigate(&mut chrome, vec![NavRequest::Back, prune(7)]);
        assert_eq!(
            history(&chrome, 1),
            vec![None, Some(7), Some(8)],
            "nothing moves under egui-nav while the back slides"
        );

        land_back(&mut chrome);
        let removed = chrome.apply_nav_requests(Vec::new());

        // 8 was popped by the back, then 7 pruned: one step lands on the root.
        assert_eq!(removed.len(), 1);
        assert_eq!(history(&chrome, 1), vec![None]);
    }

    #[test]
    fn a_held_prune_applies_before_the_batch_it_meets() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);

        // Push 7 and prune it in one batch: the prune waits for the slide.
        navigate(&mut chrome, vec![push(7), prune(7)]);
        land_slide(&mut chrome);

        // The next batch pushes a fresh 7. The held prune was raised before
        // it, so it takes only the old 7; applied after the batch, it would
        // take the new one too.
        let removed = chrome.apply_nav_requests(vec![push(7)]);
        assert_eq!(removed.len(), 1, "only the old 7 went");
        assert_eq!(history(&chrome, 1), vec![None, Some(7)]);
        assert!(chrome.active_nav().unwrap().pending_prunes.is_empty());
    }

    #[test]
    fn dropping_an_apps_nav_resets_it_and_forgets_openers_naming_it() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);
        chrome.push_app_route(2, Rc::new(8u32));

        let dropped = chrome.drop_app_nav(1);

        assert_eq!(history(&chrome, 1), vec![None]);
        let tokens: Vec<_> = dropped
            .iter()
            .map(|e| (e.app, e.token.downcast_ref::<u32>().copied()))
            .collect();
        assert_eq!(tokens, vec![(AppId(1), None), (AppId(1), Some(7))]);
        assert_eq!(chrome.app_nav(2).unwrap().opener, None, "names app 1");
    }

    #[test]
    fn the_nav_tools_are_offered_only_while_there_is_somewhere_to_go() {
        let mut chrome = nav_test_chrome();
        let back = |chrome: &Chrome| chrome.nav_tool_available(NAV_BACK);
        let forward = |chrome: &Chrome| chrome.nav_tool_available(NAV_FORWARD);
        assert_eq!(
            back(&chrome),
            Some(false),
            "at the root: Ctrl-O falls through"
        );
        assert_eq!(forward(&chrome), Some(false));
        assert_eq!(chrome.nav_tool_available("chrome_jump_back"), None);

        navigate(&mut chrome, vec![push(7)]);
        land_slide(&mut chrome);
        assert_eq!(back(&chrome), Some(true));

        navigate(&mut chrome, vec![NavRequest::Back]);
        land_back(&mut chrome);
        assert_eq!(back(&chrome), Some(false));
        assert_eq!(forward(&chrome), Some(true), "7 can be redone");

        // An open that pushed nothing: Dave's root, with the return to app 0.
        chrome.set_active(1);
        chrome.remember_opener(0);
        assert!(!stack(&chrome, 1).can_go_back());
        assert_eq!(back(&chrome), Some(true), "the opener return counts");
    }

    // Driven through the real egui-nav, a drag included.
    mod slide_tests {
        use super::*;

        /// A bare chrome's slot-1 stack drawn through the real egui-nav, one
        /// harness step per frame, as the chrome's frame draws it:
        /// [`Chrome::app_nav_pass`], then drain this frame's requests.
        struct DragRig {
            chrome: Chrome,
            /// Requests to drain at the end of the next frame, as an app would
            /// have queued them while rendering.
            requests: Vec<NavRequest>,
            /// Entries a completed back popped inside the nav pass.
            popped: Vec<TakenEntry>,
            /// Entries a drained prune removed.
            removed: Vec<TakenEntry>,
        }

        fn drag_rig_frame(ui: &mut egui::Ui, rig: &mut DragRig) {
            let pass = rig
                .chrome
                .app_nav_pass(ui, true, |_, _, _, _| RouteResponse {
                    response: (),
                    can_take_drag_from: Vec::new(),
                });
            rig.popped.extend(pass.popped);
            let requests = std::mem::take(&mut rig.requests);
            let removed = rig.chrome.apply_nav_requests(requests);
            rig.removed.extend(removed);
        }

        fn pointer(harness: &mut egui_kittest::Harness<'_, DragRig>, event: egui::Event) {
            harness.input_mut().events.push(event);
            harness.step();
        }

        fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }
        }

        #[test]
        fn a_prune_during_a_drag_back_waits_and_the_drag_lands_one_step_back() {
            let mut chrome = nav_test_chrome();
            chrome.set_active(1);
            // [root, 7, 8]: the user drags 8 back towards 7.
            navigate(&mut chrome, vec![push(7), push(8)]);
            let rig = DragRig {
                chrome,
                requests: Vec::new(),
                popped: Vec::new(),
                removed: Vec::new(),
            };
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(800.0, 600.0))
                .with_step_dt(1.0 / 60.0)
                .build_ui_state(drag_rig_frame, rig);

            // Let the push's slide land.
            harness.run_steps(120);
            let nav = harness.state().chrome.active_nav().unwrap();
            assert!(!nav.stack.navigating() && !nav.in_flight);

            // Press and drag rightwards: a drag-back, which sets neither of the
            // stack's own transition flags.
            let mut pos = egui::pos2(100.0, 300.0);
            pointer(&mut harness, egui::Event::PointerMoved(pos));
            pointer(&mut harness, press(pos, true));
            for _ in 0..5 {
                pos.x += 20.0;
                pointer(&mut harness, egui::Event::PointerMoved(pos));
            }
            let nav = harness.state().chrome.active_nav().unwrap();
            assert!(!nav.stack.navigating() && !nav.stack.returning());
            assert!(nav.in_flight, "egui-nav reports the drag");

            // The app prunes the dragged entry this frame. It must wait: the
            // drag is still drawing it.
            harness.state_mut().requests.push(prune(8));
            pos.x += 20.0;
            pointer(&mut harness, egui::Event::PointerMoved(pos));
            assert_eq!(
                history(&harness.state().chrome, 1),
                vec![None, Some(7), Some(8)],
                "nothing moves under egui-nav mid-drag"
            );
            let nav = harness.state().chrome.active_nav().unwrap();
            assert_eq!(nav.pending_prunes.len(), 1);

            // Drag past the return threshold, release, and let it land.
            for _ in 0..10 {
                pos.x += 20.0;
                pointer(&mut harness, egui::Event::PointerMoved(pos));
            }
            pointer(&mut harness, press(pos, false));
            harness.run_steps(120);

            // Exactly one step back: the drag popped 8, and the held prune then
            // found it only on the forward stack, where it drops it uncleaned
            // (the pop is what cleans it).
            let state = harness.state();
            assert_eq!(history(&state.chrome, 1), vec![None, Some(7)]);
            assert_eq!(state.popped.len(), 1);
            assert_eq!(state.popped[0].token.downcast_ref::<u32>(), Some(&8));
            assert!(state.removed.is_empty());
            let nav = state.chrome.active_nav().unwrap();
            assert!(nav.pending_prunes.is_empty());
            assert!(!nav.stack.can_go_forward(), "the pruned 8 can't be redone");
        }
    }
}

// Cleanup routing needs a live `AppContext` to hand to `cleanup_nav`, so these
// build a real one over a temp dir, and a bare chrome whose only app records
// the tokens it's asked to clean up.
#[cfg(all(test, not(feature = "auto-update")))]
mod cleanup_tests {
    use super::*;
    use crate::app::NotedeckApp;
    use egui_nav::NavAction;
    use notedeck::{AppResponse, Notedeck};
    use std::cell::RefCell;

    /// An app whose `cleanup_nav` records each `u32` token it is handed.
    struct CleanupRecorder {
        cleaned: Rc<RefCell<Vec<u32>>>,
    }

    impl App for CleanupRecorder {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }

        fn cleanup_nav(&mut self, _ctx: &mut AppContext<'_>, token: &Rc<dyn Any>) {
            if let Some(token) = token.downcast_ref::<u32>() {
                self.cleaned.borrow_mut().push(*token);
            }
        }
    }

    /// A notedeck to lend an `AppContext`, and a bare chrome whose slot 0 is
    /// a [`CleanupRecorder`] sharing `cleaned`, and slot 1 a blank app.
    struct CleanupFixture {
        _dir: tempfile::TempDir,
        notedeck: Notedeck,
        chrome: Chrome,
        cleaned: Rc<RefCell<Vec<u32>>>,
    }

    impl CleanupFixture {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().expect("tmp dir");
            let args: Vec<String> = ["notedeck-test", "--testrunner"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let notedeck = Notedeck::init(&egui::Context::default(), dir.path(), &args);
            let cleaned = Rc::new(RefCell::new(Vec::new()));
            let recorder = CleanupRecorder {
                cleaned: cleaned.clone(),
            };
            let chrome = Chrome::without_context(vec![
                NotedeckApp::Other("recorder".into(), Box::new(recorder)),
                NotedeckApp::Other("blank".into(), Box::new(Blank)),
            ]);
            Self {
                _dir: dir,
                notedeck,
                chrome,
                cleaned,
            }
        }

        /// Push the recorder's `tokens` in order and land the slide:
        /// `[root, t0, t1, ..]`.
        fn push(&mut self, tokens: &[u32]) {
            let requests = tokens
                .iter()
                .map(|t| NavRequest::Push(Rc::new(*t)))
                .collect();
            let removed = self.chrome.apply_nav_requests(requests);
            assert!(removed.is_empty());
            self.chrome
                .active_nav_mut()
                .unwrap()
                .stack
                .reconcile(NavAction::Navigated);
        }

        /// Drain one frame's queued prune of the recorder's token `dead`, as
        /// the chrome's `render` does.
        fn drain_prune(&mut self, dead: u32) {
            let mut ctx = self.notedeck.app_context();
            ctx.navigator.remove_routes(move |t: &u32| *t == dead);
            self.chrome.drain_nav_requests(&mut ctx);
        }

        fn tokens(&self) -> Vec<Option<u32>> {
            self.chrome
                .app_nav(0)
                .unwrap()
                .stack
                .routes()
                .iter()
                .map(|token| token.downcast_ref::<u32>().copied())
                .collect()
        }

        fn cleaned(&self) -> Vec<u32> {
            self.cleaned.borrow().clone()
        }
    }

    #[test]
    fn a_drained_prune_hands_what_it_removed_to_the_apps_cleanup() {
        let mut fx = CleanupFixture::new();
        fx.push(&[7, 8]);

        fx.drain_prune(7);

        assert_eq!(fx.tokens(), vec![None, Some(8)]);
        assert_eq!(
            fx.cleaned(),
            vec![7],
            "the pruned entry reached cleanup_nav"
        );
    }

    #[test]
    fn a_history_jump_cleans_every_skipped_entry_once() {
        let mut fx = CleanupFixture::new();
        fx.push(&[7, 8, 9]);

        // The dropdown jumps from 9 straight back to 7.
        let mut ctx = fx.notedeck.app_context();
        fx.chrome.go_to(&mut ctx, 1);
        drop(ctx);

        assert_eq!(fx.tokens(), vec![None, Some(7)]);
        assert_eq!(
            fx.cleaned(),
            vec![9, 8],
            "each skipped entry, topmost first"
        );

        // 8 now sits on the forward stack, cleaned. Pruning it drops it from
        // there without a second cleanup.
        fx.drain_prune(8);
        assert_eq!(fx.cleaned(), vec![9, 8], "no double clean");
        assert_eq!(fx.tokens(), vec![None, Some(7)]);
    }

    #[test]
    fn the_nav_back_tool_is_the_chrome_back() {
        let mut fx = CleanupFixture::new();
        fx.chrome.set_active(1);
        fx.chrome.push_app_route(0, Rc::new(7u32));

        let mut ctx = fx.notedeck.app_context();
        let back = ToolInvocation::new(NAV_BACK, serde_json::Value::Null);
        let outcome = fx.chrome.call_chrome_tool(&mut ctx, &back);
        drop(ctx);

        assert!(matches!(outcome, Some(ToolOutcome::Data(v)) if v["moved"] == true));
        assert_eq!(fx.chrome.active, 1, "the opener return, as Alt-← takes it");
        assert_eq!(fx.cleaned(), vec![7]);
    }

    #[test]
    fn the_opener_return_cleans_the_entry_the_open_pushed() {
        let mut fx = CleanupFixture::new();
        fx.chrome.set_active(1);
        fx.chrome.push_app_route(0, Rc::new(7u32));

        let mut ctx = fx.notedeck.app_context();
        fx.chrome.go_back(&mut ctx);
        drop(ctx);

        assert_eq!(fx.chrome.active, 1);
        assert_eq!(fx.tokens(), vec![None]);
        assert_eq!(fx.cleaned(), vec![7]);
    }
}
