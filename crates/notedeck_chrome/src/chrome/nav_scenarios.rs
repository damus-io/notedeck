//! Cross-app navigation scenarios: what back, forward and an app switch do
//! under per-app nav stacks.
//!
//! headway:dave/fresh-champion-crucial mapped the chrome's old single global
//! history and settled the model; headway:dave/step-neglect-awful built it
//! (see `nav.rs`). In short:
//!
//! - Each app keeps its own back / forward stack. Switching apps (sidebar,
//!   tab strip, Ctrl-Tab) is not a history entry: it lands on that app's
//!   stack top, where it was left.
//! - Back never leaves the app, with one exception: a cross-app open (a
//!   chip, Dave's `s`) remembers its opener, and the chrome's back on the
//!   opened view returns there. The hop is forgotten once you leave the app
//!   by a switch.
//! - An app's own back (Headway's Esc closing a card) is always in-app.
//! - Closing an app (Ctrl-W) drops its stack.
//!
//! The tests drive a bare chrome over three stand-in apps, so they say
//! nothing about any one app's rendering: a `u32` token stands for a route
//! such as a Headway card's detail, and no token for an app's root (the
//! `()` entry, which Headway draws as its board grid).

use super::nav::Blank;
use super::Chrome;
use crate::app::NotedeckApp;
use egui_nav::{NavAction, ReturnType};
use notedeck::NavRequest;
use std::rc::Rc;

/// The stand-in apps' slots.
const COLUMNS: usize = 0;
const DAVE: usize = 1;
const HEADWAY: usize = 2;

/// Two Headway cards, as route tokens.
const CARD_X: u32 = 7;
const CARD_Y: u32 = 8;

/// A bare chrome over Columns, Dave and Headway stand-ins, on Columns.
fn chrome() -> Chrome {
    let apps = ["columns", "dave", "headway"]
        .into_iter()
        .map(|name| NotedeckApp::Other(name.into(), Box::new(Blank)))
        .collect();
    let mut chrome = Chrome::without_context(apps);
    chrome.set_active(COLUMNS as i32);
    chrome
}

/// What the chrome draws: the active app, and the route token on its
/// stack top (`None` for the app's root).
#[derive(Debug, PartialEq)]
struct Showing {
    app: usize,
    route: Option<u32>,
}

fn showing(app: usize, route: Option<u32>) -> Showing {
    Showing { app, route }
}

fn now(chrome: &Chrome) -> Showing {
    let top = chrome.active_nav().unwrap().stack.top();
    showing(chrome.active as usize, top.downcast_ref::<u32>().copied())
}

/// Land whatever slide the last step started on the active app's stack, as
/// `nav_frame` does once egui-nav reports it placed.
fn land(chrome: &mut Chrome) {
    let slot = chrome.active as usize;
    let nav = &mut chrome.app_navs[slot];
    if nav.stack.navigating() {
        nav.stack.reconcile(NavAction::Navigated);
    } else if nav.stack.returning() {
        nav.stack.reconcile(NavAction::Returned(ReturnType::Click));
    }
    nav.forget_stale_opener();
}

/// Switch apps from the sidebar, the tab strip or Ctrl-Tab.
fn switch(chrome: &mut Chrome, app: usize) {
    chrome.set_active(app as i32);
    land(chrome);
}

/// The active app drills in to `route` (Headway opening a card).
fn drill(chrome: &mut Chrome, route: u32) {
    let push = NavRequest::Push(Rc::new(route));
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
    // What it pops would go to `cleanup_nav`; these apps own nothing.
    let _popped = chrome.step_back();
    land(chrome);
}

/// Open `route` in `app` from another app, as a click on its inline chip
/// does through `open_note_route`.
fn open_from_elsewhere(chrome: &mut Chrome, app: usize, route: u32) {
    chrome.push_app_route(app, Rc::new(route));
    land(chrome);
}

/// Open `app` from another app without pushing onto its stack, as Dave's
/// session open from Headway's `s` does.
fn open_app_from_elsewhere(chrome: &mut Chrome, app: usize) {
    let from = chrome.active;
    chrome.set_active(app as i32);
    chrome.remember_opener(from);
}

fn can_go_back(chrome: &Chrome) -> bool {
    let nav = chrome.active_nav().unwrap();
    nav.stack.can_go_back() || nav.hop().is_some()
}

// Behaviour the old global history had, which the model keeps.

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

// What the model changed.

/// Leave Headway on card X for Dave and come back: you're on card X.
///
/// Under the old global history the switch back pushed a fresh `()` entry,
/// which Headway draws as its board grid, and card X was two backs away,
/// behind Dave.
#[test]
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
/// Under the old global history every switch was an entry, so back walked
/// the switches: Headway, Dave, Headway, Dave, Columns, one app per press.
#[test]
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
/// Under the old global history Headway's close was a global back, and the
/// entry under the opened card was Dave's, so closing a card left Headway.
#[test]
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
/// Under the old global history the switch back to Headway landed on its
/// grid (a new `()` entry), and back from there went to Columns.
#[test]
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
/// Under the old global history its entries stayed behind, and backing onto
/// one reopened the closed app's tab.
#[test]
fn closing_an_app_drops_its_history() {
    let mut chrome = chrome();
    switch(&mut chrome, HEADWAY);
    drill(&mut chrome, CARD_X);
    switch(&mut chrome, DAVE);
    switch(&mut chrome, HEADWAY);
    let _dropped = chrome.close_active_app();
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

/// Headway's `s` on a card opens its session in Dave, pushing nothing onto
/// Dave's stack; the chrome's back still lands back on the card.
#[test]
fn an_open_that_pushes_nothing_returns_to_the_card() {
    let mut chrome = chrome();
    switch(&mut chrome, HEADWAY);
    drill(&mut chrome, CARD_X);
    open_app_from_elsewhere(&mut chrome, DAVE);
    assert_eq!(now(&chrome), showing(DAVE, None));
    assert!(can_go_back(&chrome), "the chevron is live: it returns");

    back(&mut chrome);
    assert_eq!(now(&chrome), showing(HEADWAY, Some(CARD_X)));
}
