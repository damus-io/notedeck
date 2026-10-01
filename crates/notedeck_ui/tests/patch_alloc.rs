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
use notedeck_ui::diff::{
    git_patch_ui, FileImages, GitPatch, GitPatchState, ImageSide, MediaKind, PatchImage,
    PatchModel, PatchScroll,
};

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
    ctx.global_style_mut(|s| s.interaction.selectable_labels = selectable);
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
    ctx.run_ui(input, |ui| {
        CentralPanel::default().show(ui, |ui| git_patch_ui(patch, state, ui));
    })
    .drop_without_applying_deltas();
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
/// egui's `LabelSelectionState`. Up to egui 0.31 that boxed its state on every
/// store, one allocation per row (57 for the tall view's 57 extra rows); since
/// egui 0.36 it no longer does, so selectable rows cost nothing per frame
/// either.
///
/// Measured, not chosen. Before the galley cache the same 57 rows cost 855 (a
/// gutter `String`, a `LayoutJob` and its regrowth, a `horizontal` child `Ui`
/// and two labels' selection state per row).
#[test]
fn selectable_rows_do_not_allocate_per_frame() {
    assert_eq!(marginal_allocs(true), 0);
}

/// A commit changing one PNG.
const IMAGE_PATCH: &str = "\
diff --git a/shot.png b/shot.png
index 1111111..2222222 100644
Binary files a/shot.png and b/shot.png differ
";

/// The before and after of a changed PNG, uploaded to `ctx` once, as a
/// caller does when its load lands.
fn uploaded_images(ctx: &Context) -> FileImages {
    let side = |w, h| {
        Some(ImageSide::Shown(PatchImage {
            texture: ctx.load_texture(
                "side",
                egui::ColorImage::filled([w, h], egui::Color32::GRAY),
                Default::default(),
            ),
            width: w as u32,
            height: h as u32,
            bytes: 4096,
        }))
    };
    FileImages {
        old: side(300, 200),
        new: side(320, 240),
        ..Default::default()
    }
}

/// The before and after of a changed 3D model, as textures the caller
/// renders into (any id will do: nothing here samples them).
fn rendered_models(_ctx: &Context) -> FileImages {
    let side = |id| {
        Some(ImageSide::Model(PatchModel {
            texture: egui::TextureId::User(id),
            size: egui::vec2(320.0, 240.0),
            triangles: 12,
            bytes: 4096,
        }))
    };
    FileImages {
        old: side(1),
        new: side(2),
        kind: MediaKind::Model,
    }
}

/// One steady-state frame of [`IMAGE_PATCH`] with the sides `sides` makes
/// and the file `collapsed` or not.
fn image_frame_allocs(sides: fn(&Context) -> FileImages, collapsed: bool) -> u64 {
    let patch = GitPatch::parse(IMAGE_PATCH);
    let ctx = Context::default();
    let mut state = GitPatchState::new(&patch, &mut Localization::default());
    state.set_file_images(0, sides(&ctx), &mut Localization::default());
    state.set_collapsed(0, collapsed);
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(900.0, TALL_HEIGHT),
        )),
        ..Default::default()
    };
    for _ in 0..8 {
        frame(&ctx, &patch, &mut state, input.clone());
    }
    let ((), counts) = measure(|| frame(&ctx, &patch, &mut state, input));
    eprintln!("image file collapsed {collapsed}: {counts}");
    counts.thread.allocs + counts.thread.reallocs
}

/// A visible image costs nothing per frame: the textures were uploaded when
/// the load landed, the captions are laid out once as they scroll in, and the
/// rows only paint. Measured as the file expanded (its images in view) over
/// the same file collapsed (only its header).
#[test]
fn image_rows_do_not_allocate_per_frame() {
    let shown = image_frame_allocs(uploaded_images, false);
    let hidden = image_frame_allocs(uploaded_images, true);
    assert_eq!(shown.saturating_sub(hidden), 0);
}

/// Nor does a visible 3D model at rest: it paints the caller's texture like
/// an image, and its drag sense records nothing until it's dragged.
#[test]
fn model_rows_do_not_allocate_per_frame() {
    let shown = image_frame_allocs(rendered_models, false);
    let hidden = image_frame_allocs(rendered_models, true);
    assert_eq!(shown.saturating_sub(hidden), 0);
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
