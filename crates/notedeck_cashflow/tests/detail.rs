//! A transaction's detail: opened from a register row or an inbox row as an
//! entry on the chrome's nav stack, drawn cold from that entry, and left by
//! its own back, Escape, or a view picked from the rail.
//!
//! The nav requests are read off the harness host's `Navigator`, which the
//! chrome would drain after the frame. The override, the split and the
//! link are written through the vault before the app's first load
//! (`common::detail_harness`), so the detail traces them to their notes.
//!
//! Its Categorize section writes through the vault too, each write planned
//! first: the tests wait for the plan, Apply, and read the notes back.
//! The snapshots are `#[ignore]`d and run via `scripts/snapshot-test`
//! (lavapipe).

mod common;

use std::any::Any;
use std::rc::Rc;

use cashflow::kinds;
use common::{
    CashflowTestState, DESKTOP, DETAIL_AGENTIUM, MOBILE, VaultNote, detail_fixture, detail_harness,
    harness, read_vault, snapshot_harness, step_until, triage_app, wait_vault, watch_vault,
};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
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

/// Step `harness` until `label` is on screen: a plan's text, which new
/// books arriving meanwhile (a sync, a rescan) would make again.
fn plan_says(harness: &mut Harness<'static, CashflowTestState>, label: &str) {
    let deadline = std::time::Instant::now() + common::INGEST_TIMEOUT;
    while harness.query_by_label(label).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {label:?}"
        );
        harness.step();
    }
}

/// Type `text` into the last text box on screen: the account picker's,
/// which opens under whatever it picks for.
fn type_last(harness: &mut Harness<'static, CashflowTestState>, text: &str) {
    harness
        .get_all_by_role(Role::TextInput)
        .last()
        .expect("a text box")
        .type_text(text);
    harness.run();
}

/// Focus the text box at `at` among those on screen and type `text`.
fn type_into(harness: &mut Harness<'static, CashflowTestState>, at: usize, text: &str) {
    harness
        .get_all_by_role(Role::TextInput)
        .nth(at)
        .expect("a text box")
        .focus();
    harness.run();
    harness
        .get_all_by_role(Role::TextInput)
        .nth(at)
        .expect("a text box")
        .type_text(text);
    harness.run();
}

/// Whether the button labelled `label` is disabled.
fn disabled(harness: &Harness<'static, CashflowTestState>, label: &str) -> bool {
    harness.get_by_label(label).accesskit_node().is_disabled()
}

/// Recategorize picks an account, says what the override does, and on
/// Apply writes it; the books re-derive with it and the detail reads
/// Override.
#[test]
fn recategorize_writes_an_override() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:1"));
    harness.run();
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Recategorize…").click();
    harness.run();
    type_last(&mut harness, "travel");
    harness.get_by_label("expenses:travel").click();
    harness.run();
    plan_says(&mut harness, "This one: expenses:unknown → expenses:travel");
    harness.get_by_label("Apply override").click();
    harness.run();
    assert!(harness.query_by_label("Saving…").is_some());

    let overrides = wait_vault(harness.state_mut(), sub, 1, kinds::OVERRIDE);
    assert!(overrides.contains(&VaultNote {
        d: "ov:card:1".to_owned(),
        account: Some("expenses:travel".to_owned()),
        ..VaultNote::default()
    }));
    step_until(&mut harness, "the override re-derives", |s| {
        s.app
            .triage()
            .is_some_and(|t| t.inputs.edits.overrides.len() == 2)
    });
    assert!(harness.query_by_label("Override").is_some());
    assert!(harness.query_by_label("Saving…").is_none());
}

/// The split editor writes one leg per row, the blank one taking the
/// rest; legs that add up to more than the charge say so and can't be
/// applied.
#[test]
fn the_split_editor_writes_its_legs() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:1"));
    harness.run();
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Split…").click();
    harness.run();
    // The picker opens on each leg without an account in turn.
    type_last(&mut harness, "groceries");
    harness.get_by_label("expenses:food:groceries").click();
    harness.run();
    type_last(&mut harness, "household");
    harness.get_by_label("expenses:household").click();
    harness.run();
    assert!(
        harness
            .query_by_label("only one leg can go without an amount")
            .is_some()
    );

    // 3.00 + 2.00 is more than the 4.50 coffee.
    type_into(&mut harness, 0, "3.00");
    type_into(&mut harness, 1, "2.00");
    assert!(
        harness
            .query_by_label("the legs add up to 5.00 CAD, but the counter leg is 4.50 CAD")
            .is_some()
    );
    assert!(disabled(&harness, "Apply split"));

    // Blank the second, so it takes the rest.
    for _ in 0.."2.00".len() {
        harness.key_press(egui::Key::Backspace);
    }
    harness.run();
    plan_says(
        &mut harness,
        "This one: expenses:unknown → expenses:food:groceries + expenses:household",
    );
    assert!(!disabled(&harness, "Apply split"));
    harness.get_by_label("Apply split").click();
    harness.run();

    let splits = wait_vault(harness.state_mut(), sub, 1, kinds::SPLIT);
    assert!(splits.contains(&VaultNote {
        d: "split:card:1".to_owned(),
        legs: 2,
        ..VaultNote::default()
    }));
}

