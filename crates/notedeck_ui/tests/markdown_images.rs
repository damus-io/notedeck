//! A lone https image in Markdown draws as the picture, its title the caption
//! under it, through the same `render_markdown_with_refs` path a card
//! description or a note takes; an image in the middle of a sentence stays
//! its link. The picture is seeded into the host's disk cache, so nothing
//! leaves the machine.

use std::time::Duration;

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use nostrdb::Transaction;
use notedeck_testing::ui::wait_for_label;
use notedeck_testing::TestHost;
use notedeck_ui::markdown::render_markdown_with_refs;

/// Where the seeded picture claims to live: https, so the renderer draws it;
/// port 9 (discard), so the mime probe the media cache makes fails at once.
const SHOT_URL: &str = "https://127.0.0.1:9/shot.png";

/// The seeded picture's size, narrower than the window so a draw at it
/// shows nothing upscaled.
const SHOT_SIZE: [usize; 2] = [120, 40];

struct ImageState {
    host: TestHost,
    body: String,
}

/// One frame: the host's background half (media jobs land their textures),
/// then the body as a card description draws.
fn frame(ui: &mut egui::Ui, state: &mut ImageState) {
    notedeck::test_harness::full_window(ui, |ui| {
        if state.host.setup_once(ui.ctx()) {
            return;
        }
        // No app installed, so this is only the background half of a frame.
        state.host.notedeck().tick(ui);
        let mut app_ctx = state.host.app_context();
        let txn = Transaction::new(app_ctx.ndb).expect("txn");
        let mut note_ctx = app_ctx.note_context();
        egui::CentralPanel::default().show(ui, |ui| {
            render_markdown_with_refs(ui, &mut note_ctx, &txn, &state.body);
        });
    });
}

#[test]
fn a_lone_https_image_draws_with_its_caption() {
    let mut host = TestHost::new();
    {
        let app_ctx = host.app_context();
        let image = egui::ColorImage::filled(SHOT_SIZE, egui::Color32::from_rgb(200, 120, 40));
        notedeck::MediaCache::write(&app_ctx.img_cache.static_imgs.cache_dir, SHOT_URL, image)
            .expect("the picture goes in the disk cache");
    }
    let body = format!(
        "Before.\n\n![A seeded shot]({SHOT_URL} \"Its caption.\")\n\n\
         See ![inline]({SHOT_URL}) here.\n"
    );
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(600.0, 400.0))
        .build_ui_state(frame, ImageState { host, body });

    wait_for_label(&mut harness, "A seeded shot", Duration::from_secs(10));
    let shot = harness.get_by_label("A seeded shot").rect();
    assert!(
        (shot.width() - SHOT_SIZE[0] as f32).abs() < 0.5,
        "drawn at its own width, not the column's: {shot:?}"
    );
    assert!(harness.query_by_label("Its caption.").is_some());
    assert!(
        harness.query_by_label("[Image: A seeded shot]").is_none(),
        "the picture, not its link"
    );
    assert!(
        harness.query_by_label("[Image: inline]").is_some(),
        "an image mid-sentence stays a link"
    );
}
