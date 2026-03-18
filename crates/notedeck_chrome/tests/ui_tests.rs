use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use notedeck::{App, Notedeck};
use notedeck_columns::Damus;

// ---------------------------------------------------------------------------
// Phase 2a: Pure egui smoke tests (no notedeck state)
// ---------------------------------------------------------------------------

#[test]
fn smoke_test_harness() {
    let harness = Harness::new_ui(|ui| {
        ui.label("Hello Notedeck");
        let _ = ui.button("Click me");
    });
    harness.get_by_label("Click me");
}

#[test]
fn smoke_test_checkbox_interaction() {
    let mut harness = Harness::new_ui_state(
        |ui, checked| {
            ui.checkbox(checked, "Enable notifications");
        },
        false,
    );

    harness.get_by_label("Enable notifications").click();
    harness.run();

    assert!(*harness.state(), "Checkbox should be checked after click");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_basic_ui() {
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(400.0, 300.0))
        .renderer(notedeck::software_renderer())
        .build_ui(|ui| {
            ui.heading("Notedeck");
            ui.separator();
            ui.label("A nostr browser");
            let _ = ui.button("Login");
        });

    harness.run();

    harness.snapshot("basic_ui");
}

// ---------------------------------------------------------------------------
// Phase 2b/2c: Real Notedeck + Damus widget tests
// ---------------------------------------------------------------------------

/// State bundle for harness tests that need both Notedeck context and a Damus app.
/// Separate fields enable split borrows: `app_context()` borrows `&mut notedeck`
/// while `render()` borrows `&mut damus` — no conflict.
struct TestState {
    notedeck: Notedeck,
    damus: Damus,
    // hold tmpdir so it doesn't get cleaned up while tests run
    _tmpdir: tempfile::TempDir,
    // egui defers set_fonts — definitions aren't available until the next
    // ctx.run(). We set up fonts on the first frame and skip rendering;
    // subsequent frames have fonts loaded and render normally.
    fonts_installed: bool,
}

/// Create a Notedeck + Damus pair initialized in a fresh tmpdir.
/// Note: fonts/theme setup is NOT done here — the harness creates its own
/// egui::Context, so setup() must be called inside the harness closure
/// (which runs during construction). See `render_damus_frame`.
fn make_test_state(egui_ctx: &egui::Context) -> TestState {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let args: Vec<String> = vec![
        "notedeck-test".into(), // argv[0]: consumed by init as program name
        "--testrunner".into(),
    ];
    let mut notedeck_ctx = Notedeck::init(egui_ctx, tmpdir.path(), &args);
    let damus = Damus::new(&mut notedeck_ctx.notedeck.app_context(egui_ctx), &args);
    TestState {
        notedeck: notedeck_ctx.notedeck,
        damus,
        _tmpdir: tmpdir,
        fonts_installed: false,
    }
}

/// Render one frame of Damus inside a CentralPanel.
///
/// On the first call, installs notedeck fonts/theme on the harness's egui
/// context and skips rendering — `set_fonts` is deferred in egui, so fonts
/// aren't usable until the next `ctx.run()`. The harness's `run_ok()` loop
/// (called during construction) will invoke this again with fonts loaded.
fn render_damus_frame(ctx: &egui::Context, state: &mut TestState) {
    if !state.fonts_installed {
        state.notedeck.setup(ctx);
        state.fonts_installed = true;
        return;
    }
    let mut app_ctx = state.notedeck.app_context(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.damus.render(&mut app_ctx, ui);
    });
}

#[test]
fn test_damus_renders() {
    let ctx = egui::Context::default();
    let state = make_test_state(&ctx);

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(800.0, 600.0))
        .build_state(render_damus_frame, state);

    harness.run();
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_damus_columns() {
    let ctx = egui::Context::default();
    let state = make_test_state(&ctx);

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(800.0, 600.0))
        .renderer(notedeck::software_renderer())
        .build_state(render_damus_frame, state);

    harness.run();

    harness.snapshot("damus_columns");
}

// ---------------------------------------------------------------------------
// Viewport size regression snapshots
// ---------------------------------------------------------------------------

