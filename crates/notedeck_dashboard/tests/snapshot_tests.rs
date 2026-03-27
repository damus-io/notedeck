use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use enostr::FullKeypair;
use nostrdb::{Config, FilterBuilder, Ndb, NoteBuilder};
use notedeck::{App, Notedeck};
use notedeck_dashboard::Dashboard;
use notedeck_testing::ui::wait_for_label;

const NUM_NOTES: usize = 50;

struct DashTestState {
    notedeck: Notedeck,
    dashboard: Dashboard,
    _tmpdir: tempfile::TempDir,
    fonts_installed: bool,
}

fn render_dashboard(ctx: &egui::Context, state: &mut DashTestState) {
    if !state.fonts_installed {
        state.notedeck.setup(ctx);
        ctx.style_mut(|s| s.animation_time = 0.0);
        state.fonts_installed = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    state.dashboard.update(&mut app_ctx, ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.dashboard.render(&mut app_ctx, ui);
    });
}

/// Pre-seed notes of various kinds into the ndb before the dashboard boots.
fn seed_dashboard_notes(data_dir: &std::path::Path) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let db_path = notedeck::DataPath::new(data_dir).path(notedeck::DataPathType::Db);
    std::fs::create_dir_all(&db_path).expect("create db dir");

    let ndb = Ndb::new(db_path.to_str().unwrap(), &Config::new()).expect("ndb");
    let account = FullKeypair::generate();
    let account2 = FullKeypair::generate();

    let now = chrono::Utc::now().timestamp() as u64;

    // Subscribe before ingesting so wait_for_all_notes can track them
    let filters = [FilterBuilder::new().build()];
    let sub = ndb.subscribe(&filters).expect("subscribe");

    for i in 0..NUM_NOTES {
        // Spread notes across the last few months so they land in monthly buckets
        let ts = now - (i as u64 * 86400);
        let (kind, content) = match i % 5 {
            0 => (1, format!("text note {i}")),
            1 => (7, "+".to_string()),
            2 => (1, format!("another post {i}")),
            3 => (6, String::new()),
            _ => (1, format!("note {i}")),
        };

        let signer = if i % 2 == 0 { &account } else { &account2 };

        let mut builder = NoteBuilder::new()
            .kind(kind)
            .content(&content)
            .created_at(ts);

        // Add client tags to some notes so the client charts have data
        if kind == 1 {
            builder = builder.start_tag().tag_str("client").tag_str("notedeck");
        }

        let note = builder
            .sign(&signer.secret_key.secret_bytes())
            .build()
            .expect("build note");

        let json = note.json().expect("note json");
        ndb.process_event(&json).expect("ingest note");
    }

    // Wait for all notes to be ingested
    rt.block_on(async {
        ndb.wait_for_all_notes(sub, NUM_NOTES as u32)
            .await
            .expect("wait for all notes");
    });
}

/// Responsive breakpoints to snapshot
const SIZES: &[(&str, f32, f32)] = &[
    ("dashboard_mobile", 400.0, 900.0),
    ("dashboard_tablet", 800.0, 600.0),
    ("dashboard_desktop", 1200.0, 800.0),
    ("dashboard_wide", 1800.0, 800.0),
];

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_dashboard() {
    let tmpdir = tempfile::TempDir::new().unwrap();
    seed_dashboard_notes(tmpdir.path());

    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck_ctx = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = DashTestState {
        notedeck: notedeck_ctx.notedeck,
        dashboard: Dashboard::default(),
        _tmpdir: tmpdir,
        fonts_installed: false,
    };

    // Start at desktop size for initial data load
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(1200.0, 800.0))
        .renderer(notedeck::software_renderer())
        .build_state(render_dashboard, state);

    // Let the dashboard initialize — auto-refresh begins on first frame
    harness.run();

    // Wait for the worker to finish and the event count to appear
    let expected = NUM_NOTES.to_string();
    wait_for_label(&mut harness, &expected, Duration::from_secs(5));

    // Click the refresh button to trigger a second pass
    harness.get_by_label("⟳ Refresh").click();
    harness.run();

    // Wait for the count to stabilize after the second refresh
    wait_for_label(&mut harness, &expected, Duration::from_secs(5));

    // Snapshot at each breakpoint
    for &(name, w, h) in SIZES {
        harness.set_size(egui::Vec2::new(w, h));
        // Run a few frames so layout settles at the new size
        harness.run_steps(3);
        harness.snapshot(name);
    }
}
