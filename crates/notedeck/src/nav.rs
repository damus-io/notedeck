use crate::route::ReplacementType;
use egui::scroll_area::ScrollAreaOutput;
use egui_nav::{Nav, NavAction, NavResponse, NavUiType, ReturnType, RouteResponse};
use std::ops::Range;

/// A reusable, business-logic-free navigation stack.
///
/// `NavStack<R>` is the single navigation primitive shared by notedeck apps
/// (and, eventually, the chrome-owned global history). It folds three concerns
/// that used to be split across `columns`' `ColumnsRouter` and core's base
/// `Router` into one self-contained type:
///
/// - the **back stack** (`routes`) plus a **forward stack** so that going back
///   and then forward replays the popped route;
/// - **overlay ranges**, contiguous groups of routes where only the most recent
///   survives a single back step (used for stacked modals/overlays);
/// - the `navigating` / `returning` **transition flags** and the
///   route-`replacing` bookkeeping consumed by the egui-nav render loop.
///
/// `R` is the app's route type. `NavStack` never inspects it — it only pushes,
/// pops, and hands routes back to the caller.
#[derive(Clone, Debug)]
pub struct NavStack<R: Clone> {
    /// The active back stack. The last element is the currently-visible route.
    routes: Vec<R>,

    /// Routes popped by [`NavStack::pop`] that a [`NavStack::go_forward`] can
    /// replay. Cleared whenever a fresh [`NavStack::route_to`] navigates
    /// somewhere new.
    forward_stack: Vec<R>,

    /// Overlay groupings. Each range covers a contiguous span of `routes` where
    /// only the most-recently-added route persists when going back.
    overlay_ranges: Vec<Range<usize>>,

    /// True while a back transition is animating out.
    returning: bool,

    /// True while a forward transition is animating in.
    navigating: bool,

    /// Set when a route was pushed to replace previous routes; resolved by
    /// [`NavStack::complete_replacement`] once the new route is placed.
    replacing: Option<ReplacementType>,
}

impl<R: Clone> NavStack<R> {
    /// Create a new stack seeded with `routes`. Panics if `routes` is empty —
    /// a nav stack always has a current route.
    pub fn new(routes: Vec<R>) -> Self {
        if routes.is_empty() {
            panic!("routes can't be empty")
        }
        NavStack {
            routes,
            forward_stack: Vec::new(),
            overlay_ranges: Vec::new(),
            returning: false,
            navigating: false,
            replacing: None,
        }
    }

    /// Push `route` onto the back stack and begin a forward transition. Does not
    /// touch the forward stack — that bookkeeping lives in the public callers.
    fn push_route(&mut self, route: R) {
        self.navigating = true;
        self.routes.push(route);
    }

    /// Navigate to `route`, discarding any forward history.
    pub fn route_to(&mut self, route: R) {
        self.push_route(route);
        self.forward_stack.clear();
    }

    /// Navigate to `route` and fold it into the current overlay group (or start
    /// one), so a single back step collapses the whole overlay.
    pub fn route_to_overlaid(&mut self, route: R) {
        self.route_to(route);
        self.set_overlaying();
    }

    /// Navigate to `route` starting a fresh overlay group.
    pub fn route_to_overlaid_new(&mut self, route: R) {
        self.route_to(route);
        self.new_overlay();
    }

    /// Navigate to `route`, then once it is placed
    /// [`NavStack::complete_replacement`] should be called to drop all previous
    /// routes.
    pub fn route_to_replaced(&mut self, route: R) {
        self.replacing = Some(ReplacementType::All);
        self.push_route(route);
    }

    /// Begin going back, collapsing the current overlay group if any. Returns
    /// the route that will become visible, or `None` if already returning or at
    /// the root.
    pub fn go_back(&mut self) -> Option<R> {
        if self.returning || self.routes.len() == 1 {
            return None;
        }

        if let Some(range) = self.overlay_ranges.pop() {
            tracing::debug!("Going back, found overlay: {:?}", range);
            self.remove_overlay(range);
        } else {
            tracing::debug!("Going back, no overlay");
        }

        self.returning = true;
        if self.routes.len() == 1 {
            return None;
        }
        self.prev().cloned()
    }

    /// Replay the most recently popped route from the forward stack. Returns
    /// true if there was one.
    pub fn go_forward(&mut self) -> bool {
        if let Some(route) = self.forward_stack.pop() {
            self.push_route(route);
            true
        } else {
            false
        }
    }

