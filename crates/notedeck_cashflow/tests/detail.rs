//! A transaction's detail: opened from a register row or an inbox row as an
//! entry on the chrome's nav stack, drawn cold from that entry, and left by
//! its own back, Escape, or a view picked from the rail.
//!
//! The nav requests are read off the harness host's `Navigator`, which the
//! chrome would drain after the frame. The override, the split and the
//! link are written through the vault before the app's first load
//! (`common::detail_harness`), so the detail traces them to their notes.
//!
//! Its Notes section posts a sealed kind-1111 note, which comes back
//! through the app's subscription into the thread and marks the
//! register's row.
//!
//! Its Similar section lists the coffees and the other side's amount, a
//! row opens its own entry, and "Use …" pre-fills Categorize without
//! writing.
//!
//! Its Categorize and Transfer sections write through the vault too, each
//! write planned first: the tests wait for the plan, Apply, and read the
//! notes back. Its Project section's tag writes the account's metadata,
//! and its move is the Categorize section's override.
//! The snapshots are `#[ignore]`d and run via `scripts/snapshot-test`
//! (lavapipe).

mod common;

use std::any::Any;
use std::rc::Rc;

use cashflow::kinds;
use cashflow::projects::{ProjectRoot, Projects};
use common::{
    CashflowTestState, DESKTOP, DETAIL_AGENTIUM, MOBILE, VaultNote, detail_fixture, detail_harness,
    harness, loaded_harness, post_notes, publish_account_meta, publish_account_project, read_vault,
    snapshot_harness, step_until, triage_app, wait_vault, watch_vault,
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
    // Among the postings, and over its project in the Project section.
    assert_eq!(harness.query_all_by_label("expenses:household").count(), 2);
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

/// A linked record can't be categorized: its Transfer section offers the
/// unlink, which writes the link cleared.
#[test]
fn a_linked_record_offers_unlink() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:8"));
    harness.run();
    assert!(harness.query_by_label("Linked with bank:4 →").is_some());
    assert!(
        harness
            .query_by_label("A transfer can't be categorized: unlink it first.")
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

/// The card payment leaving the bank is suggested as a transfer with its
/// arrival on the card: the section shows that leg, and Link, once its plan
/// lands, writes the link.
#[test]
fn a_suggested_transfer_links() {
    let mut harness = open_on("bank:4");
    assert!(harness.query_by_label("suggested").is_some());
    assert!(
        harness
            .query_by_label("Payment received · 2024-10-14")
            .is_some()
    );
    assert!(harness.query_by_label("2 days later").is_some());
    plan_says(
        &mut harness,
        "Linking nets both legs: no income, no expense.",
    );
    assert!(!disabled(&harness, "Link as transfer"));
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Link as transfer").click();
    harness.run();
    assert!(harness.query_by_label("Saving…").is_some());

    let links = wait_vault(harness.state_mut(), sub, 1, kinds::TRANSFER);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].d, "xfer:bank:4|card:8");
    assert!(!links[0].cleared);
}

/// "Not a transfer" writes the link cleared; once the books re-derive with
/// it, neither the detail nor the inbox suggests the pair.
#[test]
fn not_a_transfer_dismisses_it() {
    let fixture = detail_fixture();
    let mut app = fixture.app();
    app.set_view(View::Transactions);
    let mut harness = loaded_harness(app, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:8"));
    harness.run();
    assert!(harness.query_by_label("open bank:4 →").is_some());
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Not a transfer").click();
    harness.run();

    let links = wait_vault(harness.state_mut(), sub, 1, kinds::TRANSFER);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].d, "xfer:bank:4|card:8");
    assert!(links[0].cleared);
    step_until(&mut harness, "the dismissal re-derives", |s| {
        s.app.triage().is_some_and(|t| t.suggestions.is_empty())
    });
    harness.run();
    assert!(
        harness
            .query_by_label("No matching leg in your other accounts within 5 days")
            .is_some()
    );
    harness.state_mut().app.set_open_txn(None);
    harness.run();
    harness.get_by_label("Inbox").click();
    harness.run();
    assert!(harness.query_by_label("Link as transfer").is_none());
}

