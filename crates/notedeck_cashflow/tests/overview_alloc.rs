//! The Overview works out nothing per frame.
//!
//! Its figures, deltas, trend and top lists are computed when the books, the
//! period or today change (`OverviewState::refresh`) and drawn from there, so
//! a steady-state frame pays only what egui charges to lay out and draw them.
//! This draws the synthetic journal's Overview through the real app over a
//! real `Notedeck`, and an empty view in the same app (Goals, with no
//! goals: a heading and a hint), and holds the difference to a measured
//! figure. A query or a `format!` slipped into the
//! per-frame path costs at least one allocation per tile or per top-list
//! row, which the tolerance has no room for; recomputing the figures each
//! frame costs hundreds.

mod common;

use common::{CashflowTestState, harness, synthetic_app};
use egui_kittest::Harness;
use notedeck_cashflow::View;
use notedeck_testing::alloc::{AllocBudget, CountingAllocator, attribute, measure};

/// Process-wide and one per binary, so it lives in this test rather than in
/// `notedeck_testing`.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Frames run before measuring, so fonts, galleys, ids and the figures are
/// all warm.
const WARM_UP: usize = 30;

/// Frames measured; the median is what is held to the budget.
const FRAMES: usize = 15;

/// What drawing the Overview adds to a steady-state frame over the empty
/// Goals view, measured on the UI thread: a layer chip, four tiles
/// with two deltas, the chart, and eight top-list bars with their labels.
/// Attributed with [`where_an_overview_frame_allocates`] when it was
/// written down, nearly all of it is egui's: the accesskit nodes and text
/// it builds for every label (about half), `RichText` owning its string,
/// and each label's layout job. Of the Overview's own, the bars are the
/// Categories view's `bar_row`, which hands its three texts to the painter
/// by clone (about 40), and the chart asks for its axis labels as `String`s
/// (about 20). Neither is a figure worked out per frame. It was 468 over
/// the Adventure view's old one-line placeholder; the empty Goals view
/// that replaced it as the baseline costs 83 more, and the Overview's own
/// 617 didn't move. Measured, not chosen; if it moves, measure it again
/// and write the new figure down.
const MEASURED_OVERVIEW_ALLOCS: u64 = 385;

/// How far above the measured figure a frame may land and still be
/// unchanged: a different machine has measured a frame one or two
/// allocations apart (see [`AllocBudget::tolerance_allocs`]), while one
/// allocation added per row costs eight.
const TOLERANCE: u64 = 4;

/// The median allocations on the UI thread of a steady-state frame of the
/// synthetic app showing `view`.
fn frame_allocs(view: View) -> u64 {
    let mut app = synthetic_app();
    app.set_view(view);
    let mut harness: Harness<'static, CashflowTestState> = harness(app, common::DESKTOP);
    for _ in 0..WARM_UP {
        harness.step();
    }
    let mut allocs: Vec<u64> = (0..FRAMES)
        .map(|_| measure(|| harness.step()).counts.thread.allocs)
        .collect();
    allocs.sort_unstable();
    allocs[FRAMES / 2]
}

/// The Overview adds what egui charges for its widgets, and nothing for
/// working out what they say.
#[test]
fn the_overview_works_out_nothing_per_frame() {
    let empty = frame_allocs(View::Goals);
    let overview = frame_allocs(View::Overview);
    let added = overview.saturating_sub(empty);
    println!("overview {overview}, empty {empty}: the overview adds {added}");
    let budget = AllocBudget {
        measured_median: MEASURED_OVERVIEW_ALLOCS,
        measured_peak: MEASURED_OVERVIEW_ALLOCS,
        tolerance_allocs: TOLERANCE,
    };
    assert!(
        added <= budget.median_ceiling(),
        "drawing the Overview costs {added} allocations a frame ({overview} with it, \
         {empty} with an empty view), measured at {MEASURED_OVERVIEW_ALLOCS}: \
         a frame must not query the books or format a figure"
    );
    assert!(
        added >= budget.median_floor(),
        "drawing the Overview got cheaper: {added} allocations a frame, measured at \
         {MEASURED_OVERVIEW_ALLOCS}. Write the new figure down."
    );
}

/// Where a steady-state Overview frame's allocations go, heaviest call site
/// first. Not a check: run it to find what moved the figure above.
///
///     cargo test -p notedeck_cashflow --test overview_alloc -- --ignored --nocapture
#[test]
#[ignore]
fn where_an_overview_frame_allocates() {
    let mut harness: Harness<'static, CashflowTestState> =
        harness(synthetic_app(), common::DESKTOP);
    for _ in 0..WARM_UP {
        harness.step();
    }
    for site in attribute(|| harness.step()).sites.iter().take(40) {
        println!("{:5} {:8}  {}", site.allocs, site.bytes, site.frame);
    }
}
