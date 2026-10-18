//! The Transactions view: the register and its filters, and the triage
//! inbox's writes (a rule from a group, an override for one row, a
//! transfer linked or dismissed), over the made-up triage journals in
//! `common`.
//!
//! The behavioural tests run under a plain `cargo test`; the writes land in
//! the harness host's nostrdb, as the account it is logged in as. The
//! snapshots are `#[ignore]`d and run via `scripts/snapshot-test`
//! (lavapipe).

mod common;

use cashflow::kinds;
use common::{
    CashflowTestState, DESKTOP, JournalFixture, MOBILE, TRIAGE_CARD, VaultNote, harness,
    loaded_harness, snapshot_harness, step_until, triage_app, wait_vault, watch_vault,
};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

/// A window tall enough for every group of the triage journal.
const TALL: egui::Vec2 = egui::Vec2::new(1200.0, 1100.0);

/// Open the inbox tab.
fn open_inbox(harness: &mut Harness<'static, CashflowTestState>) {
    harness.get_by_label("Inbox").click();
    harness.run();
}

/// The register lists this month's transactions, and its filters narrow
/// them.
#[test]
fn the_register_filters() {
    let mut harness = harness(triage_app(), DESKTOP);
    assert!(harness.query_by_label("12 transactions").is_some());

    harness.get_by_label("Unknown only").click();
    harness.run();
    assert!(harness.query_by_label("10 transactions").is_some());
    harness.get_by_label("Unknown only").click();
    harness.run();

    // The account filter's picker searches the chart.
    harness.get_by_label("Any account").click();
    harness.run();
    // The payee search is the first text box, the picker's the second.
    harness
        .get_all_by_role(Role::TextInput)
        .last()
        .unwrap()
        .type_text("shopping");
    harness.run();
    harness.get_by_label("expenses:shopping").click();
    harness.run();
    assert!(harness.query_by_label("1 transaction").is_some());

    // A period with nothing in it lists nothing.
    harness.get_by_label("Last month").click();
    harness.run();
    assert!(harness.query_by_label("0 transactions").is_some());
}

/// The inbox counts what is left, suggests the card payment as a
/// transfer, and lists every payee group.
#[test]
fn the_inbox_lists_groups_and_suggestions() {
    let mut harness = harness(triage_app(), TALL);
    open_inbox(&mut harness);
    assert!(harness.query_by_label("10 unknown left").is_some());
    assert!(harness.query_by_label("Link as transfer").is_some());
    assert!(harness.query_by_label("sq coffee shop").is_some());
    assert!(harness.query_by_label("corner grocer").is_some());
    assert_eq!(harness.query_all_by_label("Categorize all…").count(), 7);
}

/// "Categorize all…" on a group writes a rule matching its payee, to an
/// account typed in as a new path, and hides the group at once.
#[test]
fn categorize_all_writes_a_rule() {
    let mut harness = harness(triage_app(), TALL);
    open_inbox(&mut harness);
    let sub = watch_vault(harness.state_mut());
    // The groups rank by total: the coffees are the smallest, so last.
    harness
        .get_all_by_label("Categorize all…")
        .last()
        .unwrap()
        .click();
    harness.run();
    harness
        .get_by_role(Role::TextInput)
        .type_text("expenses:food:coffee");
    harness.run();
    harness
        .get_by_label("+ New account expenses:food:coffee")
        .click();
    harness.run();

    let rules = wait_vault(harness.state_mut(), sub, 1, kinds::RULE);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].account.as_deref(), Some("expenses:food:coffee"));
    assert!(harness.query_by_label("sq coffee shop").is_none());
    assert!(
        harness
            .query_by_label("7 unknown left (−3 today)")
            .is_some()
    );
}

