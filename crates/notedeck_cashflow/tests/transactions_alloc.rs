//! Scrolling the register works out nothing per frame.
//!
//! The register's matches are worked out when the books or the filters
//! change, and a row's text when it scrolls into view
//! (`register::Window`), so a frame that holds still after a scroll pays
//! only what egui charges to paint the rows on screen. This scrolls a
//! three-thousand-row register part way down, through the real app over a
//! real `Notedeck`, and holds what a steady-state frame adds over a
//! placeholder view to a measured figure. Formatting a row's date or
//! amount per frame costs at least one allocation per row on screen, which
//! the tolerance has no room for; matching the filters again costs
//! thousands.

mod common;

use common::{CashflowTestState, DESKTOP, harness, loaded_app, long_register};
use egui_kittest::Harness;
use notedeck_cashflow::View;
use notedeck_testing::alloc::{AllocBudget, CountingAllocator, attribute, measure};

/// Process-wide and one per binary, so it lives in this test rather than in
/// `notedeck_testing`.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// How many transactions the register lists.
const ROWS: usize = 3000;

/// Frames run before measuring, so fonts, galleys and the row window are
/// all warm.
const WARM_UP: usize = 30;

/// Frames measured; the median is what is held to the budget.
const FRAMES: usize = 15;

/// What a steady-state frame of the scrolled register adds over a
/// placeholder view, measured on the UI thread: the tab row, the filter
/// bar, the header and the twenty-odd rows on screen. Attributed with
/// [`where_a_register_frame_allocates`] when it was written down, it is
/// egui's: accesskit nodes and `RichText` strings for the tab row and the
/// filter bar's widgets, and the header's five painted texts. The rows
/// themselves add nothing: each is laid out once, when it scrolls into
/// view, and painted from its galleys (before that, painting their texts
/// cost 240 more). Measured, not chosen; if it moves, measure it again and
/// write the new figure down.
const MEASURED_REGISTER_ALLOCS: u64 = 190;

/// How far above the measured figure a frame may land and still be
/// unchanged: a different machine has measured a frame one or two
/// allocations apart, while one allocation added per row costs
/// twenty-five.
const TOLERANCE: u64 = 4;

/// A harness of the long register showing `view`, scrolled part way down
/// when it is the Transactions view, and warmed up.
fn scrolled(view: View) -> Harness<'static, CashflowTestState> {
    let journal = long_register(ROWS);
    let mut app = loaded_app(&journal.store);
    app.set_view(view);
    // The period that covers the whole made-up ledger.
    app.set_period(notedeck_cashflow::Period::AllTime);
    let mut harness = harness(app, DESKTOP);
    harness.event(egui::Event::PointerMoved(egui::pos2(600.0, 500.0)));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -4000.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..WARM_UP {
        harness.step();
    }
    harness
}

/// The median allocations on the UI thread of a steady-state frame of
/// `view`.
fn frame_allocs(view: View) -> u64 {
    let mut harness = scrolled(view);
    let mut allocs: Vec<u64> = (0..FRAMES)
        .map(|_| measure(|| harness.step()).counts.thread.allocs)
        .collect();
    allocs.sort_unstable();
    allocs[FRAMES / 2]
}

/// The scrolled register adds what egui charges to paint its rows, and
/// nothing for working out what they say.
#[test]
fn the_register_works_out_nothing_per_frame() {
    let placeholder = frame_allocs(View::Goals);
    let register = frame_allocs(View::Transactions);
    let added = register.saturating_sub(placeholder);
    println!("register {register}, placeholder {placeholder}: the register adds {added}");
    let budget = AllocBudget {
        measured_median: MEASURED_REGISTER_ALLOCS,
        measured_peak: MEASURED_REGISTER_ALLOCS,
        tolerance_allocs: TOLERANCE,
    };
    assert!(
        added <= budget.median_ceiling(),
        "drawing the scrolled register costs {added} allocations a frame ({register} with \
         it, {placeholder} with a placeholder), measured at {MEASURED_REGISTER_ALLOCS}: a \
         frame must not match the filters or format a row"
    );
    assert!(
        added >= budget.median_floor(),
        "drawing the register got cheaper: {added} allocations a frame, measured at \
         {MEASURED_REGISTER_ALLOCS}. Write the new figure down."
    );
}

/// Where a steady-state register frame's allocations go, heaviest call
/// site first. Not a check: run it to find what moved the figure above.
///
///     cargo test -p notedeck_cashflow --test transactions_alloc -- --ignored --nocapture
#[test]
#[ignore]
fn where_a_register_frame_allocates() {
    let mut harness = scrolled(View::Transactions);
    for site in attribute(|| harness.step()).sites.iter().take(40) {
        println!("{:5} {:8}  {}", site.allocs, site.bytes, site.frame);
    }
}