fn snapshot_at_size(width: f32, height: f32, name: &str) {
    let ctx = egui::Context::default();
    let state = make_test_state(&ctx);

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(width, height))
        .renderer(notedeck::software_renderer())
        .build_state(render_damus_frame, state);

    harness.run();
    harness.snapshot(name);
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_mobile() {
    snapshot_at_size(375.0, 667.0, "damus_mobile");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_tablet() {
    snapshot_at_size(1024.0, 768.0, "damus_tablet");
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_desktop_wide() {
    snapshot_at_size(1400.0, 900.0, "damus_desktop_wide");
}

// ---------------------------------------------------------------------------
// Light mode snapshot
// ---------------------------------------------------------------------------

/// Same as render_damus_frame but switches to light theme after font setup.
fn render_damus_frame_light(ctx: &egui::Context, state: &mut TestState) {
    if !state.fonts_installed {
        state.notedeck.setup(ctx);
        ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Light);
        state.fonts_installed = true;
        return;
    }
    let mut app_ctx = state.notedeck.app_context(ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.damus.render(&mut app_ctx, ui);
    });
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_light_mode() {
    let ctx = egui::Context::default();
    let state = make_test_state(&ctx);

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(800.0, 600.0))
        .renderer(notedeck::software_renderer())
        .build_state(render_damus_frame_light, state);

    harness.run();
    harness.snapshot("damus_light_mode");
}

// ---------------------------------------------------------------------------
// Auto-update bar snapshot
// ---------------------------------------------------------------------------

/// State for tick()-based tests — the Damus app is set on Notedeck via set_app,
/// so tick() handles all rendering through the real code path.
struct TickTestState {
    notedeck: Notedeck,
    _tmpdir: tempfile::TempDir,
    fonts_installed: bool,
}

/// Render via Notedeck::tick(), which runs the full app loop
/// including the auto-update bar rendering.
fn render_notedeck_tick(ctx: &egui::Context, state: &mut TickTestState) {
    if !state.fonts_installed {
        state.notedeck.setup(ctx);
        ctx.style_mut(|s| s.animation_time = 0.0);
        state.fonts_installed = true;
        return;
    }
    state.notedeck.tick(ctx);
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_update_bar() {
    // tick() uses tokio (media jobs, relay limits)
    let _rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = _rt.enter();

    use notedeck::updater::nostr::test_helpers;

    let ctx = egui::Context::default();
    let tmpdir = tempfile::TempDir::new().unwrap();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let mut notedeck_ctx = Notedeck::init(&ctx, tmpdir.path(), &args);
    let damus = Damus::new(&mut notedeck_ctx.notedeck.app_context(&ctx), &args);
    notedeck_ctx.notedeck.set_app(damus);

    // Point the updater at our test signing key
    notedeck_ctx
        .notedeck
        .set_release_pubkey(test_helpers::TEST_PUBKEY);

    // Ingest a properly signed kind 1063 release event
    let ev = test_helpers::build_signed_release_event(
        &test_helpers::TEST_SECRET_KEY,
        "99.0.0",
        notedeck::updater::nostr::target_asset_name(),
        "https://example.com/download/notedeck.tar.gz",
        "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
    );
    {
        let app_ctx = notedeck_ctx.notedeck.app_context(&ctx);
        app_ctx
            .ndb
            .process_event_with(&ev, nostrdb::IngestMetadata::new())
            .unwrap();
    }

    // Force updater into ReadyToInstall (the event was ingested and would
    // be discovered by tick(), but the download would fail in tests since
    // the URL is fake — so we skip straight to ReadyToInstall)
    notedeck_ctx
        .notedeck
        .force_update_ready("99.0.0".to_string());

    let state = TickTestState {
        notedeck: notedeck_ctx.notedeck,
        _tmpdir: tmpdir,
        fonts_installed: false,
    };

    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(800.0, 600.0))
        .renderer(notedeck::software_renderer())
        .build_state(render_notedeck_tick, state);

    harness.snapshot("update_bar");
}