/// "All like this" starts from the pattern every coffee shares, limited to
/// this one's source, says how many it moves, and writes the rule.
#[test]
fn all_like_this_writes_a_rule() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:1"));
    harness.run();
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("All like this…").click();
    harness.run();
    assert!(harness.query_by_label("source: card").is_some());
    type_last(&mut harness, "expenses:food:coffee");
    harness
        .get_by_label("+ New account expenses:food:coffee")
        .click();
    harness.run();
    plan_says(
        &mut harness,
        "Would move 3 transactions to expenses:food:coffee.",
    );
    harness.get_by_label("Add rule").click();
    harness.run();

    let rules = wait_vault(harness.state_mut(), sub, 1, kinds::RULE);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].account.as_deref(), Some("expenses:food:coffee"));
    assert_eq!(rules[0].pattern.as_deref(), Some("sq *coffee shop"));
    assert_eq!(rules[0].source.as_deref(), Some("card"));
}

/// An override on a split record replaces the split: the override is
/// written and the split cleared.
#[test]
fn an_override_clears_the_split() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:2"));
    harness.run();
    assert!(harness.query_by_label("Uncategorize").is_some());
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Recategorize…").click();
    harness.run();
    type_last(&mut harness, "groceries");
    harness
        .get_by_role_and_label(Role::Button, "expenses:food:groceries")
        .click();
    harness.run();
    plan_says(
        &mut harness,
        "This one: expenses:food:groceries + expenses:household → expenses:food:groceries",
    );
    harness.get_by_label("Apply override").click();
    harness.run();

    let overrides = wait_vault(harness.state_mut(), sub, 2, kinds::OVERRIDE);
    assert!(overrides.contains(&VaultNote {
        d: "ov:card:2".to_owned(),
        account: Some("expenses:food:groceries".to_owned()),
        ..VaultNote::default()
    }));
    let splits = read_vault(harness.state_mut(), kinds::SPLIT);
    assert_eq!(splits.len(), 1);
    assert_eq!(splits[0].d, "split:card:2");
    assert!(splits[0].cleared);
}

/// A linked record can't be categorized: its section offers the unlink,
/// which writes the link cleared.
#[test]
fn a_linked_record_offers_unlink() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:8"));
    harness.run();
    assert!(
        harness
            .query_by_label("Linked as a transfer with bank:4")
            .is_some()
    );
    assert!(harness.query_by_label("Recategorize…").is_none());
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Unlink").click();
    harness.run();
    plan_says(&mut harness, "applies");
    harness.get_by_label("Apply unlink").click();
    harness.run();

    let links = wait_vault(harness.state_mut(), sub, 1, kinds::TRANSFER);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].d, "xfer:bank:4|card:8");
    assert!(links[0].cleared);
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

/// Recategorize open on the coffee: the picker, and the override's plan
/// under it.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_categorize() {
    let mut app = triage_app();
    app.set_open_txn(Some("card:1"));
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.get_by_label("Recategorize…").click();
    harness.run();
    type_last(&mut harness, "shop");
    harness.get_by_label("expenses:shopping").click();
    harness.run();
    plan_says(&mut harness, "applies");
    harness.snapshot("cashflow_detail_categorize");
}

/// The Corner Grocer split open in the editor, its legs as written.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_split_editor() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    harness.state_mut().app.set_open_txn(Some("card:2"));
    harness.run();
    harness.get_by_label("Split…").click();
    harness.run();
    plan_says(&mut harness, "applies");
    harness.snapshot("cashflow_detail_split_editor");
}

/// "All like this" on the coffee, its account picked and its plan landed.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_rule_preview() {
    let mut app = triage_app();
    app.set_open_txn(Some("card:1"));
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.get_by_label("All like this…").click();
    harness.run();
    type_last(&mut harness, "expenses:food:coffee");
    harness
        .get_by_label("+ New account expenses:food:coffee")
        .click();
    harness.run();
    plan_says(&mut harness, "applies");
    harness.snapshot("cashflow_detail_rule_preview");
}

/// The card payment, linked as a transfer: Unlink in place of categorizing.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_linked() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    harness.state_mut().app.set_open_txn(Some("card:8"));
    harness.run();
    harness.snapshot("cashflow_detail_linked");
}
