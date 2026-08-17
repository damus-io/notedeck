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

    /// Pop the top route. Should only be called on a `NavResponse::Returned`.
    /// A non-overlay pop is pushed onto the forward stack so it can be replayed.
    pub fn pop(&mut self) -> Option<R> {
        if self.routes.len() == 1 {
            return None;
        }

        self.returning = false;

        let is_overlay = 's: {
            let Some(last_range) = self.overlay_ranges.last_mut() else {
                break 's false;
            };

            if last_range.end != self.routes.len() {
                break 's false;
            }

            if last_range.end - 1 <= last_range.start {
                self.overlay_ranges.pop();
            } else {
                last_range.end -= 1;
            }

            true
        };

        let popped = self.routes.pop()?;
        if !is_overlay {
            self.forward_stack.push(popped.clone());
        }
        Some(popped)
    }

    /// Removes all routes in the overlay besides the last.
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
                self.routes.remove(num_routes - 2);
            }
            ReplacementType::All => {
                self.routes.drain(..num_routes - 1);
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

    let event = action.and_then(|action| stack.reconcile(action));

    NavFrameResponse {
        response,
        title_response,
        can_take_drag_from,
        event,
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
    use super::{NavStack, NavStackEvent};
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
}
