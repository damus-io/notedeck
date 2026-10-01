//! The chrome's global navigation history: seeding it, recording app
//! switches and cross-app opens as entries, applying the requests apps queue
//! through the [`Navigator`](notedeck::Navigator), global back / forward /
//! jump, handing every entry they take off the history to its app's
//! `cleanup_nav`, and re-deriving the active app from the stack top.

use super::Chrome;
use notedeck::{App, AppContext};
use notedeck::{AppId, ChromeNavEntry, NavRequest, NavStack, RoutePredicate};
use std::rc::Rc;

/// A [`NavRequest::RemoveActive`] prune, tagged with the app that raised it.
///
/// Held on [`Chrome`] while a slide is in flight, because egui-nav indexes the
/// history's routes during the animation; applied once the slide has landed.
pub(super) struct PendingPrune {
    /// The app whose entries this prune may remove: the active app when the
    /// request was drained.
    app: AppId,
    /// True for a token whose entry is dead.
    is_dead: RoutePredicate,
}

impl PendingPrune {
    /// Remove this prune's dead entries from `nav`, returning the removed
    /// back-stack entries (oldest first) for their app's `cleanup_nav`.
    fn apply(&self, nav: &mut NavStack<ChromeNavEntry>) -> Vec<ChromeNavEntry> {
        // `as_ref` hands the predicate the token itself: `&entry.token` would
        // coerce the `Rc` to a `dyn Any`, which no route type downcasts from.
        nav.retain_routes(|entry| entry.app != self.app || !(self.is_dead)(entry.token.as_ref()))
    }
}

/// True while `nav` is animating a slide or a drag, when its routes must not
/// move.
///
/// The stack's own flags cover a slide the chrome started; `in_flight` is
/// what egui-nav reported at the end of this frame's
/// [`nav_frame`](notedeck::nav_frame) (see [`Chrome::global_nav_in_flight`]).
/// That second half is the one a drag-back needs: a drag sets neither flag,
/// so a prune applied under it would remove the dragged top at once, and the
/// drag's `Returned` would then pop the entry beneath it too.
fn sliding(nav: &NavStack<ChromeNavEntry>, in_flight: bool) -> bool {
    nav.navigating() || nav.returning() || in_flight
}

/// Seed the chrome-global navigation history with the initial app's route
/// (slot 0). [`NavStack::new`] panics on an empty stack, so the chrome is born
/// with exactly this one entry. The route token is a placeholder `Rc::new(())`:
/// no app reads its own token yet (that lands with Columns' `render_nav`
/// override in a later subissue) and the chrome never inspects it.
pub(super) fn seed_global_nav() -> NavStack<ChromeNavEntry> {
    NavStack::new(vec![ChromeNavEntry::new(AppId(0), Rc::new(()))])
}

impl Chrome {
    /// Switch the active app to `app`.
    ///
    /// Every app-switch path funnels through here, so this is where the switch
    /// is recorded in the global history: unless `app` already owns the top
    /// entry (guarding a redundant duplicate), a new [`ChromeNavEntry`] is
    /// pushed via [`NavStack::route_to`], which sets the `navigating` flag so
    /// [`nav_frame`](notedeck::nav_frame) animates the transition. `active` and
    /// the `opened` bitset are updated to match — they stay authoritative for
    /// `update()`-gating, the tab strip, and focus-restore, and always agree
    /// with the stack top (see [`Chrome::sync_active_from_nav`]).
    pub fn set_active(&mut self, app: i32) {
        if let Some(nav) = self.global_nav.as_mut() {
            if nav.top().app != AppId(app as usize) {
                nav.route_to(ChromeNavEntry::new(AppId(app as usize), Rc::new(())));
            }
        }
        self.active = app;
        self.set_opened(app as usize);
    }

