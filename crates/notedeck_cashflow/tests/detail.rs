//! A transaction's detail: opened from a register row or an inbox row as an
//! entry on the chrome's nav stack, drawn cold from that entry, and left by
//! its own back, Escape, or a view picked from the rail.
//!
//! The nav requests are read off the harness host's `Navigator`, which the
//! chrome would drain after the frame. The override and the split are
//! written through the vault before the app's first load
//! (`common::detail_harness`), so the detail traces them to their notes.
//! The snapshots are `#[ignore]`d and run via `scripts/snapshot-test`
//! (lavapipe).

mod common;

use std::any::Any;
use std::rc::Rc;

use common::{
    CashflowTestState, DESKTOP, DETAIL_AGENTIUM, MOBILE, detail_fixture, detail_harness, harness,
    snapshot_harness, triage_app,
};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use notedeck::NavRequest;
use notedeck_cashflow::{CashflowApp, CashflowRoute, TxnRoute, View};

/// A window tall enough for every group of the triage journal.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1100.0);

/// The first coffee's payee, `card:1`.
const COFFEE: &str = "SQ *COFFEE SHOP #12";

/// The route a detail entry for `record` titled `title` carries.
fn txn_route(record: &str, title: &str) -> CashflowRoute {
    CashflowRoute::Txn(TxnRoute {
        record: record.to_owned(),
        title: title.to_owned(),
    })
}

/// The route of the one push among `requests`.
fn pushed(requests: &[NavRequest]) -> &CashflowRoute {
    let pushes: Vec<&Rc<dyn Any>> = requests
        .iter()
        .filter_map(|r| match r {
            NavRequest::Push(token) => Some(token),
            _ => None,
        })
        .collect();
    assert_eq!(pushes.len(), 1, "one push");
    pushes[0]
        .downcast_ref::<CashflowRoute>()
        .expect("a cashflow route")
}

/// A harness over the triage journal with `record`'s detail open, as a
/// row's click would leave it, and the nav queue empty.
fn open_on(record: &str) -> Harness<'static, CashflowTestState> {
    let mut app = triage_app();
    app.set_open_txn(Some(record));
    let mut harness = harness(app, DESKTOP);
    harness.state_mut().take_nav();
    harness
}

/// A register row opens its detail and pushes one entry naming it by
/// record id, titled "payee · date".
#[test]
fn a_register_row_opens_its_detail() {
    let mut harness = harness(triage_app(), DESKTOP);
    harness.state_mut().take_nav();
    harness.get_by_role_and_label(Role::Button, COFFEE).click();
    harness.run();

    assert_eq!(harness.state().app.open_txn(), Some("card:1"));
    assert!(harness.query_by_label("Uncategorized").is_some());
    assert!(harness.query_by_label("← Transactions").is_some());
    let requests = harness.state_mut().take_nav();
    assert_eq!(
        pushed(&requests),
        &txn_route("card:1", "SQ *COFFEE SHOP #12 · 2024-10-02")
    );
}

/// The entry's token alone opens the detail: what the chrome hands
/// `render_nav` on a back or forward onto it.
#[test]
fn render_nav_draws_the_detail_cold() {
    let mut harness = harness(triage_app(), DESKTOP);
    let token: Rc<dyn Any> = Rc::new(txn_route("card:4", "Bookshop · 2024-10-06"));
    harness.state_mut().route = Some(token);
    harness.run();

    assert_eq!(harness.state().app.open_txn(), Some("card:4"));
    assert!(harness.query_by_label("Bookshop").is_some());
    // Seeded once, so it queues nothing: the entry is already there.
    assert!(harness.state_mut().take_nav().is_empty());

    // The root token shows the view again.
    harness.state_mut().route = Some(Rc::new(()));
    harness.run();
    assert_eq!(harness.state().app.open_txn(), None);
}

/// The detail's history title is the snapshotted "payee · date"; the root
/// has none, so the chrome says "Cashflow".
#[test]
fn nav_title_is_the_snapshot() {
    use notedeck::App;
    let app = CashflowApp::new();
    let token: Rc<dyn Any> = Rc::new(txn_route("card:4", "Bookshop · 2024-10-06"));
    assert_eq!(
        app.nav_title(&token).as_deref(),
        Some("Bookshop · 2024-10-06")
    );
    assert_eq!(app.nav_title(&(Rc::new(()) as Rc<dyn Any>)), None);
}

/// The "← Transactions" back closes the detail and steps back.
#[test]
fn the_back_button_steps_back() {
    let mut harness = open_on("card:1");
    harness.get_by_label("← Transactions").click();
    harness.run();

    assert_eq!(harness.state().app.open_txn(), None);
    let requests = harness.state_mut().take_nav();
    assert!(matches!(requests[..], [NavRequest::Back]));
}