    /// Jump directly to the back-stack entry at `index`, sending every route
    /// above it onto the forward stack (so a later [`go_forward`](Self::go_forward)
    /// replays them) and making it the current route. Unlike
    /// [`go_back`](Self::go_back) this runs no transition — it lands instantly —
    /// so it suits a history dropdown that jumps several steps at once. An
    /// out-of-range or already-current `index` is a no-op.
    ///
    /// Returns the popped routes, topmost first, for the caller to clean up as
    /// it cleans up a route popped by a completed back: every one of them was
    /// popped, overlay routes included, even though only the non-overlay ones
    /// are kept for redo.
    pub fn go_to_route(&mut self, index: usize) -> Vec<R> {
        let mut popped = Vec::new();
        if index + 1 >= self.routes.len() {
            return popped;
        }
        // Clear any in-flight transition: this is an instant jump, not an
        // animated step.
        self.returning = false;
        self.navigating = false;
        // `pop` records each popped route on the forward stack, so redo still
        // works after a multi-step jump back.
        while self.routes.len() > index + 1 {
            let Some(route) = self.pop() else {
                break;
            };
            popped.push(route);
        }
        popped
    }

    /// True if a [`go_back`](Self::go_back) can make progress: there is a route
    /// beneath the top and no back transition is already animating.
    pub fn can_go_back(&self) -> bool {
        self.routes.len() > 1 && !self.returning
    }

    /// True if a [`go_forward`](Self::go_forward) can replay a popped route.
    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    /// Pop the top route. Should only be called on a `NavResponse::Returned`.
    /// A non-overlay pop is pushed onto the forward stack so it can be replayed.
    pub fn pop(&mut self) -> Option<R> {
        self.remove_top_route(true)
    }

    /// Remove the top route outside a rendered nav return.
    ///
    /// Owner-driven cleanup uses this when the route must disappear before a
    /// different owner becomes active. Unlike [`Self::pop`], the removed route
    /// is not retained as forward history.
    pub fn remove_top_route_for_disposal(&mut self) -> Option<R> {
        let removed = self.remove_top_route(false);
        if removed.is_some() {
            self.returning = false;
            self.navigating = false;
        }
        removed
    }

    fn remove_top_route(&mut self, keep_forward_route: bool) -> Option<R> {
        if self.routes.len() == 1 {
            return None;
        }

        self.returning = false;

        let RemovedRoute { route, in_overlay } = self.remove_route_at(self.routes.len() - 1);
        if keep_forward_route && !in_overlay {
            self.forward_stack.push(route.clone());
        }
        Some(route)
    }

    /// Remove every route the owner says is dead: each back-stack route
    /// above the root for which `keep` returns false, and each forward-stack
    /// route likewise, so a later [`go_forward`](Self::go_forward) can't
    /// replay one. Returns the removed back-stack routes, oldest first, for
    /// the caller to clean up.
    ///
    /// The forward-stack drops are not returned: a route only reaches the
    /// forward stack by being popped, and a pop already hands the route to
    /// its owner's cleanup.
    ///
    /// The root (index 0) always stays, so the stack is never emptied. If the
    /// top is removed, the route beneath it becomes the top **instantly**,
    /// like [`go_to_route`](Self::go_to_route), not through an animated back:
    /// the old top is dead, and an animated back would have to draw it
    /// sliding out. Overlay ranges shift to follow the routes they cover.
    ///
    /// Call it only between transitions (neither [`navigating`](Self::navigating)
    /// nor [`returning`](Self::returning)): egui-nav indexes `routes` while it
    /// animates, so removing one mid-slide would shift what it draws.
    pub fn retain_routes(&mut self, mut keep: impl FnMut(&R) -> bool) -> Vec<R> {
        self.forward_stack.retain(&mut keep);

        let mut removed = Vec::new();
        // Walk top-down so a removal never shifts an index still to visit.
        for index in (1..self.routes.len()).rev() {
            if keep(&self.routes[index]) {
                continue;
            }
            removed.push(self.remove_route_at(index).route);
        }
        removed.reverse();
        removed
    }

