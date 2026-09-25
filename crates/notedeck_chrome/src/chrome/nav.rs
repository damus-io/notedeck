//! The chrome's global navigation history: seeding it, recording app
//! switches and cross-app opens as entries, applying the requests apps queue
//! through the [`Navigator`](notedeck::Navigator), global back / forward /
//! jump, and re-deriving the active app from the stack top.

use super::Chrome;
#[cfg(feature = "headway")]
use notedeck::{App, AppContext};
use notedeck::{AppId, ChromeNavEntry, NavRequest, NavStack};
use std::rc::Rc;

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
    /// the chrome applies it here. Empty in practice until an app starts pushing
    /// routes (a later subissue), but wired now so it is ready and testable.
    pub(super) fn apply_nav_requests(&mut self, requests: Vec<NavRequest>) {
        if requests.is_empty() {
            return;
        }

        // The active-owned requests (`PushToActive`/`ReplaceActive`) don't name
        // their app — the enqueuing app doesn't know its own slot — so the chrome
        // completes them here by tagging the token with the active slot. This
        // runs during the same frame's render as the enqueue and before
        // `sync_active_from_nav`, so `active` still names the app that raised the
        // request (a plain app-switch funnels through `set_active`, not here).
        let active = AppId(self.active as usize);

        if let Some(nav) = self.global_nav.as_mut() {
            for request in requests {
                match request {
                    NavRequest::Push(entry) => nav.route_to(entry),
                    NavRequest::Replace(entry) => nav.route_to_replaced(entry),
                    NavRequest::PushToActive(entry) => nav.route_to(entry.tag(active)),
                    NavRequest::ReplaceActive(entry) => nav.route_to_replaced(entry.tag(active)),
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
    pub(super) fn global_go_to(&mut self, index: usize) {
        if let Some(nav) = self.global_nav.as_mut() {
            nav.go_to_route(index);
        }
        self.sync_active_from_nav();
    }
}

// The global-nav wiring exercised here is independent of the auto-update
// `updater` field, which needs a live egui/ndb context to build. Gating on
// `not(auto-update)` lets these tests construct a bare `Chrome` with no context
// (the field is compiled out) while still running under the default feature set
// CI uses for `notedeck_chrome`. The render half (the `nav_frame` body) is
// exercised by compilation; the reconcile state machine it drives is covered by
// `notedeck::nav`'s own `NavStack` tests.
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
        }
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

        chrome.apply_nav_requests(vec![NavRequest::Push(ChromeNavEntry::new(
            AppId(2),
            Rc::new(()),
        ))]);

        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.len(), 2);
        assert_eq!(nav.top().app, AppId(2));
        assert_eq!(chrome.active, 2, "active derives from the new stack top");
    }

    #[test]
    fn drained_replace_request_drops_the_previous_route() {
        let mut chrome = nav_test_chrome();
        chrome.set_active(1); // [app0, app1]

        chrome.apply_nav_requests(vec![NavRequest::Replace(ChromeNavEntry::new(
            AppId(2),
            Rc::new(()),
        ))]);
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
        chrome.apply_nav_requests(vec![NavRequest::PushToActive(ActiveNavEntry::new(
            Rc::new(7u32),
        ))]);

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

        chrome.apply_nav_requests(vec![NavRequest::ReplaceActive(ActiveNavEntry::new(
            Rc::new(9u32),
        ))]);
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
        chrome.apply_nav_requests(vec![NavRequest::Back]);
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
        chrome.apply_nav_requests(vec![NavRequest::Forward]);
        let nav = chrome.global_nav.as_ref().unwrap();
        assert_eq!(nav.top().app, AppId(1));
        assert_eq!(chrome.active, 1, "forward crossed back to app1");
    }
}
