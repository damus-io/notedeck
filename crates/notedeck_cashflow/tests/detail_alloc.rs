//! A transaction's detail works out nothing per frame.
//!
//! The detail's text is worked out when the books or the record change,
//! and laid out when the width does (`views::detail::Galleys`), so a frame
//! that holds still pays only what egui charges for the widgets the galleys
//! sit in. This opens one record's detail over the triage journal, through
//! the real app over a real `Notedeck`, and holds a whole steady-state
//! frame, the rail with it, to a measured figure. (The register's ratchet
//! measures over an empty view instead, but the empty Goals view costs
//! more than the detail does, so a difference would sit at zero and hide
//! a regression under it.) Formatting or laying out a line per frame costs
//! at least one allocation per line, which the tolerance has room for only
//! a few of.

mod common;

use common::{CashflowTestState, DESKTOP, harness, triage_app};
use egui_kittest::Harness;
use notedeck_cashflow::View;
use notedeck_testing::alloc::{AllocBudget, CountingAllocator, attribute, measure};

/// Process-wide and one per binary, so it lives in this test rather than in
/// `notedeck_testing`.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Frames run before measuring, so fonts and galleys are all warm.
const WARM_UP: usize = 30;

/// Frames measured; the median is what is held to the budget.
const FRAMES: usize = 15;

/// What a steady-state frame of the uncategorized coffee's detail costs,
/// rail and all, measured on the UI thread. Attributed with
/// [`where_a_detail_frame_allocates`] when it was written down, it is
/// egui's: accesskit nodes and their labels for each line (which the app
/// builds only under a screen reader), the two columns' child `Ui`s, the
/// buttons' atoms and the scroll area. Every line is painted from a galley
/// laid out once. Measured, not chosen; if it moves, measure it again and
/// write the new figure down.
const MEASURED_DETAIL_ALLOCS: u64 = 237;

/// How far above the measured figure a frame may land and still be
/// unchanged: a different machine has measured a frame one or two
/// allocations apart.
const TOLERANCE: u64 = 4;

/// A harness of the triage journal with `card:1`'s detail open over the
/// Transactions view, warmed up.
fn warm() -> Harness<'static, CashflowTestState> {
    let mut app = triage_app();
    app.set_view(View::Transactions);
    app.set_open_txn(Some("card:1"));
    let mut harness = harness(app, DESKTOP);
    harness.event(egui::Event::PointerMoved(egui::pos2(600.0, 500.0)));
    for _ in 0..WARM_UP {
        harness.step();
    }
    harness
}

/// The median allocations on the UI thread of a steady-state frame.
fn frame_allocs() -> u64 {
    let mut harness = warm();
    let mut allocs: Vec<u64> = (0..FRAMES)
        .map(|_| measure(|| harness.step()).counts.thread.allocs)
        .collect();
    allocs.sort_unstable();
    allocs[FRAMES / 2]
}

/// The open detail costs what egui charges for its widgets, and nothing
/// for working out what they say.
#[test]
fn the_detail_works_out_nothing_per_frame() {
    let added = frame_allocs();
    println!("a detail frame costs {added}");
    let budget = AllocBudget {
        measured_median: MEASURED_DETAIL_ALLOCS,
        measured_peak: MEASURED_DETAIL_ALLOCS,
        tolerance_allocs: TOLERANCE,
    };
    assert!(
        added <= budget.median_ceiling(),
        "a detail frame costs {added} allocations, measured at {MEASURED_DETAIL_ALLOCS}: \
         a frame must not work out or lay out its text"
    );
    assert!(
        added >= budget.median_floor(),
        "drawing the detail got cheaper: {added} allocations a frame, measured at \
         {MEASURED_DETAIL_ALLOCS}. Write the new figure down."
    );
}

/// Where a steady-state detail frame's allocations go, heaviest call site
/// first. Not a check: run it to find what moved the figure above.
///
///     cargo test -p notedeck_cashflow --test detail_alloc -- --ignored --nocapture
#[test]
#[ignore]
fn where_a_detail_frame_allocates() {
    let mut harness = warm();
    for site in attribute(|| harness.step()).sites.iter().take(40) {
        println!("{:5} {:8}  {}", site.allocs, site.bytes, site.frame);
    }
}