    /// Remove the route at `index` and shift the overlay ranges to match:
    /// a range covering `index` shrinks by one (and is dropped once empty),
    /// and a range above it moves down one. The single removal path under
    /// both a top pop and [`retain_routes`](Self::retain_routes).
    fn remove_route_at(&mut self, index: usize) -> RemovedRoute<R> {
        let mut in_overlay = false;
        self.overlay_ranges.retain_mut(|range| {
            if index < range.start {
                range.start -= 1;
                range.end -= 1;
            } else if index < range.end {
                in_overlay = true;
                range.end -= 1;
            }
            range.start < range.end
        });

        RemovedRoute {
            route: self.routes.remove(index),
            in_overlay,
        }
    }

    /// Removes all routes in the overlay besides the last.
    ///
    /// Do not treat the drained routes as missing cleanup work. In Columns, a
    /// multi-route overlay is one thread stack, not a list of independent route
    /// owners: `route_to_overlaid` appends a route to the current `ThreadSubs`
    /// scope, while `route_to_overlaid_new` starts a separate overlay and
    /// scope. On click/back, the retained top route is returned through normal
    /// nav handling with `ReturnType::Click`; `ThreadSubs::unsubscribe_click`
    /// then drops the whole current scope, including the stack entries
    /// represented by routes drained here.
    ///
    /// Returning the drained routes would make callers dispose them as separate
    /// owners and double-release one thread scope. If another overlay type needs
    /// per-route ownership, model that explicitly at the route-owner layer.
    /// Drag returns are different: they do not call `go_back`, but pop one route
    /// and use `ReturnType::Drag`.
    fn remove_overlay(&mut self, overlay_range: Range<usize>) {
        let num_routes = self.routes.len();
        if num_routes <= 1 {
            return;
        }

        if overlay_range.len() <= 1 {
            return;
        }

        self.routes
            .drain(overlay_range.start..overlay_range.end - 1);
    }

    /// Resolve a pending replacement, dropping the routes the new top replaced.
    pub fn complete_replacement(&mut self) {
        let num_routes = self.routes.len();

        self.returning = false;
        let Some(replacement) = self.replacing.take() else {
            return;
        };
        if num_routes < 2 {
            return;
        }

        match replacement {
            ReplacementType::Single => {
                self.remove_route_at(num_routes - 2);
            }
            ReplacementType::All => {
                self.routes.drain(..num_routes - 1);
                // Only the new top is left, and a replacing push never starts
                // an overlay, so no range covers anything any more.
                self.overlay_ranges.clear();
            }
        }
    }

    /// True while a route pushed via [`NavStack::route_to_replaced`] awaits
    /// [`NavStack::complete_replacement`].
    pub fn is_replacing(&self) -> bool {
        self.replacing.is_some()
    }

    /// Fold an `egui_nav` [`NavAction`] back into the stack, applying the stack
    /// half of the transition and returning the resulting [`NavStackEvent`] (if
    /// any) for the caller to react to.
    ///
    /// This is the single, business-logic-free bridge between egui-nav's
    /// animation state machine and the nav stack. It performs only stack
    /// bookkeeping and never touches app state — any cleanup that a popped route
    /// needs is left to the caller, which drives it off the returned event:
    ///
    /// - [`NavAction::Returned`] pops the top route and surfaces it as
    ///   [`NavStackEvent::Popped`] so the caller can free the route's resources.
    /// - [`NavAction::Navigated`] clears the `navigating` flag and resolves any
    ///   pending replacement, surfacing [`NavStackEvent::Navigated`].
    /// - [`NavAction::Navigating`] surfaces [`NavStackEvent::Navigating`]
    ///   without mutating the stack.
    /// - [`NavAction::Returning`], [`NavAction::Dragging`] and
    ///   [`NavAction::Resetting`] are in-flight animation states with no stack
    ///   effect, and return `None`.
    pub fn reconcile(&mut self, action: NavAction) -> Option<NavStackEvent<R>> {
        match action {
            NavAction::Returned(return_type) => {
                let route = self.pop();
                Some(NavStackEvent::Popped { route, return_type })
            }
            NavAction::Navigated => {
                self.navigating_mut(false);
                if self.is_replacing() {
                    self.complete_replacement();
                }
                Some(NavStackEvent::Navigated)
            }
            NavAction::Navigating => Some(NavStackEvent::Navigating),
            NavAction::Returning(_) | NavAction::Dragging | NavAction::Resetting => None,
        }
    }

