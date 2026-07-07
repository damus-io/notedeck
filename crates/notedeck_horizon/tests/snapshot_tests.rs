//! Snapshot tests for the Horizon calendar UI.
//!
//! These seed a handful of NIP-52 calendar events (time-based 31923 and
//! date-based 31922) into a throwaway nostrdb, then render the three-pane
//! calendar and snapshot it. Like the other Notedeck snapshot suites they are
//! `#[ignore]`d and run via `scripts/snapshot-test` (lavapipe) on CI.

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use egui_kittest::Harness;
use enostr::{FullKeypair, Keypair, SecretKey};
use nostrdb::{IngestMetadata, Ndb, NoteBuilder};
use notedeck::{App, Notedeck};
use notedeck_horizon::{Horizon, View};
use std::time::{Duration, Instant};

/// How many calendar events [`seed_calendar`] ingests (2 all-day + 6 timed);
/// the harness waits for all of them before snapshotting.
const SEEDED_EVENTS: usize = 8;

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
        let app_ctx = &mut state.notedeck.app_context();
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

    let mut app_ctx = state.notedeck.app_context();
    // Drive the app's data load (subscribe + reload) then render.
    state.horizon.update(&mut app_ctx, ctx);
    egui::CentralPanel::default().show(ctx, |ui| {
        state.horizon.render(&mut app_ctx, ui);
    });
}

/// Ingest one signed note built by `builder`. Pins `created_at` to the fixed
/// clock so nostrdb returns the seeded notes in a stable order — otherwise
/// same-start events tie-break on the wall clock and reshuffle between runs.
fn ingest(ndb: &Ndb, builder: NoteBuilder, secret: &[u8; 32]) {
    let note = builder
        .created_at(fixed_now().timestamp() as u64)
        .sign(secret)
        .build()
        .expect("note builds");
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

/// A fixed reference "now" the tests pin Horizon to (via [`Horizon::pin_now`]),
/// so the now-line, the "today" badge and the seeded demo calendar all render
/// deterministically instead of drifting with the wall clock. Constructing it
/// from local calendar fields (Monday, 29 June 2026, 09:30) keeps the rendered
/// times identical regardless of the machine's timezone. It is also the day the
/// demo calendar is seeded around, so events land on the focused day.
fn fixed_now() -> DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 6, 29, 9, 30, 0)
        .single()
        .expect("valid local time")
}

/// A fixed signing account, so the seeded notes' ids are stable across runs. A
/// random author reshuffles same-start events (their ids tie-break the order),
/// which would make the snapshots non-deterministic.
fn test_account() -> FullKeypair {
    let secret = SecretKey::from_slice(&[7u8; 32]).expect("valid secret key");
    let keypair = Keypair::from_secret(secret.clone());
    FullKeypair::new(keypair.pubkey, secret)
}

/// Seed a small, entirely fictional demo calendar around "today" so the day
/// view and agenda have content to render against.
fn seed_calendar(ndb: &Ndb, secret: &[u8; 32]) {
    let today = fixed_now().date_naive();
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

    // Pin the reference clock before the first render so the now-line, badge
    // and date highlighting are deterministic across runs and machines.
    let mut horizon = Horizon::default();
    horizon.pin_now(fixed_now());

    let state = HorizonTestState {
        notedeck,
        horizon,
        account: test_account(),
        _tmpdir: tmpdir,
        setup_done: false,
    };

    let mut harness = Harness::builder()
        .with_size(size)
        .with_max_steps(16)
        .renderer(renderer())
        .build_state(render_horizon, state);

    // Seeded notes ingest on nostrdb's writer thread, so pump frames until the
    // app has reloaded them all before any snapshot — otherwise the first size
    // rendered can race a half-loaded calendar and differ between runs.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        harness.run_steps(2);
        if harness.state().horizon.loaded_block_count() >= SEEDED_EVENTS {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the seeded calendar to load"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    harness
}

/// Viewports to snapshot the day view at, from a phone in portrait up to a
/// desktop. The three-pane layout is fixed-width (a ~300px sidebar + ~320px
/// inspector), so the narrow sizes exercise how it degrades when the timeline
/// is squeezed — see the `horizon: responsive layout` follow-up.
const SIZES: &[(&str, f32, f32)] = &[
    ("horizon_phone_portrait", 390.0, 844.0),
    ("horizon_phone_landscape", 844.0, 390.0),
    ("horizon_tablet", 768.0, 1024.0),
    ("horizon_desktop", 1400.0, 900.0),
];

// No baseline images are committed yet: the goldens must be generated on the
// canonical lavapipe renderer (Linux) so they're reproducible in CI. Generate
// them with `scripts/snapshot-test --update` on Linux, then commit the
// resulting `tests/snapshots/horizon_*.png`. Until then this test has nothing
// to compare against and will write `.new.png` files rather than pass.
#[test]
#[ignore] // requires a GPU/lavapipe renderer — run via scripts/snapshot-test
fn snapshot_horizon_day() {
    let mut harness = horizon_harness(egui::Vec2::new(1400.0, 900.0));

    for &(name, w, h) in SIZES {
        harness.set_size(egui::Vec2::new(w, h));
        harness.run_steps(4);
        harness.snapshot(name);
    }

    // Week view: the full seven columns on desktop, but only a three-day
    // window on a phone (seven are unreadable at ~390px).
    harness.state_mut().horizon.set_view(View::Week);
    harness.set_size(egui::Vec2::new(1400.0, 900.0));
    harness.run_steps(4);
    harness.snapshot("horizon_week_desktop");

    harness.set_size(egui::Vec2::new(390.0, 844.0));
    harness.run_steps(4);
    harness.snapshot("horizon_week_phone");

    // Month view: a six-week day-cell grid with truncated event chips, on both
    // desktop and phone widths (the phone cells simply carry fewer chips).
    harness.state_mut().horizon.set_view(View::Month);
    harness.set_size(egui::Vec2::new(1400.0, 900.0));
    harness.run_steps(4);
    harness.snapshot("horizon_month_desktop");

    harness.set_size(egui::Vec2::new(390.0, 844.0));
    harness.run_steps(4);
    harness.snapshot("horizon_month_phone");

    // The fullscreen event-detail view: below desktop width there's no
    // inspector pane, so a selected event opens over the timeline with a
    // back / prev-next nav bar. Open it on the first timed event and snapshot
    // at phone-portrait width.
    harness.state_mut().horizon.set_view(View::Day);
    harness.state_mut().horizon.open_first_timed_detail();
    harness.set_size(egui::Vec2::new(390.0, 844.0));
    harness.run_steps(4);
    harness.snapshot("horizon_event_detail");
}