/// "open bank:4 →" pushes the other leg's detail on the nav stack.
#[test]
fn opening_the_other_leg_pushes_its_detail() {
    let mut harness = open_on("card:8");
    harness.get_by_label("open bank:4 →").click();
    harness.run();
    let requests = harness.state_mut().take_nav();
    assert_eq!(
        pushed(&requests),
        &txn_route("bank:4", "Card payment · 2024-10-12")
    );
    assert_eq!(harness.state().app.open_txn(), Some("bank:4"));
}

/// The coffee's detail lists the other two coffees, newest first, and
/// tallies them: both unknown, so there is nothing to use.
#[test]
fn similar_lists_the_same_payee() {
    let harness = open_on("card:1");
    assert!(harness.query_by_label("same payee 2").is_some());
    assert!(harness.query_by_label("Same payee (2)").is_some());
    assert!(harness.query_by_label("2 unknown").is_some());
    let rows: Vec<String> = harness
        .get_all_by_role(Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| l.contains("COFFEE SHOP"))
        .collect();
    assert_eq!(
        rows,
        [
            "2024-10-09 SQ *COFFEE SHOP #31",
            "2024-10-05 SQ *COFFEE SHOP #9"
        ]
    );
    assert!(harness.query_by_label_contains("Use ").is_none());
}

/// The card payment arriving lists the bank's payment leaving: the same
/// 600 CAD from another payee.
#[test]
fn similar_lists_the_same_amount() {
    let harness = open_on("card:8");
    assert!(
        harness
            .query_by_label("Same amount, other payees (1)")
            .is_some()
    );
    assert!(harness.query_by_label("2024-10-12 Card payment").is_some());
    assert!(harness.query_by_label("Same payee (0)").is_none());
}

/// A similar row pushes its own entry over the first, and the chrome's
/// back onto the first entry draws the first record again.
#[test]
fn a_similar_row_pushes_another_detail() {
    let mut harness = harness(triage_app(), DESKTOP);
    harness.state_mut().take_nav();
    harness.get_by_role_and_label(Role::Button, COFFEE).click();
    harness.run();
    harness
        .get_by_label("2024-10-05 SQ *COFFEE SHOP #9")
        .click();
    harness.run();

    let requests = harness.state_mut().take_nav();
    let routes: Vec<&CashflowRoute> = requests
        .iter()
        .filter_map(|r| match r {
            NavRequest::Push(token) => token.downcast_ref::<CashflowRoute>(),
            _ => None,
        })
        .collect();
    let first = txn_route("card:1", "SQ *COFFEE SHOP #12 · 2024-10-02");
    let second = txn_route("card:3", "SQ *COFFEE SHOP #9 · 2024-10-05");
    assert_eq!(routes, [&first, &second]);
    assert_eq!(harness.state().app.open_txn(), Some("card:3"));

    // Back: the chrome hands the first entry's token to `render_nav`.
    harness.state_mut().route = Some(Rc::new(first));
    harness.run();
    assert_eq!(harness.state().app.open_txn(), Some("card:1"));
    assert!(harness.query_by_label("SQ *COFFEE SHOP #12").is_some());
}

/// "Show all" lists every coffee in the register, by what their payees
/// share.
#[test]
fn show_all_filters_the_register_by_the_payee() {
    let mut harness = open_on("card:1");
    harness.get_by_label("Show all 3 in the register →").click();
    harness.run();

    assert_eq!(harness.state().app.view(), View::Transactions);
    assert_eq!(harness.state().app.open_txn(), None);
    assert!(harness.query_by_label("3 transactions").is_some());
}

/// Step `harness` until a label containing `part` is on screen.
fn plan_says_part(harness: &mut Harness<'static, CashflowTestState>, part: &str) {
    let deadline = std::time::Instant::now() + common::INGEST_TIMEOUT;
    while harness.query_by_label_contains(part).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {part:?}"
        );
        harness.step();
    }
}