    /// Extend the active overlay group to include the new top route, or start a
    /// group if the previous route isn't already the tail of one.
    fn set_overlaying(&mut self) {
        let mut overlaying_active = None;
        let mut binding = self.overlay_ranges.last_mut();
        if let Some(range) = &mut binding {
            if range.end == self.routes.len() - 1 {
                overlaying_active = Some(range);
            }
        };

        if let Some(range) = overlaying_active {
            range.end = self.routes.len();
        } else {
            let new_range = self.routes.len() - 1..self.routes.len();
            self.overlay_ranges.push(new_range);
        }
    }

    /// Start a brand-new overlay group at the current top route.
    fn new_overlay(&mut self) {
        let new_range = self.routes.len() - 1..self.routes.len();
        self.overlay_ranges.push(new_range);
    }

    /// The full back stack, oldest first.
    pub fn routes(&self) -> &Vec<R> {
        &self.routes
    }

    /// Snapshot the visible route stack for owner-driven disposal.
    ///
    /// Forced disposal owns only these visible routes. Routes already drained
    /// while collapsing an overlay are represented by the retained overlay
    /// route's `ThreadSubs` scope and must not be synthesized here.
    pub fn routes_for_disposal(&self) -> Vec<R> {
        self.routes.clone()
    }

    /// True while a forward transition is animating.
    pub fn navigating(&self) -> bool {
        self.navigating
    }

    /// Set the forward-transition flag.
    pub fn navigating_mut(&mut self, new: bool) {
        self.navigating = new;
    }

    /// True while a back transition is animating.
    pub fn returning(&self) -> bool {
        self.returning
    }

    /// Set the back-transition flag.
    pub fn returning_mut(&mut self, new: bool) {
        self.returning = new;
    }

    /// The currently-visible (top) route.
    pub fn top(&self) -> &R {
        self.routes.last().expect("routes can't be empty")
    }

    /// The route immediately beneath the top, if any.
    pub fn prev(&self) -> Option<&R> {
        self.routes.get(self.routes.len() - 2)
    }

    /// Number of routes on the back stack.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// True if the back stack is empty (only possible transiently; `new`
    /// forbids constructing an empty stack).
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// One route taken out of a [`NavStack`] by its single removal path.
struct RemovedRoute<R> {
    /// The route that was removed.
    route: R,
    /// Whether it sat inside an overlay range. A popped overlay route isn't
    /// kept for [`NavStack::go_forward`], since going back collapses the
    /// whole overlay.
    in_overlay: bool,
}

/// A business-logic-free navigation event surfaced by [`nav_frame`] (or
/// [`NavStack::reconcile`]) after the egui-nav transition has already been
/// applied to the stack. Callers react to it to run the app-specific cleanup
/// that the core layer deliberately knows nothing about (freeing subscriptions,
/// timeline caches, and so on).
#[derive(Debug)]
pub enum NavStackEvent<R> {
    /// A back navigation completed and the top route was popped. `route` is the
    /// popped route — surfaced so the caller can free the resources it owned,
    /// and `None` only if the stack was already at its root — while
    /// `return_type` records whether the pop came from a drag or a click.
    Popped {
        route: Option<R>,
        return_type: ReturnType,
    },
    /// A forward navigation completed. The stack has already cleared its
    /// `navigating` flag and resolved any pending replacement.
    Navigated,
    /// A forward navigation began.
    Navigating,
}

/// The result of a [`nav_frame`] render pass: the actions produced by the body
/// and title render callbacks, the drag ids the frame can accept, and the
/// reconciled [`NavStackEvent`] (already applied to the stack) that the caller
/// drives its cleanup off of.
pub struct NavFrameResponse<R, A> {
    /// The action returned by rendering the body of the top route.
    pub response: A,
    /// The action returned by rendering the title of the top route.
    pub title_response: A,
    /// egui ids from which this frame is willing to accept a drag.
    pub can_take_drag_from: Vec<egui::Id>,
    /// The stack event produced by reconciling egui-nav's transition, if any.
    pub event: Option<NavStackEvent<R>>,
    /// True while egui-nav is still moving the routes: a forward slide, a back
    /// slide, a drag, or a released drag springing back. egui-nav indexes the
    /// stack's routes through all of these, but a **drag**-back sets neither
    /// of the stack's [`navigating`](NavStack::navigating) /
    /// [`returning`](NavStack::returning) flags, so an owner that must not
    /// remove routes mid-transition reads this as well as those.
    pub in_flight: bool,
}

/// True for an egui-nav action that leaves the transition still moving, so
/// the next frame indexes the same routes again: everything but the two
/// landings, [`NavAction::Returned`] and [`NavAction::Navigated`].
fn nav_action_in_flight(action: Option<NavAction>) -> bool {
    matches!(
        action,
        Some(
            NavAction::Navigating
                | NavAction::Returning(_)
                | NavAction::Dragging
                | NavAction::Resetting
        )
    )
}

/// Render `stack` through [`egui_nav::Nav`] and reconcile the resulting
/// transition back onto the stack, returning a [`NavFrameResponse`].
///
/// This is the shared, business-logic-free nav render loop. It wires the
/// stack's routes and transition flags into egui-nav, renders each route via
/// the caller-supplied `render` callback, and then folds egui-nav's
/// [`NavAction`] back into the stack via [`NavStack::reconcile`]. All
/// app-specific work stays in the caller: what a route looks like lives in
/// `render`, and any cleanup a pop needs is driven off the returned
/// [`NavFrameResponse::event`].
///
/// `id_source` disambiguates this nav's egui state from other navs in the same
/// context; `animate` toggles the slide transitions.
///
/// Note the caller must own `stack` separately from whatever `render` borrows —
/// this suits a chrome that owns the global history and calls into an app to
/// draw each route. A caller whose stack lives inside the same state that
/// `render` mutates (as columns' per-column router does) cannot lend both here
/// at once; it renders with [`egui_nav::Nav`] directly and reconciles afterward
/// via [`NavStack::reconcile`].
pub fn nav_frame<R, A>(
    ui: &mut egui::Ui,
    id_source: egui::Id,
    stack: &mut NavStack<R>,
    animate: bool,
    render: impl FnMut(&mut egui::Ui, NavUiType, &Nav<R>) -> RouteResponse<A>,
) -> NavFrameResponse<R, A>
where
    R: Clone,
{
    let NavResponse {
        response,
        title_response,
        action,
        can_take_drag_from,
    } = Nav::new(stack.routes())
        .id_source(id_source)
        .navigating(stack.navigating())
        .returning(stack.returning())
        .animate_transitions(animate)
        .show_mut(ui, render);

    let in_flight = nav_action_in_flight(action);
    let event = action.and_then(|action| stack.reconcile(action));

    NavFrameResponse {
        response,
        title_response,
        can_take_drag_from,
        event,
        in_flight,
    }
}

pub struct DragResponse<R> {
    pub drag_id: Option<egui::Id>, // the id which was used for dragging.
    pub output: Option<R>,
}

impl<R> DragResponse<R> {
    pub fn none() -> Self {
        Self {
            drag_id: None,
            output: None,
        }
    }