    /// Land a cross-app open as ONE global-history entry: push `token` tagged
    /// with the target app's `slot`, then re-derive `active` from the new top.
    ///
    /// This deliberately bypasses [`set_active`](Chrome::set_active). That
    /// records the switch as its own untyped `()` app-switch entry, which the
    /// target renders as its root; the app then routes to the opened note on
    /// top of it, so a cross-app open would land TWO entries and take two back
    /// presses to return. Here the routed entry IS the switch.
    ///
    /// Split out from [`open_note_in_app`](Chrome::open_note_in_app) so the
    /// one-entry invariant is testable without an [`AppContext`].
    #[cfg(any(feature = "headway", test))]
    fn push_app_route(&mut self, slot: usize, token: Rc<dyn std::any::Any>) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.route_to(ChromeNavEntry::new(AppId(slot), token));
        }
        self.active = slot as i32;
        self.set_opened(slot);
    }

    /// Ask the app in `slot` for the route that opening `note_id` should land
    /// on (see [`notedeck::App::open_note_route`]) and push it as one entry via
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

    /// Re-derive `active` (and its `opened` bit) from the global history's top
    /// entry. Called after every stack mutation the chrome doesn't drive through
    /// [`set_active`](Chrome::set_active) — the per-frame [`nav_frame`](notedeck::nav_frame) reconcile
    /// (which may pop on a completed back) and the [`Navigator`](notedeck::Navigator)
    /// drain — so the active app tracks whichever app owns the current route,
    /// including after a global back/forward crossed an app boundary.
    pub(super) fn sync_active_from_nav(&mut self) {
        let Some(nav) = self.global_nav.as_ref() else {
            return;
        };
        let slot = nav.top().app.slot();
        self.active = slot as i32;
        self.set_opened(slot);
    }

    /// Apply the navigation `requests` an app enqueued this frame (drained from
    /// [`AppContext::navigator`](notedeck::AppContext)) to the global history,
    /// then re-derive the active app from the new top.
    ///
    /// This is the chrome's half of the [`Navigator`](notedeck::Navigator)
    /// contract: apps never touch the authoritative stack, they queue intent and
    /// the chrome applies it here.
    ///
    /// Returns the entries a prune ([`NavRequest::RemoveActive`]) took off the
    /// back stack, oldest first. The caller hands each to its app's
    /// `cleanup_nav`, as a popped entry is: [`drain_nav_requests`](Chrome::drain_nav_requests)
    /// does both halves, and is what the frame calls. A prune that meets a
    /// slide or drag in flight is held in `pending_prunes` and applied by the
    /// first call after it lands, so this runs every frame even with no new
    /// requests.
    #[must_use = "a removed entry that skips its app's cleanup_nav leaks what its route opened"]
    pub(super) fn apply_nav_requests(&mut self, requests: Vec<NavRequest>) -> Vec<ChromeNavEntry> {
        if requests.is_empty() && self.pending_prunes.is_empty() {
            return Vec::new();
        }

        // The active-owned requests (`PushToActive`/`ReplaceActive`/
        // `RemoveActive`) don't name their app — the enqueuing app doesn't know
        // its own slot — so the chrome completes them here by tagging them with
        // the active slot. This runs during the same frame's render as the
        // enqueue and before `sync_active_from_nav`, so `active` still names the
        // app that raised the request (a plain app-switch funnels through
        // `set_active`, not here).
        let active = AppId(self.active as usize);

        let mut removed = Vec::new();
        let Chrome {
            global_nav,
            pending_prunes,
            global_nav_in_flight,
            ..
        } = self;
        let in_flight = *global_nav_in_flight;
        if let Some(nav) = global_nav.as_mut() {
            // Prunes held over a slide that has since landed go first: they
            // were raised before anything in this frame's batch.
            if !sliding(nav, in_flight) {
                for prune in pending_prunes.drain(..) {
                    removed.extend(prune.apply(nav));
                }
            }

            for request in requests {
                match request {
                    NavRequest::Push(entry) => nav.route_to(entry),
                    NavRequest::Replace(entry) => nav.route_to_replaced(entry),
                    NavRequest::PushToActive(entry) => nav.route_to(entry.tag(active)),
                    NavRequest::ReplaceActive(entry) => nav.route_to_replaced(entry.tag(active)),
                    NavRequest::RemoveActive(is_dead) => {
                        let prune = PendingPrune {
                            app: active,
                            is_dead,
                        };
                        if sliding(nav, in_flight) {
                            pending_prunes.push(prune);
                        } else {
                            removed.extend(prune.apply(nav));
                        }
                    }
                    NavRequest::Back => {
                        nav.go_back();
                    }
                    NavRequest::Forward => {
                        nav.go_forward();
                    }
                }
            }
        }

        self.sync_active_from_nav();
        removed
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

    /// Hand each of `entries`, taken off the global history, to the
    /// `cleanup_nav` of the app that owns it, so the app can free what the
    /// entry's route opened (e.g. Columns closes a deep-linked thread's
    /// subscription).
    ///
    /// Every path that removes entries funnels here — a completed back's pop
    /// in the frame, a prune, a history-dropdown jump — so each removed
    /// entry is cleaned exactly once. A redo ([`go_forward`](NavStack::go_forward))
    /// can replay an entry that was cleaned when it was popped; the app
    /// renders it again from its token, as after any back then forward.
    pub(super) fn cleanup_entries(
        &mut self,
        ctx: &mut AppContext,
        entries: impl IntoIterator<Item = ChromeNavEntry>,
    ) {
        for entry in entries {
            if let Some(app) = self.apps.get_mut(entry.app.slot()) {
                app.cleanup_nav(ctx, &entry.token);
            }
        }
    }

    /// Step one entry back in the global history, then re-derive the active app.
    ///
    /// The back is deferred/animated: [`NavStack::go_back`] only flags the
    /// transition, and [`nav_frame`](notedeck::nav_frame) reconciles the pop once
    /// the slide completes — so `active` doesn't change until the following frame
    /// (the outgoing app stays rendered during the slide). No-op at the root.
    /// Shared by the header chevron, `Alt+←`, and the back mouse button.
    pub(super) fn global_go_back(&mut self) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.go_back();
        }
        self.sync_active_from_nav();
    }

    /// Step one entry forward in the global history, replaying the most recently
    /// popped route, then re-derive the active app. Unlike a back step the push
    /// lands immediately, so `active` updates this frame. No-op with an empty
    /// forward stack. Shared by the header chevron, `Alt+→`, and the forward
    /// mouse button.
    pub(super) fn global_go_forward(&mut self) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.go_forward();
        }
        self.sync_active_from_nav();
    }

    /// Jump straight to back-stack `index` in the global history (used by the
    /// header history dropdown), then re-derive the active app. Instant, with the
    /// skipped-over routes preserved on the forward stack for redo.
    ///
    /// Every skipped-over entry was popped, so it goes to its app's
    /// `cleanup_nav` just as a completed back's pop does. That is also what
    /// lets a later prune drop forward-stack entries without cleaning them
    /// ([`NavStack::retain_routes`]): they were cleaned on the way there.
    pub(super) fn global_go_to(&mut self, ctx: &mut AppContext, index: usize) {
        let popped = self
            .global_nav
            .as_mut()
            .map(|nav| nav.go_to_route(index))
            .unwrap_or_default();
        self.sync_active_from_nav();
        self.cleanup_entries(ctx, popped);
    }
}