/// "Use …" on the second grocer run plans an override to where the first
/// run's split sent the most, and writes nothing; with "All like this"
/// open it is the rule's account instead.
#[test]
fn use_pre_fills_categorize_without_writing() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:7"));
    harness.run();
    assert!(
        harness
            .query_by_label("1 → expenses:food:groceries (split), 1 → expenses:household (split)")
            .is_some()
    );
    harness
        .get_by_label("Use expenses:food:groceries →")
        .click();
    harness.run();
    plan_says(
        &mut harness,
        "This one: expenses:unknown → expenses:food:groceries",
    );
    assert!(!disabled(&harness, "Apply override"));

    harness.get_by_label("All like this…").click();
    harness.run();
    harness
        .get_by_label("Use expenses:food:groceries →")
        .click();
    harness.run();
    assert!(harness.query_by_label("expenses:food:groceries").is_some());
    plan_says_part(
        &mut harness,
        "Would move 1 transaction to expenses:food:groceries",
    );
    assert!(!disabled(&harness, "Add rule"));

    // Nothing was written: the fixture's one override, and no rule.
    harness.run();
    assert_eq!(read_vault(harness.state_mut(), kinds::OVERRIDE).len(), 1);
    assert!(read_vault(harness.state_mut(), kinds::RULE).is_empty());
}

/// A categorized record can't be linked: Link is disabled, and
/// "Uncategorize first" opens the uncategorize.
#[test]
fn a_categorized_record_cannot_link() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    harness.state_mut().app.set_open_txn(Some("card:5"));
    harness.run();
    assert!(disabled(&harness, "Link as transfer"));
    harness.get_by_label("Uncategorize first").click();
    harness.run();
    plan_says(&mut harness, "applies");
    assert!(!disabled(&harness, "Apply uncategorize"));
}

/// The triage app with `record`'s detail open and two accounts put in
/// projects: housing (rent under it) in `home`, shopping in `side-gig`.
fn projects_app(record: &str) -> notedeck_cashflow::CashflowApp {
    let root = |account: &str, project: &str| ProjectRoot {
        account: account.to_owned(),
        project: project.to_owned(),
    };
    let mut app = triage_app();
    app.set_projects(Projects::new([
        root("expenses:housing", "home"),
        root("expenses:shopping", "side-gig"),
    ]));
    app.set_open_txn(Some(record));
    app
}

/// A zap into a wallet, for the tag test: an income account nothing has
/// put in a project. Made up.
const WALLET: &str = "\
2024-10-08 * Zap from a fan
  ; id: wallet:1
  assets:wallet  21.00 CAD
  income:zaps
";

/// The rent's project comes from the housing account above it, and says
/// so.
#[test]
fn an_inherited_project_shows_its_root() {
    let mut harness = harness(projects_app("bank:2"), DESKTOP);
    harness.run();
    assert!(harness.query_by_label("Project").is_some());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "home")
            .is_some()
    );
    assert!(harness.query_by_label("from expenses:housing").is_some());
    assert!(
        harness
            .query_by_label("Tag expenses:housing:rent with a project…")
            .is_some()
    );
}

/// The project's chip opens the Projects view drilled into it, and leaves
/// the detail.
#[test]
fn the_project_chip_opens_projects() {
    let mut harness = harness(projects_app("bank:2"), DESKTOP);
    harness.state_mut().take_nav();
    harness.get_by_role_and_label(Role::Button, "home").click();
    harness.run();

    assert_eq!(harness.state().app.view(), View::Projects);
    assert_eq!(harness.state().app.open_txn(), None);
    assert!(harness.query_by_label("← All projects").is_some());
    let requests = harness.state_mut().take_nav();
    assert!(matches!(requests[..], [NavRequest::Remove(_)]));
}

/// The coffee nothing has categorized isn't worth tagging, every unknown
/// would go with it: no tag, but it can be moved.
#[test]
fn an_unknown_leg_offers_the_move_not_the_tag() {
    let harness = open_on("card:1");
    assert!(harness.query_by_label("Unattributed").is_some());
    assert!(
        harness
            .query_by_label("Tag expenses:unknown with a project…")
            .is_none()
    );
    assert!(
        harness
            .query_by_label("Move to a project account…")
            .is_some()
    );
}