    pub fn scroll(output: ScrollAreaOutput<Option<R>>) -> Self {
        Self {
            drag_id: Some(Self::scroll_output_to_drag_id(output.id)),
            output: output.inner,
        }
    }

    pub fn set_scroll_id(&mut self, output: &ScrollAreaOutput<Option<R>>) {
        self.drag_id = Some(Self::scroll_output_to_drag_id(output.id));
    }

    pub fn output(output: Option<R>) -> Self {
        Self {
            drag_id: None,
            output,
        }
    }

    pub fn set_output(&mut self, output: R) {
        self.output = Some(output);
    }

    /// The id of an `egui::ScrollAreaOutput`
    /// Should use `Self::scroll` when possible
    pub fn scroll_raw(mut self, id: egui::Id) -> Self {
        self.drag_id = Some(Self::scroll_output_to_drag_id(id));
        self
    }

    /// The id which is directly used for dragging
    pub fn set_drag_id_raw(&mut self, id: egui::Id) {
        self.drag_id = Some(id);
    }

    fn scroll_output_to_drag_id(id: egui::Id) -> egui::Id {
        id.with("area")
    }

    pub fn map_output<S>(self, f: impl FnOnce(R) -> S) -> DragResponse<S> {
        DragResponse {
            drag_id: self.drag_id,
            output: self.output.map(f),
        }
    }

    pub fn map_output_maybe<S>(self, f: impl FnOnce(R) -> Option<S>) -> DragResponse<S> {
        DragResponse {
            drag_id: self.drag_id,
            output: self.output.and_then(f),
        }
    }

    pub fn maybe_map_output<S>(self, f: impl FnOnce(Option<R>) -> S) -> DragResponse<S> {
        DragResponse {
            drag_id: self.drag_id,
            output: Some(f(self.output)),
        }
    }

