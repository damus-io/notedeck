//! Normal mode's cue on the chat input costs nothing per frame beyond what
//! insert mode already pays.
//!
//! The cue replaces the input's placeholder instead of adding to it, so both
//! modes should run the same one `tr!` and draw the same number of widgets.
//! This measures a steady-state frame of the real `InputboxLayout` in each mode
//! and holds normal mode to insert mode's count.

use egui_kittest::Harness;
use notedeck::Localization;
use notedeck_dave::ui::{InputMode, InputboxLayout};
use notedeck_testing::alloc::{measure, CountingAllocator};

/// Process-wide and one per binary, so it lives in this test rather than in
/// `notedeck_testing`.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Frames run before measuring, so fonts, galleys and ids are all warm.
const WARM_UP: usize = 30;

/// Allocations on the UI thread for one steady-state frame of the input in
/// `mode`, with an empty draft (the state the cue is visible in) and the Stop
/// button drawn if `show_stop`.
fn frame_allocs(mode: InputMode, show_stop: bool) -> u64 {
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(600.0, 120.0))
        .build_ui_state(
            move |ui, (text, i18n): &mut (String, Localization)| {
                InputboxLayout::new(text, i18n, mode)
                    .show_stop(show_stop)
                    .show(ui);
            },
            (String::new(), Localization::default()),
        );
    for _ in 0..WARM_UP {
        harness.step();
    }
    let ((), counts) = measure(|| harness.step());
    println!("{mode:?}: {counts}");
    counts.thread.allocs
}

/// Each normal-mode cue against insert mode with the same buttons: idle (no
/// Stop button) and mid-turn (Stop drawn, and the cue offers `s`).
#[test]
fn normal_mode_cue_allocates_no_more_than_insert() {
    for can_stop in [false, true] {
        let insert = frame_allocs(InputMode::Insert, can_stop);
        let mode = InputMode::Normal { can_stop };
        let normal = frame_allocs(mode, can_stop);
        assert!(
            normal <= insert,
            "{mode:?} allocated {normal} a frame, insert mode {insert}: \
             the cue must swap the placeholder, not add to it"
        );
    }
}
