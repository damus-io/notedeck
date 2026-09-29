//! What one steady-state frame of [`git_patch_ui`] allocates on the UI thread.
//!
//! The patch widget is virtualized, so a frame lays out only the rows in view.
//! The rule it has to meet (CLAUDE.md rule 18) is that those rows don't
//! allocate once they are on screen: a diff line's gutter and highlighted
//! content are laid out when the row scrolls in and cached in
//! [`GitPatchState`], so repainting the same view builds no strings, no
//! `LayoutJob`s and no galleys.
//!
//! egui itself still allocates every pass (its shape lists, layer maps,
//! memory), and that is not this widget's to fix. So the check here is the
//! *marginal* cost of a row: the same patch, scrolled to the same place, in a
//! short view and in a tall one. Whatever the tall view allocates beyond the
//! short one is what the extra rows cost.

use egui::{CentralPanel, Context, Pos2, RawInput, Rect};
use notedeck::Localization;
use notedeck_testing::alloc::{measure, CountingAllocator};
use notedeck_ui::diff::{git_patch_ui, GitPatch, GitPatchState, PatchScroll};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Rows a short view shows fewer than a tall one; comfortably more than the
/// short view's whole screenful, so the tall view is mostly extra rows.
const SHORT_HEIGHT: f32 = 300.0;
const TALL_HEIGHT: f32 = 1500.0;

/// A one-file patch of `lines` Rust-ish lines, cycling context, deletion and
/// insertion so every row kind and plenty of token kinds are on screen.
fn rust_patch(lines: usize) -> GitPatch {
    let mut text = format!(
        "diff --git a/src/big.rs b/src/big.rs\n--- a/src/big.rs\n+++ b/src/big.rs\n@@ -1,{lines} +1,{lines} @@\n"
    );
    for i in 0..lines {
        let prefix = [' ', '-', '+'][i % 3];
        text.push_str(&format!(
            "{prefix}    let value_{i} = compute(\"row {i}\", {i}); // step {i}\n"
        ));
    }
    GitPatch::parse(text)
}

/// A context showing `patch` in a view `height` tall, scrolled well into the
/// file so every visible row is a diff line, and settled: fonts loaded, the
/// scroll applied, the view's rows laid out once.
fn settled(patch: &GitPatch, height: f32, selectable: bool) -> (Context, GitPatchState, RawInput) {
    let ctx = Context::default();
    ctx.style_mut(|s| s.interaction.selectable_labels = selectable);
    let mut state = GitPatchState::new(patch, &mut Localization::default());
    state.scroll(PatchScroll::Rows(200));
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, height))),
        ..Default::default()
    };
    for _ in 0..8 {
        frame(&ctx, patch, &mut state, input.clone());
    }
    (ctx, state, input)
}

fn frame(ctx: &Context, patch: &GitPatch, state: &mut GitPatchState, input: RawInput) {
    let _ = ctx.run(input, |ctx| {
        CentralPanel::default().show(ctx, |ui| git_patch_ui(patch, state, ui));
    });
}

/// Allocations (fresh + regrown) one steady-state frame makes on this thread.
fn frame_allocs(patch: &GitPatch, height: f32, selectable: bool) -> u64 {
    let (ctx, mut state, input) = settled(patch, height, selectable);
    let ((), counts) = measure(|| frame(&ctx, patch, &mut state, input));
    eprintln!("height {height}, selectable {selectable}: {counts}");
    counts.thread.allocs + counts.thread.reallocs
}

/// What the tall view's extra rows cost per frame, over the short view's.
fn marginal_allocs(selectable: bool) -> u64 {
    let patch = rust_patch(900);
    let short = frame_allocs(&patch, SHORT_HEIGHT, selectable);
    let tall = frame_allocs(&patch, TALL_HEIGHT, selectable);
    tall.saturating_sub(short)
}

/// With selection off, a visible diff line costs nothing per frame: its
/// galleys come from the cache and are painted without a child `Ui` or a
/// label.
#[test]
fn diff_rows_do_not_allocate_per_frame() {
    assert_eq!(marginal_allocs(false), 0);
}

/// With selection on (egui's default), each row's content goes through
/// egui's `LabelSelectionState`, which loads and re-stores its state per
/// selectable label and boxes it on every store: one allocation per row that
/// this widget can't avoid without giving up text selection.
///
/// Measured, not chosen: the tall view shows 57 more rows than the short one.
/// Before the galley cache the same 57 rows cost 855 (a gutter `String`, a
/// `LayoutJob` and its regrowth, a `horizontal` child `Ui` and two labels'
/// selection state per row).
#[test]
fn selectable_rows_cost_only_egui_selection_state() {
    assert_eq!(marginal_allocs(true), 57);
}

/// Where a tall selectable frame's allocations go, heaviest first:
/// `cargo test -p notedeck_ui --test patch_alloc -- --ignored --nocapture`
#[test]
#[ignore]
fn attribute_tall_frame() {
    let patch = rust_patch(900);
    let (ctx, mut state, input) = settled(&patch, TALL_HEIGHT, true);
    let ((), sites) = notedeck_testing::alloc::attribute(|| frame(&ctx, &patch, &mut state, input));
    for s in sites.iter().take(25) {
        eprintln!("{:5} {:8}  {}", s.allocs, s.bytes, s.frame);
    }
}