    /// insert the contents of the new DragResponse if they are empty in Self
    pub fn insert(&mut self, body: DragResponse<R>) {
        self.drag_id = self.drag_id.or(body.drag_id);
        if self.output.is_none() {
            self.output = body.output;
        }
    }
}

#[cfg(test)]
mod nav_stack_tests {
    use super::{nav_action_in_flight, NavStack, NavStackEvent};
    use crate::route::ReplacementType;
    use egui_nav::{NavAction, ReturnType};

    #[test]
    #[should_panic(expected = "routes can't be empty")]
    fn new_empty_panics() {
        NavStack::<i32>::new(vec![]);
    }

    #[test]
    fn route_to_pushes_and_flags_navigating() {
        let mut stack = NavStack::new(vec![1]);
        assert!(!stack.navigating());
        stack.route_to(2);
        assert_eq!(stack.routes(), &vec![1, 2]);
        assert_eq!(stack.len(), 2);
        assert_eq!(*stack.top(), 2);
        assert_eq!(stack.prev(), Some(&1));
        assert!(stack.navigating());
    }

    #[test]
    fn pop_records_and_go_forward_replays() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to(3);

        // pop replays in reverse: 3 then 2 land on the forward stack
        assert_eq!(stack.pop(), Some(3));
        assert_eq!(stack.pop(), Some(2));
        assert_eq!(stack.routes(), &vec![1]);

        // going forward replays them back in order
        assert!(stack.go_forward());
        assert_eq!(stack.routes(), &vec![1, 2]);
        assert!(stack.go_forward());
        assert_eq!(stack.routes(), &vec![1, 2, 3]);
        assert!(!stack.go_forward());
    }

    #[test]
    fn disposal_removal_does_not_create_forward_history() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.returning_mut(true);

