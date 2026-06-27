//! Snapshot tests for the Horizon calendar UI.
//!
//! These seed a handful of NIP-52 calendar events (time-based 31923 and
//! date-based 31922) into a throwaway nostrdb, then render the three-pane
//! calendar and snapshot it. Like the other Notedeck snapshot suites they are
//! `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe) on CI.

use chrono::{Local, NaiveDate, TimeZone};
use egui_kittest::Harness;
use enostr::{FullKeypair, Keypair};
use nostrdb::{IngestMetadata, Ndb, NoteBuilder};
use notedeck::{App, Notedeck};
use notedeck_horizon::Horizon;

struct HorizonTestState {
    notedeck: Notedeck,
    horizon: Horizon,
    account: FullKeypair,
    _tmpdir: tempfile::TempDir,
    setup_done: bool,
}

fn render_horizon(ctx: &egui::Context, state: &mut HorizonTestState) {
    // Install fonts/styles and inject a signing account on the first frame,
    // then seed the demo calendar before the app's first `update`.
    if !state.setup_done {
        state.notedeck.setup(ctx);
        ctx.style_mut(|s| s.animation_time = 0.0);

        let secret = state.account.secret_key.clone();
        let pubkey = state.account.pubkey;
        let app_ctx = &mut state.notedeck.app_context(ctx);
        if let Some(resp) = app_ctx.accounts.add_account(Keypair::from_secret(secret)) {
            let txn = nostrdb::Transaction::new(app_ctx.ndb).expect("txn");
            resp.unk_id_action
                .process_action(app_ctx.unknown_ids, app_ctx.ndb, &txn);
        }
        app_ctx.select_account(&pubkey);

        seed_calendar(app_ctx.ndb, &state.account.secret_key.secret_bytes());

        state.setup_done = true;
        return;
    }

    let mut app_ctx = state.notedeck.app_context(ctx);
    // Drive the app's data load (subscribe + reload) then render.
    state.horizon.update(&mut app_ctx, ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.horizon.render(&mut app_ctx, ui);
    });
}

/// Ingest one signed note built by `builder`.
fn ingest(ndb: &Ndb, builder: NoteBuilder, secret: &[u8; 32]) {
    let note = builder.sign(secret).build().expect("note builds");
    let json = enostr::ClientMessage::event(&note)
        .expect("client msg")
        .to_json()
        .expect("json");
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .expect("ingest");
}

/// A NIP-52 time-based (kind 31923) event spanning `[start, end]` unix seconds.
fn timed(ndb: &Ndb, secret: &[u8; 32], id: &str, title: &str, start: i64, end: i64) {
    ingest(
        ndb,
        NoteBuilder::new()
            .content("")
            .kind(31923)
            .start_tag()
            .tag_str("d")
            .tag_str(id)
            .start_tag()
            .tag_str("title")
            .tag_str(title)
            .start_tag()
            .tag_str("start")
            .tag_str(&start.to_string())
            .start_tag()
            .tag_str("end")
            .tag_str(&end.to_string()),
        secret,
    );
}

/// A NIP-52 date-based (kind 31922) all-day event over `[start, end)` dates.
fn all_day(ndb: &Ndb, secret: &[u8; 32], id: &str, title: &str, start: NaiveDate, end: NaiveDate) {
    ingest(
        ndb,
        NoteBuilder::new()
            .content("")
            .kind(31922)
            .start_tag()
            .tag_str("d")
            .tag_str(id)
            .start_tag()
            .tag_str("title")
            .tag_str(title)
            .start_tag()
            .tag_str("start")
            .tag_str(&start.format("%Y-%m-%d").to_string())
            .start_tag()
            .tag_str("end")
            .tag_str(&end.format("%Y-%m-%d").to_string()),
        secret,
    );
}

/// Seed a small, entirely fictional demo calendar around "today" so the day
/// view and agenda have content to render against.
fn seed_calendar(ndb: &Ndb, secret: &[u8; 32]) {
    let today = Local::now().date_naive();
    let unix = |d: NaiveDate, h: u32, m: u32| {
        Local
            .from_local_datetime(&d.and_hms_opt(h, m, 0).unwrap())
            .single()
            .unwrap()
            .timestamp()
    };

    // Today.
    all_day(
        ndb,
        secret,
        "conf",
        "Acme Dev Conference",
        today,
        today + chrono::Duration::days(2),
    );
    all_day(
        ndb,
        secret,
        "release",
        "Release day",
        today,
        today + chrono::Duration::days(1),
    );
    timed(
        ndb,
        secret,
        "deepwork",
        "Deep work block",
        unix(today, 7, 0),
        unix(today, 11, 15),
    );
    timed(
        ndb,
        secret,
        "standup",
        "Team standup",
        unix(today, 9, 0),
        unix(today, 10, 0),
    );
    timed(
        ndb,
        secret,
        "review",
        "Design review",
        unix(today, 13, 0),
        unix(today, 15, 0),
    );

    // Tomorrow.
    let tom = today + chrono::Duration::days(1);
    timed(
        ndb,
        secret,
        "gym",
        "Morning workout",
        unix(tom, 6, 30),
        unix(tom, 7, 30),
    );
    timed(
        ndb,
        secret,
        "oneonone",
        "1:1 with manager",
        unix(tom, 9, 0),
        unix(tom, 10, 0),
    );
    timed(
        ndb,
        secret,
        "lunch",
        "Lunch with the team",
        unix(tom, 12, 0),
        unix(tom, 13, 30),
    );
}

/// Force a CPU/lavapipe renderer on Linux CI for determinism; use the default
/// (Metal/Vulkan) GPU adapter elsewhere so the suite renders on dev machines.
#[cfg(target_os = "linux")]
fn renderer() -> egui_kittest::wgpu::WgpuTestRenderer {
    notedeck::software_renderer()
}
#[cfg(not(target_os = "linux"))]
fn renderer() -> egui_kittest::wgpu::WgpuTestRenderer {
    egui_kittest::wgpu::WgpuTestRenderer::default()
}

fn horizon_harness(size: egui::Vec2) -> Harness<'static, HorizonTestState> {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let ctx = egui::Context::default();
    let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
    let notedeck = Notedeck::init(&ctx, tmpdir.path(), &args);

    let state = HorizonTestState {
        notedeck,
        horizon: Horizon::default(),
        account: FullKeypair::generate(),
        _tmpdir: tmpdir,
        setup_done: false,
    };

    let mut harness = Harness::builder()
        .with_size(size)
        .with_max_steps(16)
        .renderer(renderer())
        .build_state(render_horizon, state);

    // First frame installs fonts + seeds; pump more so ndb ingests and the
    // app's reload picks the events up.
    harness.run_steps(8);
    harness
}

// No baseline image is committed yet: the golden must be generated on the
// canonical lavapipe renderer (Linux) so it's reproducible in CI. Generate it
// with `scripts/snapshot-test --update` on Linux, then commit the resulting
// `tests/snapshots/horizon_day.png`. Until then this test has nothing to
// compare against and will write a `.new.png` rather than pass.
#[test]
#[ignore] // requires a GPU/lavapipe renderer — run via scripts/snapshot-test
fn snapshot_horizon_day() {
    let mut harness = horizon_harness(egui::Vec2::new(1400.0, 900.0));
    harness.run_steps(4);
    harness.snapshot("horizon_day");
}