/// Escape is the same back.
#[test]
fn escape_steps_back() {
    let mut harness = open_on("card:1");
    harness.key_press(egui::Key::Escape);
    harness.run();

    assert_eq!(harness.state().app.open_txn(), None);
    let requests = harness.state_mut().take_nav();
    assert!(matches!(requests[..], [NavRequest::Back]));
}

/// A view picked from the rail over a detail switches to it and drops every
/// detail entry.
#[test]
fn a_rail_click_drops_the_detail_entries() {
    let mut harness = open_on("card:1");
    harness
        .get_by_role_and_label(Role::Button, "Overview")
        .click();
    harness.run();

    assert_eq!(harness.state().app.view(), View::Overview);
    assert_eq!(harness.state().app.open_txn(), None);
    let requests = harness.state_mut().take_nav();
    assert!(matches!(requests[..], [NavRequest::Remove(_)]));
    let NavRequest::Remove(dead) = &requests[0] else {
        unreachable!()
    };
    assert!(dead(&txn_route("card:1", "") as &dyn Any));
    assert!(!dead(&() as &dyn Any));
}

/// A record the loaded books don't hold says so.
#[test]
fn an_unknown_record_shows_the_placeholder() {
    let harness = open_on("card:404");
    assert!(
        harness
            .query_by_label("Not in the loaded journal (is its layer off?)")
            .is_some()
    );
    assert!(harness.query_by_label("card:404").is_some());
}

/// "Show payee in register" lists that payee's transactions, whole, and
/// leaves the detail.
#[test]
fn show_payee_filters_the_register() {
    let mut app = triage_app();
    app.set_view(View::Overview);
    app.set_open_txn(Some("card:1"));
    let mut harness = harness(app, DESKTOP);
    harness.state_mut().take_nav();
    harness.get_by_label("Show payee in register").click();
    harness.run();

    assert_eq!(harness.state().app.view(), View::Transactions);
    assert_eq!(harness.state().app.open_txn(), None);
    assert!(harness.query_by_label("Exact payee ✕").is_some());
    assert!(harness.query_by_label("1 transaction").is_some());
    let requests = harness.state_mut().take_nav();
    assert!(matches!(requests[..], [NavRequest::Remove(_)]));
}

/// An expanded inbox group's row opens its detail.
#[test]
fn an_inbox_row_opens_its_detail() {
    let mut harness = harness(triage_app(), TALL);
    harness.get_by_label("Inbox").click();
    harness.run();
    harness.get_by_label("sq coffee shop").click();
    harness.run();
    harness.state_mut().take_nav();
    harness.get_by_label_contains(COFFEE).click();
    harness.run();

    assert_eq!(harness.state().app.open_txn(), Some("card:1"));
    let requests = harness.state_mut().take_nav();
    assert_eq!(pushed(&requests).record(), "card:1");
}

/// An override written from an agentium session says so, and its ref
/// opens that session.
#[test]
fn an_override_traces_to_its_session() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:5"));
    harness.run();

    assert!(
        harness
            .query_by_label("Categorized by an override")
            .is_some()
    );
    assert!(
        harness
            .query_by_label("This one transaction goes to expenses:travel")
            .is_some()
    );
    harness
        .get_by_role_and_label(Role::Button, DETAIL_AGENTIUM)
        .click();
    harness.run();
    assert_eq!(harness.state().opened, [DETAIL_AGENTIUM]);
}

/// A split lists its legs.
#[test]
fn a_split_lists_its_legs() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:2"));
    harness.run();

    assert!(harness.query_by_label("Split into 2 parts").is_some());
    // Each leg, in the why and among the postings.
    assert!(
        harness
            .query_by_label("expenses:household  24.20 CAD")
            .is_some()
    );
    assert!(harness.query_by_label("expenses:household").is_some());
}

/// An uncategorized coffee's detail, wide: facts on the left, actions on
/// the right.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail() {
    let mut app = triage_app();
    app.set_open_txn(Some("card:1"));
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.snapshot("cashflow_detail");
}

/// The same on a phone: one column.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_mobile() {
    let mut app = triage_app();
    app.set_open_txn(Some("card:1"));
    let mut harness = snapshot_harness(app, MOBILE);
    harness.snapshot("cashflow_detail_mobile");
}

/// The airline tickets, overridden from an agentium session.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_override() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    harness.state_mut().app.set_open_txn(Some("card:5"));
    harness.run();
    harness.snapshot("cashflow_detail_override");
}

/// The Corner Grocer run, split in two.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_split() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    harness.state_mut().app.set_open_txn(Some("card:2"));
    harness.run();
    harness.snapshot("cashflow_detail_split");
}
