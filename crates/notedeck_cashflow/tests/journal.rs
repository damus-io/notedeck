//! The app loading its own books: the journal under `$CASHFLOW_DIR` and the
//! vault, off the UI thread, and the panes that stand in for the views
//! until there are books.
//!
//! The behavioural tests run under a plain `cargo test`. The snapshots are
//! `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe).

mod common;

use common::{
    ABSENT_DIR, DESKTOP, JournalFixture, journal_app, journal_harness, loaded_harness,
    publish_missing_segment, snapshot_harness, step_until, synthetic_app,
};
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::View;
use notedeck_cashflow::journal::JournalState;

/// The first load runs on the job pool: the first frame shows the loading
/// pane, and the synthetic journal's books arrive a few frames later.
#[test]
fn the_journal_loads_off_the_ui_thread() {
    let fixture = JournalFixture::synthetic();
    let mut harness = journal_harness(fixture.app(), DESKTOP, false);
    assert!(harness.state().app.books().is_none());
    harness.get_by_label("Loading the journal…");

    step_until(&mut harness, "the books load", |s| s.app.books().is_some());
    let books = harness.state().app.books().unwrap();
    assert_eq!(books.ledger().txns().len(), 35);
    assert!(harness.query_by_label("Loading the journal…").is_none());
}

/// A segment that lands in the journal while the app is open, by any
/// route, is merged in by the next rescan.
#[test]
fn a_new_segment_is_picked_up_by_a_rescan() {
    let fixture = JournalFixture::synthetic();
    let mut harness = loaded_harness(fixture.app(), DESKTOP, false);
    assert_eq!(
        harness.state().app.books().unwrap().ledger().txns().len(),
        35
    );

    fixture.write(
        "2024-10-17 * Late Lunch\n  ; id: late:1\n  expenses:food  14.00 CAD\n  assets:checking\n",
    );
    step_until(&mut harness, "the rescan picks up the new segment", |s| {
        s.app.books().unwrap().ledger().txns().len() == 36
    });
    assert_eq!(
        harness.state().app.books().unwrap().report.loaded.len(),
        1,
        "only the new segment is read"
    );
}

/// The summary names what is loaded and how long it took.
#[test]
fn settings_summarize_the_loaded_journal() {
    let fixture = JournalFixture::synthetic();
    let mut app = fixture.app();
    app.set_view(View::Settings);
    let harness = loaded_harness(app, DESKTOP, false);
    let summary = harness
        .query_all_by_label_contains("35 transactions · 2024-09-01 to 2024-10-16 · loaded in")
        .count();
    assert_eq!(summary, 1);
}

/// A data dir with no journal shows the import commands in place of every
/// view, with no sync offered when no transport is configured.
#[test]
fn an_empty_journal_shows_how_to_import() {
    let harness = loaded_harness(journal_app(ABSENT_DIR.as_ref()), DESKTOP, false);
    assert_eq!(
        harness.state().app.journal().unwrap().state(true),
        JournalState::Empty
    );
    harness.get_by_label("No transactions yet");
    harness.get_by_label("cashflow import amex ~/Downloads/activity.csv");
    assert!(harness.query_by_label("Sync now").is_none());
}

/// A segment the vault lists that the journal lacks shows a banner naming
/// the host that has it, with a sync to fetch it.
#[test]
fn a_missing_segment_shows_the_sync_banner() {
    let fixture = JournalFixture::synthetic().with_transport();
    let mut harness = journal_harness(fixture.app(), DESKTOP, false);
    publish_missing_segment(harness.state_mut(), "bigbox");
    step_until(&mut harness, "the missing segment is noticed", |s| {
        s.app.books().is_some()
            && s.app
                .journal()
                .is_some_and(|j| j.missing().hashes.len() == 1)
    });
    harness.get_by_label("1 segment on bigbox");
    harness.get_by_label("Sync now");
}

/// The pane before the first load comes back.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_journal_loading() {
    let mut harness = journal_harness(journal_app(ABSENT_DIR.as_ref()), DESKTOP, true);
    harness.snapshot("cashflow_journal_loading");
}

/// The pane for a data dir with no journal.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_journal_empty() {
    let mut harness = loaded_harness(journal_app(ABSENT_DIR.as_ref()), DESKTOP, true);
    harness.snapshot("cashflow_journal_empty");
}

/// The banner over a view while the vault lists a segment the journal
/// lacks.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_journal_missing_segments() {
    let fixture = JournalFixture::synthetic().with_transport();
    let mut app = fixture.app();
    app.set_view(View::Goals);
    let mut harness = journal_harness(app, DESKTOP, true);
    publish_missing_segment(harness.state_mut(), "bigbox");
    step_until(&mut harness, "the missing segment is noticed", |s| {
        s.app.books().is_some()
            && s.app
                .journal()
                .is_some_and(|j| j.missing().hashes.len() == 1)
    });
    harness.snapshot("cashflow_journal_missing_segments");
}

/// The summary of the loaded books in Settings. Handed its books, so there
/// is no load time to vary between runs.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_journal_summary() {
    let mut app = synthetic_app();
    app.set_view(View::Settings);
    let mut harness = snapshot_harness(app, DESKTOP);
    harness.snapshot("cashflow_journal_summary");
}
