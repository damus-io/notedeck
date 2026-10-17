//! The Cashflow shell: the view nav and the router, on desktop and phone.
//!
//! The behavioural tests run under a plain `cargo test`. The snapshots are
//! `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe), like every
//! Notedeck snapshot suite.

mod common;

use common::{DESKTOP, MOBILE, harness, snapshot_harness};
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::{CashflowApp, View};

/// The app opens on Overview, and picking a view in the rail routes to it.
#[test]
fn rail_routes_to_the_picked_view() {
    let mut harness = harness(CashflowApp::new(), DESKTOP);
    assert_eq!(harness.state().app.view(), View::Overview);

    // Only the rail names Trends until it is the routed view, whose heading
    // names it too.
    assert_eq!(harness.query_all_by_label("Trends").count(), 1);
    harness.get_by_label("Trends").click_accesskit();
    harness.run();

    assert_eq!(harness.state().app.view(), View::Trends);
    assert_eq!(harness.query_all_by_label("Trends").count(), 2);
}

/// On a phone the nav is a tab strip, and every view is still reachable.
#[test]
fn strip_routes_to_the_picked_view_on_mobile() {
    let mut harness = harness(CashflowApp::new(), MOBILE);

    harness.get_by_label("Settings").click_accesskit();
    harness.run();

    assert_eq!(harness.state().app.view(), View::Settings);
}

/// The shell on desktop: the nav rail beside the routed view.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_shell_desktop() {
    let mut harness = snapshot_harness(CashflowApp::new(), DESKTOP);
    harness.snapshot("cashflow_shell_desktop");
}

/// The shell on a phone: the nav collapses to a tab strip above the view.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_shell_mobile() {
    let mut harness = snapshot_harness(CashflowApp::new(), MOBILE);
    harness.snapshot("cashflow_shell_mobile");
}