// The global-nav wiring exercised here is independent of the auto-update
// `updater` field, which needs a live egui/ndb context to build. Gating on
// `not(auto-update)` lets these tests construct a bare `Chrome` with no context
// (the field is compiled out) while still running under the default feature set
// CI uses for `notedeck_chrome`. Most drive the stack's reconcile by hand;
// `slide_tests` draws it through the real egui-nav, which a drag needs.
#[cfg(all(test, not(feature = "auto-update")))]
mod global_nav_tests {
    use super::*;
    use crate::chrome::keyboard::AnimState;
    use crate::chrome::MAX_APPS;
    use crate::ChromeOptions;
    use egui_nav::{NavAction, ReturnType};
    use notedeck::ActiveNavEntry;
    use notedeck::DrawerRouter;
    use std::collections::HashMap;

    /// Build a bare chrome with a seeded global history and no apps — enough to
    /// drive the stack machinery (`set_active`, the `Navigator` drain, and the
    /// active-app derive) without an egui/ndb context. Mirrors the real
    /// constructors' seeding via [`seed_global_nav`].
    fn nav_test_chrome() -> Chrome {
        Chrome {
            active: 0,
            options: ChromeOptions::default(),
            apps: Vec::new(),
            opened: [0u16; MAX_APPS / 16],
            app_focus: HashMap::new(),
            prev_active: 0,
            tools_snapshot: [0u16; MAX_APPS / 16],
            soft_kb_anim_state: AnimState::default(),
            repaint_causes: HashMap::new(),
            nav: DrawerRouter::default(),
            global_nav: Some(seed_global_nav()),
            pending_open: None,
            pending_prunes: Vec::new(),
            global_nav_in_flight: false,
        }
    }