/// Tagging the zap's income account says how far it reaches, writes the
/// account's metadata with the project and its icon kept, and once the
/// chart re-derives the detail and Projects show it.
#[test]
fn tagging_the_account_writes_its_project() {
    let fixture = detail_fixture();
    fixture.write_as("wallet", WALLET);
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    let icon = cashflow::chart::AccountMeta {
        icon: Some("⚡"),
        ..cashflow::chart::AccountMeta::default()
    };
    publish_account_meta(harness.state_mut(), "income:zaps", &icon);
    harness.state_mut().app.set_open_txn(Some("wallet:1"));
    harness.run();
    assert!(harness.query_by_label("Unattributed").is_some());

    let sub = watch_vault(harness.state_mut());
    harness
        .get_by_label("Tag income:zaps with a project…")
        .click();
    harness.run();
    assert!(
        harness
            .query_by_label("Tags income:zaps and everything under it: 1 transaction.")
            .is_some()
    );
    type_last(&mut harness, "damus");
    harness.get_by_label("+ New project damus").click();
    harness.run();
    assert!(harness.query_by_label("income:zaps → damus").is_some());
    harness.get_by_label("Apply tag").click();
    harness.run();
    assert!(harness.query_by_label("Saving…").is_some());

    let accounts = wait_vault(harness.state_mut(), sub, 1, kinds::ACCOUNT);
    let zaps = accounts
        .iter()
        .find(|a| a.d == "acct:income:zaps")
        .expect("the account's metadata");
    assert_eq!(zaps.project.as_deref(), Some("damus"));
    assert_eq!(zaps.icon.as_deref(), Some("⚡"));

    step_until(&mut harness, "the project re-derives", |s| {
        s.app.projects().project_of("income:zaps") == Some("damus")
    });
    assert!(harness.query_by_label("set on this account").is_some());
    assert!(harness.query_by_label("Saving…").is_none());
    harness.get_by_role_and_label(Role::Button, "damus").click();
    harness.run();
    assert_eq!(harness.state().app.view(), View::Projects);
    assert!(harness.query_by_label("← All projects").is_some());
}

/// "Move to a project account" lists the accounts in a project first,
/// under their project, and writes the override the Categorize section
/// plans.
#[test]
fn move_to_a_project_account_writes_an_override() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, false);
    publish_account_project(harness.state_mut(), "expenses:shopping", "side-gig");
    step_until(&mut harness, "the project loads", |s| {
        s.app.projects().project_of("expenses:shopping").is_some()
    });
    harness.state_mut().app.set_open_txn(Some("card:1"));
    harness.run();
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("Move to a project account…").click();
    harness.run();
    // Listed first, with its project beside it, before anything is typed.
    assert!(harness.query_by_label("side-gig").is_some());
    harness
        .get_by_role_and_label(Role::Button, "expenses:shopping")
        .click();
    harness.run();
    plan_says(
        &mut harness,
        "This one: expenses:unknown → expenses:shopping",
    );
    harness.get_by_label("Apply override").click();
    harness.run();

    let overrides = wait_vault(harness.state_mut(), sub, 1, kinds::OVERRIDE);
    assert!(overrides.contains(&VaultNote {
        d: "ov:card:1".to_owned(),
        account: Some("expenses:shopping".to_owned()),
        ..VaultNote::default()
    }));
}

/// Wait until `label` is on screen, stepping frames: what a note that
/// lands through the ingest thread needs.
fn until_shown(harness: &mut Harness<'static, CashflowTestState>, label: &str) {
    let deadline = std::time::Instant::now() + common::INGEST_TIMEOUT;
    while harness.query_by_label(label).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {label:?}"
        );
        harness.step();
    }
}