/// "This one…" on a row writes an override for just that transaction.
#[test]
fn this_one_writes_an_override() {
    let mut harness = harness(triage_app(), TALL);
    open_inbox(&mut harness);
    let sub = watch_vault(harness.state_mut());
    harness.get_by_label("sq coffee shop").click();
    harness.run();
    harness
        .get_all_by_label("This one…")
        .next()
        .unwrap()
        .click();
    harness.run();
    harness.get_by_role(Role::TextInput).type_text("shopping");
    harness.run();
    harness.get_by_label("expenses:shopping").click();
    harness.run();

    let overrides = wait_vault(harness.state_mut(), sub, 1, kinds::OVERRIDE);
    assert_eq!(
        overrides,
        [VaultNote {
            d: "ov:card:1".to_owned(),
            cleared: false,
            account: Some("expenses:shopping".to_owned()),
        }]
    );
    assert!(
        harness
            .query_by_label("9 unknown left (−1 today)")
            .is_some()
    );
}

/// Accepting a suggestion writes the link; dismissing one writes it
/// cleared, so it is never suggested again.
#[test]
fn transfers_link_or_dismiss() {
    for (button, cleared) in [("Link as transfer", false), ("Not a transfer", true)] {
        let mut harness = harness(triage_app(), TALL);
        open_inbox(&mut harness);
        let sub = watch_vault(harness.state_mut());
        harness.get_by_label(button).click();
        harness.run();
        let links = wait_vault(harness.state_mut(), sub, 1, kinds::TRANSFER);
        assert_eq!(links.len(), 1, "{button}");
        assert_eq!(links[0].d, "xfer:bank:4|card:8");
        assert_eq!(links[0].cleared, cleared, "{button}");
        assert!(
            harness.query_by_label(button).is_none(),
            "{button} hides it"
        );
    }
}

/// The whole loop over a real journal: a rule written from the inbox lands
/// in the vault, the journal source re-derives with it off the UI thread,
/// and the new triage counts the group as done today.
#[test]
fn a_rule_from_the_inbox_rederives() {
    let fixture = JournalFixture::empty();
    fixture.write(TRIAGE_CARD);
    let mut app = fixture.app();
    app.set_view(notedeck_cashflow::View::Transactions);
    let mut harness = loaded_harness(app, TALL, false);
    let progress = |s: &CashflowTestState| s.app.triage().map(|t| t.progress);
    assert_eq!(progress(harness.state()).unwrap().left, 7);
    open_inbox(&mut harness);
    let _sub = watch_vault(harness.state_mut());
    harness
        .get_all_by_label("Categorize all…")
        .last()
        .unwrap()
        .click();
    harness.run();
    harness
        .get_by_role(Role::TextInput)
        .type_text("expenses:food:coffee");
    harness.run();
    harness
        .get_by_label("+ New account expenses:food:coffee")
        .click();
    step_until(&mut harness, "the rule re-derives", |s| {
        progress(s).is_some_and(|p| p.left == 4)
    });
    assert_eq!(progress(harness.state()).unwrap().today, 3);
    assert!(
        harness
            .query_by_label("4 unknown left (−3 today)")
            .is_some()
    );
}

/// The register over the triage journals: icons and display names from
/// the chart, unknown accounts in the warning color, money in in green.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_transactions_register() {
    let mut harness = snapshot_harness(triage_app(), DESKTOP);
    harness.snapshot("cashflow_transactions_register");
}

/// On a phone the register drops the home-currency column.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_transactions_register_mobile() {
    let mut harness = snapshot_harness(triage_app(), MOBILE);
    harness.snapshot("cashflow_transactions_register_mobile");
}

/// The inbox: progress, the card payment suggested as a transfer, and the
/// payee groups, one opened to its rows.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_transactions_inbox() {
    let mut harness = snapshot_harness(triage_app(), TALL);
    open_inbox(&mut harness);
    harness.get_by_label("corner grocer").click();
    harness.run();
    harness.snapshot("cashflow_transactions_inbox");
}

/// The account picker open on a group, a new path typed in.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_transactions_account_picker() {
    let mut harness = snapshot_harness(triage_app(), TALL);
    open_inbox(&mut harness);
    harness
        .get_all_by_label("Categorize all…")
        .last()
        .unwrap()
        .click();
    harness.run();
    harness
        .get_by_role(Role::TextInput)
        .type_text("expenses:food");
    harness.run();
    harness.snapshot("cashflow_transactions_account_picker");
}
