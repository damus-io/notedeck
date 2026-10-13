//! Cross-app navigation scenarios: what back, forward and an app switch do
//! today, and what they should do under per-app nav stacks.
//!
//! headway:dave/fresh-champion-crucial mapped the chrome's one global history
//! and settled the model; headway:dave/step-neglect-awful builds it. The
//! settled model, in short:
//!
//! - Each app keeps its own back / forward stack. Switching apps (sidebar,
//!   tab strip, Ctrl-Tab) is not a history entry: it lands on that app's
//!   stack top, where it was left.
//! - Back never leaves the app, with one exception: a cross-app open (a
//!   chip, Dave's `s`) pushes onto the target app's stack and remembers its
//!   opener, and the chrome's back on that entry returns to the opener. The
//!   hop is forgotten once you leave the app by a switch.
//! - An app's own back (Headway's Esc closing a card) is always in-app.
//! - Closing an app (Ctrl-W) drops its stack.
//!
//! The tests drive a bare chrome over three stand-in apps, so they say
//! nothing about any one app's rendering: a `u32` token stands for a route
//! such as a Headway card's detail, and no token for an app's root (the
//! `()` entry, which Headway draws as its board grid). The `#[ignore]`d ones
//! state the settled model and fail today; the step-neglect-awful card
//! rewrites them against its API and turns them on. The rest pin behaviour
//! that holds today and must survive the change.

use super::Chrome;
use crate::app::NotedeckApp;
use egui_nav::{NavAction, ReturnType};
use notedeck::{ActiveNavEntry, App, AppContext, AppResponse, NavRequest};
use std::rc::Rc;

/// The stand-in apps' slots.
const COLUMNS: usize = 0;
const DAVE: usize = 1;
const HEADWAY: usize = 2;

/// Two Headway cards, as route tokens.
const CARD_X: u32 = 7;
const CARD_Y: u32 = 8;

/// An app that draws nothing; the scenarios only read the history.
struct StandIn;

impl App for StandIn {
    fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
        AppResponse::none()
    }
}

/// A bare chrome over Columns, Dave and Headway stand-ins, on Columns.
fn chrome() -> Chrome {
    let apps = ["columns", "dave", "headway"]
        .into_iter()
        .map(|name| NotedeckApp::Other(name.into(), Box::new(StandIn)))
        .collect();
    let mut chrome = Chrome::without_context(apps);
    chrome.set_active(COLUMNS as i32);
    chrome
}

/// What the chrome draws: the active app, and the route token on its entry
/// (`None` for the app's root).
#[derive(Debug, PartialEq)]
struct Showing {
    app: usize,
    route: Option<u32>,
}

fn showing(app: usize, route: Option<u32>) -> Showing {
    Showing { app, route }
}

fn now(chrome: &Chrome) -> Showing {
    let top = chrome.global_nav.as_ref().unwrap().top();
    assert_eq!(top.app.slot(), chrome.active as usize, "active is the top");
    showing(
        chrome.active as usize,
        top.token.downcast_ref::<u32>().copied(),
    )
}

/// Land whatever slide the last step started, as `nav_frame` does once
/// egui-nav reports it placed.
fn land(chrome: &mut Chrome) {
    let nav = chrome.global_nav.as_mut().unwrap();
    if nav.navigating() {
        nav.reconcile(NavAction::Navigated);
    } else if nav.returning() {
        nav.reconcile(NavAction::Returned(ReturnType::Click));
    }
    chrome.sync_active_from_nav();
}

/// Switch apps from the sidebar, the tab strip or Ctrl-Tab.
fn switch(chrome: &mut Chrome, app: usize) {
    chrome.set_active(app as i32);
    land(chrome);
}

/// The active app drills in to `route` (Headway opening a card).
fn drill(chrome: &mut Chrome, route: u32) {
    let push = NavRequest::PushToActive(ActiveNavEntry::new(Rc::new(route)));
    assert!(chrome.apply_nav_requests(vec![push]).is_empty());
    land(chrome);
}

/// The active app steps back itself (Headway's Esc closing a card).
fn app_back(chrome: &mut Chrome) {
    assert!(chrome.apply_nav_requests(vec![NavRequest::Back]).is_empty());
    land(chrome);
}

/// The chrome's back: Alt-←, the header chevron, the mouse's back button.
fn back(chrome: &mut Chrome) {
    chrome.global_go_back();
    land(chrome);
}

/// Open `route` in `app` from another app, as a click on its inline chip
/// does through `open_note_route`.
fn open_from_elsewhere(chrome: &mut Chrome, app: usize, route: u32) {
    chrome.push_app_route(app, Rc::new(route));
    land(chrome);
}

fn can_go_back(chrome: &Chrome) -> bool {
    chrome.global_nav.as_ref().unwrap().can_go_back()
}

// Today's behaviour that the model keeps.