    /// Land the global history's in-flight forward slide, as `nav_frame`
    /// does once egui-nav reports it placed.
    fn land_slide(chrome: &mut Chrome) {
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Navigated);
    }

    /// The app slot and `u32` token of every entry on the history, oldest first.
    fn history(chrome: &Chrome) -> Vec<(usize, Option<u32>)> {
        chrome
            .global_nav
            .as_ref()
            .unwrap()
            .routes()
            .iter()
            .map(|e| (e.app.slot(), e.token.downcast_ref::<u32>().copied()))
            .collect()
    }

    /// Apply `requests` that remove nothing (no prune among them, none held),
    /// as a frame's drain does.
    fn navigate(chrome: &mut Chrome, requests: Vec<NavRequest>) {
        let removed = chrome.apply_nav_requests(requests);
        assert!(removed.is_empty(), "nothing here removes an entry");
    }

    /// A prune request for the active app's `u32` tokens equal to `dead`.
    fn prune(dead: u32) -> NavRequest {
        let mut navigator = notedeck::Navigator::default();
        navigator.remove_active_routes(move |t: &u32| *t == dead);
        navigator.take().pop().unwrap()
    }

    #[test]
    fn seed_starts_at_app_zero() {
        let chrome = nav_test_chrome();
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 1);
        assert_eq!(nav.top().app, AppId(0));
        assert_eq!(chrome.active, 0);
    }

    #[test]
    fn set_active_pushes_a_route_and_derives_active() {
        let mut chrome = nav_test_chrome();

        chrome.set_active(1);
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 2, "switching apps pushes a global-history entry");
        assert_eq!(nav.top().app, AppId(1));
        assert_eq!(chrome.active, 1);
        assert!(
            nav.navigating(),
            "route_to arms the forward-transition flag"
        );
        assert!(chrome.is_opened(1), "the switched-to app is marked opened");
    }

    #[test]
    fn set_active_guards_the_no_op_switch() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        assert_eq!(chrome.global_nav.as_ref().unwrap().len(), 2);

        // switching to the already-active app must not push a duplicate entry
        chrome.set_active(1);
        assert_eq!(chrome.global_nav.as_ref().unwrap().len(), 2);
        assert_eq!(chrome.active, 1);
    }

    #[test]
    fn drained_push_request_advances_the_stack() {
        let mut chrome = nav_test_chrome();

        navigate(
            &mut chrome,
            vec![NavRequest::Push(ChromeNavEntry::new(AppId(2), Rc::new(())))],
        );

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.top().app, AppId(2));
        assert_eq!(chrome.active, 2, "active derives from the new stack top");
    }

    #[test]
    fn drained_replace_request_drops_the_previous_route() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // [app0, app1]

        navigate(
            &mut chrome,
            vec![NavRequest::Replace(ChromeNavEntry::new(
                AppId(2),
                Rc::new(()),
            ))],
        );
        // route_to_replaced defers the drop until the transition completes
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Navigated);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 1, "replace collapses the history to the new top");
        assert_eq!(nav.top().app, AppId(2));
        assert_eq!(chrome.active, 2);
    }

    #[test]
    fn drained_active_push_is_tagged_with_the_active_app() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(3); // active app is now slot 3

        // An app self-pushing carries only its token; the chrome fills in the
        // active slot on drain.
        navigate(
            &mut chrome,
            vec![NavRequest::PushToActive(ActiveNavEntry::new(Rc::new(7u32)))],
        );

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 3, "[app0, app3, app3-route]");
        assert_eq!(
            nav.top().app,
            AppId(3),
            "the self-push inherited the active slot"
        );
        assert_eq!(nav.top().token.downcast_ref::<u32>(), Some(&7));
        assert_eq!(chrome.active, 3);
    }

    #[test]
    fn drained_active_replace_is_tagged_with_the_active_app() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(2); // [app0, app2], active app2

        navigate(
            &mut chrome,
            vec![NavRequest::ReplaceActive(ActiveNavEntry::new(Rc::new(
                9u32,
            )))],
        );
        // route_to_replaced defers the drop until the transition completes.
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Navigated);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 1, "replace collapses the history to the new top");
        assert_eq!(
            nav.top().app,
            AppId(2),
            "the replacement kept the active slot"
        );
        assert_eq!(nav.top().token.downcast_ref::<u32>(), Some(&9));
        assert_eq!(chrome.active, 2);
    }

    #[test]
    fn note_route_push_adds_exactly_one_entry() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // the source app, e.g. Dave: [app0, app1]

        // A cross-app open lands the target's routed entry directly — the matched
        // pair to `drained_active_push_is_tagged_with_the_active_app`, which is the
        // switch-then-self-push shape that took two back presses.
        chrome.push_app_route(2, Rc::new(7u32));

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 3, "[app0, app1, app2-route]: one entry, not two");
        assert_eq!(
            nav.top().app,
            AppId(2),
            "tagged with the target, not the source"
        );
        assert_eq!(nav.top().token.downcast_ref::<u32>(), Some(&7));
        assert_eq!(chrome.active, 2);
        assert!(chrome.is_opened(2), "the opened app is marked opened");

        // ONE back returns to the source app.
        chrome.global_go_back();
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Returned(ReturnType::Click));
        chrome.sync_active_from_nav();

        assert_eq!(chrome.global_nav.as_ref().unwrap().top().app, AppId(1));
        assert_eq!(
            chrome.active, 1,
            "one back press crossed back to the source"
        );
    }

    #[test]
    fn global_back_and_forward_cross_app_boundaries() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // [app0, app1], active app1

        // A back navigation only pops once the transition completes; the chrome
        // reconciles that in `nav_frame`, so drive the same reconcile here.
        navigate(&mut chrome, vec![NavRequest::Back]);
        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Returned(ReturnType::Click));
        chrome.sync_active_from_nav();

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.top().app, AppId(0));
        assert_eq!(chrome.active, 0, "back crossed the boundary to app0");

        // Forward replays app1 and re-derives it as active immediately.
        navigate(&mut chrome, vec![NavRequest::Forward]);
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.top().app, AppId(1));
        assert_eq!(chrome.active, 1, "forward crossed back to app1");
    }

    #[test]
    fn drained_prune_is_tagged_with_the_active_app_and_rederives_active() {
        let mut chrome = nav_test_chrome();
        // [app0, app2:7, app1:7] — the same token value under two apps.
        navigate(
            &mut chrome,
            vec![
                NavRequest::Push(ChromeNavEntry::new(AppId(2), Rc::new(7u32))),
                NavRequest::Push(ChromeNavEntry::new(AppId(1), Rc::new(7u32))),
            ],
        );
        land_slide(&mut chrome);
        assert_eq!(chrome.active, 1);

        let removed = chrome.apply_nav_requests(vec![prune(7)]);

        // Only app1's entry went: the prune was tagged with the active slot.
        assert_eq!(history(&chrome), vec![(0, None), (2, Some(7))]);
        assert_eq!(removed.len(), 1, "the removed entry comes back for cleanup");
        assert_eq!(removed[0].app, AppId(1));
        assert_eq!(removed[0].token.downcast_ref::<u32>(), Some(&7));

        // The top was removed, so the landing is instant and on app2's entry.
        let nav = chrome.global_nav.as_ref().unwrap();
        assert!(!nav.navigating() && !nav.returning());
        assert_eq!(chrome.active, 2, "active follows the new top across apps");
        assert!(chrome.pending_prunes.is_empty());
    }

    #[test]
    fn prune_during_a_forward_slide_waits_for_it_to_land() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        land_slide(&mut chrome);

        // A push starts a slide, and the prune in the same batch must wait.
        let removed = chrome.apply_nav_requests(vec![
            NavRequest::PushToActive(ActiveNavEntry::new(Rc::new(7u32))),
            prune(7),
        ]);
        assert!(removed.is_empty());
        assert_eq!(history(&chrome), vec![(0, None), (1, None), (1, Some(7))]);
        assert_eq!(chrome.pending_prunes.len(), 1);

        // A frame with no requests while still sliding changes nothing.
        assert!(chrome.apply_nav_requests(Vec::new()).is_empty());
        assert_eq!(chrome.pending_prunes.len(), 1);

        // Once the slide lands, the next drain applies the held prune.
        land_slide(&mut chrome);
        let removed = chrome.apply_nav_requests(Vec::new());
        assert_eq!(removed.len(), 1);
        assert_eq!(history(&chrome), vec![(0, None), (1, None)]);
        assert!(chrome.pending_prunes.is_empty());
        assert_eq!(chrome.active, 1);
    }

    #[test]
    fn prune_during_a_back_slide_waits_for_the_pop() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        navigate(
            &mut chrome,
            vec![
                NavRequest::PushToActive(ActiveNavEntry::new(Rc::new(7u32))),
                NavRequest::PushToActive(ActiveNavEntry::new(Rc::new(8u32))),
            ],
        );
        land_slide(&mut chrome);

        // Back off 8 and prune 7 in one batch: the prune waits for the pop.
        navigate(&mut chrome, vec![NavRequest::Back, prune(7)]);
        assert_eq!(
            history(&chrome),
            vec![(0, None), (1, None), (1, Some(7)), (1, Some(8))],
            "nothing moves under egui-nav while the back slides"
        );

        chrome
            .global_nav
            .as_mut()
            .unwrap()
            .reconcile(NavAction::Returned(ReturnType::Click));
        let removed = chrome.apply_nav_requests(Vec::new());

        // 8 was popped by the back, then 7 pruned: one step lands on app1's root.
        assert_eq!(removed.len(), 1);
        assert_eq!(history(&chrome), vec![(0, None), (1, None)]);
        assert_eq!(chrome.active, 1);
    }

    #[test]
    fn a_held_prune_applies_before_the_batch_it_meets() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1);
        land_slide(&mut chrome);

        // Push 7 and prune it in one batch: the prune waits for the slide.
        navigate(
            &mut chrome,
            vec![
                NavRequest::PushToActive(ActiveNavEntry::new(Rc::new(7u32))),
                prune(7),
            ],
        );
        land_slide(&mut chrome);

        // The next batch pushes a fresh 7. The held prune was raised before
        // it, so it takes only the old 7; applied after the batch, it would
        // take the new one too.
        let removed = chrome.apply_nav_requests(vec![NavRequest::PushToActive(
            ActiveNavEntry::new(Rc::new(7u32)),
        )]);
        assert_eq!(removed.len(), 1, "only the old 7 went");
        assert_eq!(history(&chrome), vec![(0, None), (1, None), (1, Some(7))]);
        assert!(chrome.pending_prunes.is_empty());
    }

    // Driven through the real egui-nav, a drag included. Release only, as
    // `headway_nav_tests` is and for the same reason: mid-transition egui-nav
    // draws the routes on two layers under one widget id, which egui
    // debug-asserts against. CI's release nav step runs these.
    #[cfg(not(debug_assertions))]
    mod slide_tests {
        use super::*;

        /// A bare chrome's global history drawn through the real egui-nav, one
        /// harness step per frame, as the chrome's frame draws it: `nav_frame`,
        /// then record what it reported, then drain this frame's requests.
        struct DragRig {
            chrome: Chrome,
            /// Requests to drain at the end of the next frame, as an app would
            /// have queued them while rendering.
            requests: Vec<NavRequest>,
            /// Entries a completed back popped inside `nav_frame`.
            popped: Vec<ChromeNavEntry>,
            /// Entries a drained prune removed.
            removed: Vec<ChromeNavEntry>,
        }

        fn drag_rig_frame(ui: &mut egui::Ui, rig: &mut DragRig) {
            let nav = rig.chrome.global_nav.as_mut().unwrap();
            let frame =
                notedeck::nav_frame(ui, egui::Id::unique("drag_rig"), nav, true, |_, _, _| {
                    egui_nav::RouteResponse {
                        response: (),
                        can_take_drag_from: Vec::new(),
                    }
                });
            rig.chrome.global_nav_in_flight = frame.in_flight;
            if let Some(notedeck::NavStackEvent::Popped {
                route: Some(entry), ..
            }) = frame.event
            {
                rig.popped.push(entry);
            }
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
            // [app0, app1:7, app1:8]: the user drags 8 back towards 7.
            navigate(
                &mut chrome,
                vec![
                    NavRequest::Push(ChromeNavEntry::new(AppId(1), Rc::new(7u32))),
                    NavRequest::Push(ChromeNavEntry::new(AppId(1), Rc::new(8u32))),
                ],
            );
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
            let nav = harness.state().chrome.global_nav.as_ref().unwrap();
            assert!(!nav.navigating() && !harness.state().chrome.global_nav_in_flight);

            // Press and drag rightwards: a drag-back, which sets neither of the
            // stack's own transition flags.
            let mut pos = egui::pos2(100.0, 300.0);
            pointer(&mut harness, egui::Event::PointerMoved(pos));
            pointer(&mut harness, press(pos, true));
            for _ in 0..5 {
                pos.x += 20.0;
                pointer(&mut harness, egui::Event::PointerMoved(pos));
            }
            let nav = harness.state().chrome.global_nav.as_ref().unwrap();
            assert!(!nav.navigating() && !nav.returning());
            assert!(
                harness.state().chrome.global_nav_in_flight,
                "egui-nav reports the drag"
            );

            // The app prunes the dragged entry this frame. It must wait: the
            // drag is still drawing it.
            harness.state_mut().requests.push(prune(8));
            pos.x += 20.0;
            pointer(&mut harness, egui::Event::PointerMoved(pos));
            assert_eq!(
                history(&harness.state().chrome),
                vec![(0, None), (1, Some(7)), (1, Some(8))],
                "nothing moves under egui-nav mid-drag"
            );
            assert_eq!(harness.state().chrome.pending_prunes.len(), 1);

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
            assert_eq!(history(&state.chrome), vec![(0, None), (1, Some(7))]);
            assert_eq!(state.popped.len(), 1);
            assert_eq!(state.popped[0].token.downcast_ref::<u32>(), Some(&8));
            assert!(state.removed.is_empty());
            assert!(state.chrome.pending_prunes.is_empty());
            let nav = state.chrome.global_nav.as_ref().unwrap();
            assert!(!nav.can_go_forward(), "the pruned 8 can't be redone");
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
    use crate::chrome::keyboard::AnimState;
    use crate::chrome::MAX_APPS;
    use crate::ChromeOptions;
    use egui_nav::NavAction;
    use notedeck::{AppResponse, DrawerRouter, Notedeck};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// An app whose `cleanup_nav` records each `u32` token it is handed.
    struct CleanupRecorder {
        cleaned: Rc<RefCell<Vec<u32>>>,
    }

    impl App for CleanupRecorder {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }

        fn cleanup_nav(&mut self, _ctx: &mut AppContext<'_>, token: &Rc<dyn std::any::Any>) {
            if let Some(token) = token.downcast_ref::<u32>() {
                self.cleaned.borrow_mut().push(*token);
            }
        }
    }

    /// A notedeck to lend an `AppContext`, and a bare chrome whose slot 0 is
    /// a [`CleanupRecorder`] sharing `cleaned`.
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
            let chrome = Chrome {
                active: 0,
                options: ChromeOptions::default(),
                apps: vec![NotedeckApp::Other("recorder".into(), Box::new(recorder))],
                opened: [0u16; MAX_APPS / 16],
                app_focus: HashMap::new(),
                prev_active: 0,
                tools_snapshot: [0u16; MAX_APPS / 16],
                soft_kb_anim_state: AnimState::default(),
                repaint_causes: HashMap::new(),
                nav: DrawerRouter::default(),
                global_nav: Some(seed_global_nav()),
                pending_open: None,
                pending_prunes: Vec::new(),
                global_nav_in_flight: false,
            };
            Self {
                _dir: dir,
                notedeck,
                chrome,
                cleaned,
            }
        }

        /// Push the recorder's `tokens` in order and land the slide:
        /// `[app0, 0:t0, 0:t1, ..]`.
        fn push(&mut self, tokens: &[u32]) {
            let requests = tokens
                .iter()
                .map(|t| NavRequest::Push(ChromeNavEntry::new(AppId(0), Rc::new(*t))))
                .collect();
            let removed = self.chrome.apply_nav_requests(requests);
            assert!(removed.is_empty());
            self.chrome
                .global_nav
                .as_mut()
                .unwrap()
                .reconcile(NavAction::Navigated);
        }

        /// Drain one frame's queued prune of the recorder's token `dead`, as
        /// the chrome's `render` does.
        fn drain_prune(&mut self, dead: u32) {
            let mut ctx = self.notedeck.app_context();
            ctx.navigator
                .remove_active_routes(move |t: &u32| *t == dead);
            self.chrome.drain_nav_requests(&mut ctx);
        }

        fn tokens(&self) -> Vec<Option<u32>> {
            self.chrome
                .global_nav
                .as_ref()
                .unwrap()
                .routes()
                .iter()
                .map(|e| e.token.downcast_ref::<u32>().copied())
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
        fx.chrome.global_go_to(&mut ctx, 1);
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
}