        assert_eq!(stack.remove_top_route_for_disposal(), Some(2));
        assert_eq!(stack.routes(), &vec![1]);
        assert!(!stack.returning());
        assert!(!stack.navigating());
        assert!(!stack.go_forward());
    }

    #[test]
    fn can_go_back_tracks_depth_and_returning() {
        let mut stack = NavStack::new(vec![1]);
        // at the root there is nowhere to go back to
        assert!(!stack.can_go_back());

        stack.route_to(2);
        assert!(stack.can_go_back());

        // while a back transition is animating, a second back is a no-op, so
        // the control reports itself unavailable
        stack.go_back();
        assert!(stack.returning());
        assert!(!stack.can_go_back());
    }

    #[test]
    fn can_go_forward_tracks_forward_stack() {
        let mut stack = NavStack::new(vec![1]);
        assert!(!stack.can_go_forward());

        stack.route_to(2);
        // a fresh navigation cleared any forward history
        assert!(!stack.can_go_forward());

        stack.pop(); // 2 lands on the forward stack
        assert!(stack.can_go_forward());

        stack.go_forward(); // replays 2, draining the forward stack
        assert!(!stack.can_go_forward());
    }

    #[test]
    fn go_to_route_jumps_and_preserves_forward_replay() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to(3);
        stack.route_to(4);
        assert_eq!(stack.routes(), &vec![1, 2, 3, 4]);

        // jump straight back to the root, skipping the intermediate routes,
        // and hand back every one of them for cleanup, topmost first
        assert_eq!(stack.go_to_route(0), vec![4, 3, 2]);
        assert_eq!(stack.routes(), &vec![1]);
        assert!(!stack.returning());
        assert!(!stack.navigating());

        // everything above the target is redo-able, replayed in original order
        assert!(stack.can_go_forward());
        assert!(stack.go_forward());
        assert!(stack.go_forward());
        assert!(stack.go_forward());
        assert_eq!(stack.routes(), &vec![1, 2, 3, 4]);
        assert!(!stack.go_forward());
    }

    #[test]
    fn go_to_route_current_or_out_of_range_is_noop() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);

        // index of the current top: nothing to do
        assert!(stack.go_to_route(1).is_empty());
        assert_eq!(stack.routes(), &vec![1, 2]);
        assert!(!stack.can_go_forward());

        // past the end: also a no-op
        assert!(stack.go_to_route(9).is_empty());
        assert_eq!(stack.routes(), &vec![1, 2]);
    }

    #[test]
    fn route_to_clears_forward_stack() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.pop(); // forward stack now holds 2

        // navigating somewhere new drops the forward history
        stack.route_to(3);
        assert!(!stack.go_forward());
        assert_eq!(stack.routes(), &vec![1, 3]);
    }

    #[test]
    fn pop_at_root_returns_none() {
        let mut stack = NavStack::new(vec![1]);
        assert_eq!(stack.pop(), None);
        assert_eq!(stack.go_back(), None);
    }

    #[test]
    fn go_back_starts_returning() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        assert!(!stack.returning());
        assert_eq!(stack.go_back(), Some(1));
        assert!(stack.returning());
        // a second go_back while already returning is a no-op
        assert_eq!(stack.go_back(), None);
    }

    #[test]
    fn overlay_collapses_on_go_back() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to_overlaid(2);
        stack.route_to_overlaid(3);
        assert_eq!(stack.routes(), &vec![1, 2, 3]);

        // going back collapses the whole overlay group down to its last member
        assert_eq!(stack.go_back(), Some(1));
        assert_eq!(stack.routes(), &vec![1, 3]);

        // the render loop then pops the surviving overlay route on Returned
        assert_eq!(stack.pop(), Some(3));
        assert_eq!(stack.routes(), &vec![1]);
    }

    #[test]
    fn disposal_snapshot_contains_only_visible_overlay_routes() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to_overlaid(2);
        stack.route_to_overlaid(3);

        assert_eq!(stack.go_back(), Some(1));
        assert_eq!(stack.routes_for_disposal(), vec![1, 3]);
    }

    #[test]
    fn new_overlay_group_is_independent() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to_overlaid(2);
        stack.route_to_overlaid_new(3);
        assert_eq!(stack.routes(), &vec![1, 2, 3]);

        // a fresh single-member overlay drains on the pop that follows go_back,
        // touching only its own route and leaving the earlier overlay intact
        assert_eq!(stack.go_back(), Some(2));
        assert_eq!(stack.pop(), Some(3));
        assert_eq!(stack.routes(), &vec![1, 2]);
    }

    #[test]
    fn replace_drops_previous_routes() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to_replaced(3);
        assert!(stack.is_replacing());
        assert_eq!(stack.routes(), &vec![1, 2, 3]);

        stack.complete_replacement();
        assert!(!stack.is_replacing());
        assert_eq!(stack.routes(), &vec![3]);
    }

    #[test]
    fn retain_routes_removes_a_middle_entry() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to(3);

        assert_eq!(stack.retain_routes(|r| *r != 2), vec![2]);
        assert_eq!(stack.routes(), &vec![1, 3]);
        assert_eq!(*stack.top(), 3, "a middle removal leaves the top alone");
    }

    #[test]
    fn retain_routes_removing_the_top_lands_on_the_next_live_route() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to(3);
        stack.route_to(4);
        stack.navigating_mut(false);

        // 3 and 4 are dead: the top lands on 2 at once, with no slide.
        assert_eq!(stack.retain_routes(|r| *r < 3), vec![3, 4]);
        assert_eq!(stack.routes(), &vec![1, 2]);
        assert_eq!(*stack.top(), 2);
        assert!(!stack.returning());
        assert!(!stack.navigating());
        assert!(
            !stack.can_go_forward(),
            "a pruned top is not redo-able, unlike a pop"
        );
    }

    #[test]
    fn retain_routes_prunes_the_forward_stack() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to(3);
        stack.pop();
        stack.pop(); // forward stack replays 2 then 3

        // Only back-stack routes come back for cleanup; 3 was cleaned on its pop.
        assert!(stack.retain_routes(|r| *r != 3).is_empty());
        assert!(stack.go_forward());
        assert_eq!(stack.routes(), &vec![1, 2]);
        assert!(!stack.go_forward(), "3 was pruned from the redo history");
    }

    #[test]
    fn retain_routes_never_removes_the_root() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);

        assert_eq!(stack.retain_routes(|_| false), vec![2]);
        assert_eq!(stack.routes(), &vec![1], "the root survives a prune-all");
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn retain_routes_keeps_overlay_ranges_consistent() {
        // [1, 2, 3, 4, 5] with 4 and 5 one overlay group (indices 3..5)
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to(3);
        stack.route_to_overlaid(4);
        stack.route_to_overlaid(5);
        assert_eq!(stack.overlay_ranges, vec![3..5]);

        // Removing a route below the group shifts it down.
        assert_eq!(stack.retain_routes(|r| *r != 2), vec![2]);
        assert_eq!(stack.routes(), &vec![1, 3, 4, 5]);
        assert_eq!(stack.overlay_ranges, vec![2..4]);

        // Removing one member shrinks the group.
        assert_eq!(stack.retain_routes(|r| *r != 4), vec![4]);
        assert_eq!(stack.routes(), &vec![1, 3, 5]);
        assert_eq!(stack.overlay_ranges, vec![2..3]);

        // One back collapses what's left of the group and lands on 3. A range
        // left at 3..5 would have drained past the end of the stack here.
        assert_eq!(stack.go_back(), Some(3));
        assert_eq!(stack.pop(), Some(5));
        assert_eq!(stack.routes(), &vec![1, 3]);
    }

    #[test]
    fn retain_routes_drops_an_emptied_overlay_group() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to_overlaid(3);

        // The group's only member goes, and so does the group.
        assert_eq!(stack.retain_routes(|r| *r != 3), vec![3]);
        assert_eq!(stack.routes(), &vec![1, 2]);
        assert!(stack.overlay_ranges.is_empty());
    }

    #[test]
    fn replace_single_keeps_overlay_ranges_consistent() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to_overlaid(2);
        stack.route_to(3);
        stack.replacing = Some(ReplacementType::Single);

        // 3 replaces the route beneath it, which was the group's only member.
        stack.complete_replacement();
        assert_eq!(stack.routes(), &vec![1, 3]);
        assert!(stack.overlay_ranges.is_empty());
    }

    #[test]
    fn replace_all_clears_overlay_ranges() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to_overlaid(2);
        stack.route_to_replaced(3);

        stack.complete_replacement();
        assert_eq!(stack.routes(), &vec![3]);
        assert!(stack.overlay_ranges.is_empty());
    }

    #[test]
    fn reconcile_returned_pops_and_surfaces_route() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);

        let event = stack.reconcile(NavAction::Returned(ReturnType::Click));
        assert!(matches!(
            event,
            Some(NavStackEvent::Popped {
                route: Some(2),
                return_type: ReturnType::Click,
            })
        ));
        assert_eq!(stack.routes(), &vec![1]);
    }

    #[test]
    fn reconcile_returned_at_root_surfaces_no_route() {
        let mut stack = NavStack::new(vec![1]);
        let event = stack.reconcile(NavAction::Returned(ReturnType::Drag));
        assert!(matches!(
            event,
            Some(NavStackEvent::Popped {
                route: None,
                return_type: ReturnType::Drag,
            })
        ));
        assert_eq!(stack.routes(), &vec![1]);
    }

    #[test]
    fn reconcile_navigated_clears_flag_and_completes_replacement() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        stack.route_to_replaced(3);
        assert!(stack.navigating());
        assert!(stack.is_replacing());

        let event = stack.reconcile(NavAction::Navigated);
        assert!(matches!(event, Some(NavStackEvent::Navigated)));
        assert!(!stack.navigating());
        assert!(!stack.is_replacing());
        assert_eq!(stack.routes(), &vec![3]);
    }

    #[test]
    fn reconcile_navigating_surfaces_event_without_mutating() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);

        let before = stack.routes().clone();
        let event = stack.reconcile(NavAction::Navigating);
        assert!(matches!(event, Some(NavStackEvent::Navigating)));
        // still navigating, stack unchanged: only Navigated clears the flag
        assert!(stack.navigating());
        assert_eq!(stack.routes(), &before);
    }

    #[test]
    fn reconcile_in_flight_actions_are_noops() {
        let mut stack = NavStack::new(vec![1]);
        stack.route_to(2);
        let before = stack.routes().clone();

        for action in [
            NavAction::Returning(ReturnType::Click),
            NavAction::Dragging,
            NavAction::Resetting,
        ] {
            assert!(stack.reconcile(action).is_none());
            assert_eq!(stack.routes(), &before);
        }
    }

    #[test]
    fn only_the_two_landings_leave_a_transition_at_rest() {
        // A drag-back sets neither stack flag, so these are what tells an
        // owner the routes are still being drawn mid-move.
        for action in [
            NavAction::Navigating,
            NavAction::Returning(ReturnType::Drag),
            NavAction::Returning(ReturnType::Click),
            NavAction::Dragging,
            NavAction::Resetting,
        ] {
            assert!(nav_action_in_flight(Some(action)), "{action:?}");
        }
        for action in [
            None,
            Some(NavAction::Navigated),
            Some(NavAction::Returned(ReturnType::Drag)),
        ] {
            assert!(!nav_action_in_flight(action), "{action:?}");
        }
    }
}