/// A chip in Dave opens Headway's card as one entry, and one back returns
/// to Dave: the opener hop.
#[test]
fn a_cross_app_open_takes_one_back_to_return_to_the_opener() {
    let mut chrome = chrome();
    switch(&mut chrome, DAVE);
    open_from_elsewhere(&mut chrome, HEADWAY, CARD_X);
    assert_eq!(now(&chrome), showing(HEADWAY, Some(CARD_X)));

    back(&mut chrome);
    assert_eq!(now(&chrome), showing(DAVE, None));
}

/// Inside one app, drills walk back the way they came.
#[test]
fn drills_inside_an_app_walk_back_the_way_they_came() {
    let mut chrome = chrome();
    switch(&mut chrome, HEADWAY);
    drill(&mut chrome, CARD_X);
    drill(&mut chrome, CARD_Y);

    back(&mut chrome);
    assert_eq!(now(&chrome), showing(HEADWAY, Some(CARD_X)));
    app_back(&mut chrome);
    assert_eq!(now(&chrome), showing(HEADWAY, None));
}

// The settled model; each fails today, as its doc says.

/// Leave Headway on card X for Dave and come back: you're on card X.
///
/// Today the switch back pushes a fresh `()` entry, which Headway draws as
/// its board grid, and card X is two backs away, behind Dave.
#[test]
#[ignore = "per-app nav stacks, headway:dave/step-neglect-awful"]
fn switching_back_to_an_app_lands_where_it_was_left() {
    let mut chrome = chrome();
    switch(&mut chrome, HEADWAY);
    drill(&mut chrome, CARD_X);
    switch(&mut chrome, DAVE);
    switch(&mut chrome, HEADWAY);

    assert_eq!(now(&chrome), showing(HEADWAY, Some(CARD_X)));
}

/// Back on an app's root, reached by a switch, goes nowhere.
///
/// Today every switch is an entry, so back walks the switches: Headway,
/// Dave, Headway, Dave, Columns, one app per press.
#[test]
#[ignore = "per-app nav stacks, headway:dave/step-neglect-awful"]
fn back_never_walks_back_through_app_switches() {
    let mut chrome = chrome();
    for app in [HEADWAY, DAVE, HEADWAY, DAVE] {
        switch(&mut chrome, app);
    }

    assert!(!can_go_back(&chrome), "Dave's root has nothing behind it");
    back(&mut chrome);
    assert_eq!(now(&chrome), showing(DAVE, None));
}

/// A card opened from Dave and then closed in Headway (Esc, the ×) lands on
/// Headway's board, not back in Dave.
///
/// Today Headway's close is a global back, and the entry under the opened
/// card is Dave's, so closing a card leaves Headway.
#[test]
#[ignore = "per-app nav stacks, headway:dave/step-neglect-awful"]
fn closing_a_card_opened_from_another_app_lands_on_its_board() {
    let mut chrome = chrome();
    switch(&mut chrome, HEADWAY);
    switch(&mut chrome, DAVE);
    open_from_elsewhere(&mut chrome, HEADWAY, CARD_X);

    app_back(&mut chrome);
    assert_eq!(now(&chrome), showing(HEADWAY, None));
}

/// The opener hop lasts only until you leave the app some other way: open a
/// card from Dave, switch to Columns and back, and back is in-app.
///
/// Today the switch back to Headway lands on its grid (a new `()` entry),
/// and back from there goes to Columns.
#[test]
#[ignore = "per-app nav stacks, headway:dave/step-neglect-awful"]
fn the_opener_hop_is_forgotten_once_you_switch_away() {
    let mut chrome = chrome();
    switch(&mut chrome, DAVE);
    open_from_elsewhere(&mut chrome, HEADWAY, CARD_X);
    switch(&mut chrome, COLUMNS);
    switch(&mut chrome, HEADWAY);
    assert_eq!(now(&chrome), showing(HEADWAY, Some(CARD_X)));

    back(&mut chrome);
    assert_eq!(now(&chrome), showing(HEADWAY, None));
}

/// Closing an app (Ctrl-W) drops its history: no back leads into it again.
///
/// Today its entries stay in the global history, and backing onto one
/// reopens the closed app's tab.
#[test]
#[ignore = "per-app nav stacks, headway:dave/step-neglect-awful"]
fn closing_an_app_drops_its_history() {
    let mut chrome = chrome();
    switch(&mut chrome, HEADWAY);
    drill(&mut chrome, CARD_X);
    switch(&mut chrome, DAVE);
    switch(&mut chrome, HEADWAY);
    chrome.close_active_app();
    land(&mut chrome);
    assert!(!chrome.is_opened(HEADWAY));

    for _ in 0..4 {
        back(&mut chrome);
        assert_ne!(
            chrome.active as usize, HEADWAY,
            "back led into a closed app"
        );
    }
    assert!(!chrome.is_opened(HEADWAY));
}