/// A note typed into the detail posts on "Post note", comes back into
/// the thread, and marks the record's register row; the other rows stay
/// unmarked.
#[test]
fn a_note_posts_and_marks_the_register_row() {
    let mut harness = open_on("card:1");
    // The vault root, so the note the app seals unwraps here.
    watch_vault(harness.state_mut());
    assert!(harness.query_by_label("No notes yet.").is_some());
    assert!(disabled(&harness, "Post note"), "nothing to post yet");

    harness.get_by_role(Role::MultilineTextInput).focus();
    harness.run();
    harness
        .get_by_role(Role::MultilineTextInput)
        .type_text("receipt in the drawer");
    harness.run();
    harness.get_by_label("Post note").click();
    harness.run();
    until_shown(&mut harness, "receipt in the drawer");
    assert!(harness.query_by_label("Notes (1)").is_some());
    assert!(harness.query_by_label("you · just now").is_some());
    assert!(disabled(&harness, "Post note"), "the box was cleared");
    until_shown(&mut harness, "Post note");
    assert!(harness.query_by_label("Saving…").is_none());

    harness.get_by_label("← Transactions").click();
    harness.run();
    assert_eq!(harness.get_all_by_label("Has notes").count(), 1);
    // A note never categorizes: the coffee is still unknown.
    assert!(
        harness
            .state()
            .app
            .triage()
            .is_some_and(|t| t.inputs.edits.overrides.is_empty())
    );
}

/// Notes written elsewhere (the CLI, another device) read in the order
/// they were written, and a reopened detail reads its own thread.
#[test]
fn a_thread_reads_oldest_first() {
    let mut harness = open_on("card:1");
    post_notes(harness.state_mut(), "card:1", &["first", "second"]);
    until_shown(&mut harness, "Notes (2)");
    let top = |label: &str| {
        let node = harness.get_by_label(label);
        node.accesskit_node().bounding_box().expect("drawn").y0
    };
    assert!(top("first") < top("second"), "oldest first");

    harness.state_mut().app.set_open_txn(Some("card:4"));
    harness.run();
    assert!(harness.query_by_label("No notes yet.").is_some());
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
    // The plan landing can repaint past `run`'s few steps; `plan_says`
    // steps until it shows.
    let _ = harness.run_ok();
    plan_says(&mut harness, "applies");
    harness.snapshot("cashflow_detail_rule_preview");
}

/// The card payment arriving, suggested as a transfer with the bank's
/// side: the Transfer section first, its link planned.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_transfer_suggested() {
    let mut app = triage_app();
    app.set_open_txn(Some("card:8"));
    let mut harness = snapshot_harness(app, DESKTOP);
    plan_says(&mut harness, "applies");
    harness.snapshot("cashflow_detail_transfer_suggested");
}

/// The card payment linked: the Transfer section first, its unlink
/// planned, and Categorize saying to unlink first.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_transfer_linked() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    harness.state_mut().app.set_open_txn(Some("card:8"));
    harness.run();
    harness.get_by_label("Unlink").click();
    harness.run();
    plan_says(&mut harness, "applies");
    harness.snapshot("cashflow_detail_transfer_linked");
}

/// The Corner Grocer split, its household leg in a project and its
/// groceries leg unattributed.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_project() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    publish_account_project(harness.state_mut(), "expenses:household", "home");
    step_until(&mut harness, "the project loads", |s| {
        s.app.projects().project_of("expenses:household").is_some()
    });
    harness.state_mut().app.set_open_txn(Some("card:2"));
    harness.run();
    harness.snapshot("cashflow_detail_project");
}

/// "Move to a project account" open on the coffee: the accounts in a
/// project first, under their project.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_project_picker() {
    let mut harness = snapshot_harness(projects_app("card:1"), DESKTOP);
    harness.get_by_label("Move to a project account…").click();
    harness.run();
    harness.snapshot("cashflow_detail_project_picker");
}

/// The second Corner Grocer run: the first run in its Similar section,
/// split two ways, the tally saying so and offering its groceries.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_similar() {
    let fixture = detail_fixture();
    let mut harness = detail_harness(&fixture, DESKTOP, true);
    harness.state_mut().app.set_open_txn(Some("card:7"));
    harness.run();
    harness.snapshot("cashflow_detail_similar");
}

/// The coffee with two notes, the thread at the bottom of the facts
/// column.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_detail_notes() {
    let mut app = triage_app();
    app.set_open_txn(Some("card:1"));
    let mut harness = snapshot_harness(app, TALL);
    post_notes(
        harness.state_mut(),
        "card:1",
        &[
            "Receipt is in the shoebox.",
            "Split with Sam: he owes half.",
        ],
    );
    until_shown(&mut harness, "Notes (2)");
    harness.run();
    harness.snapshot("cashflow_detail_notes");
}
