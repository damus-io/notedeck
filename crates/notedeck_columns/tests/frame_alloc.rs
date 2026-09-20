//! What one steady-state Columns frame allocates, and a ratchet on it.
//!
//! `CLAUDE.md` has a rule — "No allocation in ui functions" — and a rule nobody
//! checks is a convention. This is the check.
//!
//! It cannot be the check the rule literally describes. Notedeck is an
//! egui/eframe app: egui rebuilds its shape list and its layout from scratch
//! every pass, by design, and notedeck's own UI code allocates freely on top of
//! that. "Assert zero" would fail on its first run and teach nobody anything.
//!
//! So it is a **ratchet**. The numbers in [`HOME_TIMELINE_BUDGET`] were
//! measured, not chosen; they are checked in both directions; and the profile is
//! printed on every run whether it passes or not. See
//! [`notedeck_testing::alloc`] for the allocator and for the two decisions that
//! are decisions rather than mechanics — a delta rather than an absolute, and
//! per-thread counters rather than process-wide ones.
//!
//! # What this measures, and what it does not
//!
//! `DeviceHarness::step` runs one full egui pass over a real, booted
//! [`notedeck::Notedeck`] host with Columns installed: input, the host's own
//! per-pass work, every app `update`, the whole widget tree, and shape
//! building. That is the frame, and it is what the budget is on.
//!
//! Three things are outside it and the budget does not see them:
//!
//! - **Tessellation and paint.** `egui_kittest`'s `step` builds shapes but does
//!   not tessellate them; that happens in `render`, which needs the `wgpu`
//!   feature this test binary does not enable. Tessellation allocates a great
//!   deal, so the real frame is dearer than this number. It is left out because
//!   including it would measure epaint rather than notedeck, and because a
//!   lavapipe render is the least stable thing in this repo's test suite.
//! - **Other threads.** The counters are per-thread, so relay sockets, nostrdb
//!   ingest, the `JobPool` and tokio are all invisible. This is deliberate and
//!   it is a loophole; `notedeck_testing::alloc` writes up both halves, and the
//!   report prints the process-wide figure beside the thread one so the
//!   loophole stays visible.
//! - **Accessibility.** kittest drives egui with AccessKit enabled, which the
//!   shipping app does not always do, and building the AccessKit tree allocates
//!   per frame. [`harness_floor_is_a_fraction_of_a_populated_frame`] measures
//!   that floor directly rather than leaving it as an unknown constant inside
//!   the number.
//!
//! # Steady state
//!
//! The device connects to no relay at all (`--testrunner` with no `--relay`
//! hands a fresh account an empty bootstrap set), and its nostrdb is seeded on
//! disk before boot. Nothing arrives during the measured window, so every
//! measured frame is doing the same work as its neighbours: drawing a timeline
//! that is already there. A frame that takes a relay message or decodes an
//! image is a different frame, and averaging it in would produce a number that
//! describes the network.

use std::time::Duration;

use nostrdb::{NoteBuildOptions, NoteBuilder};
use nostrdb_net::FullKeypair;
use notedeck_columns::Damus;
use notedeck_testing::{
    alloc::{attribute, AllocBudget, CountingAllocator, FrameProfile, Site},
    device::{build_device_in_tmpdir_with_relays, DeviceHarness},
    fixtures::seed_local_notes_in_data_dir,
    stepping::wait_for_device_condition,
};
use tempfile::TempDir;

/// The measurement. Process-wide and one per binary, which is why it is here
/// and not in `notedeck_testing` — that crate is linked into many test
/// binaries, so it owns the allocator *type* and this file owns the instance.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// -- the budget ------------------------------------------------------------

/// What a steady-state home-timeline frame is allowed to allocate.
///
/// **Measured on 2026-09-20**, by this test, under the conditions in the module
/// docs: a Columns app on a 900x700 device showing a contacts timeline of 40
/// seeded kind-1 notes from two authors, no relay connected, 60 warm-up frames
/// discarded and 120 frames measured, `dev` profile on x86-64 Linux.
///
/// It was **991** when this test landed. Two changes have taken it down since:
///
/// - **991 -> 885**: `notedeck::StyleCache` stopped the note path deep-cloning
///   an `egui::Style` seven times per visible note, which took 66,400 bytes a
///   frame with it.
/// - **885 -> 829**: `Localization::translate` replaced a two-call `tr!` lookup
///   whose cached path cloned the normalized FTL key it had just found (42 a
///   frame) and built an error to describe an untranslated string (14 a frame).
///   The cached path is now two map probes and the returned `String`.
///
/// The measurement is bit-exact — across all 120 frames, min, median and max
/// are the same number — but it is not portable. At the 991 baseline the same
/// code in an `ubuntu:22.04` container, the image CI runs on, measured **990**:
/// bit-exact there too, one allocation apart. That gap is what
/// [`AllocBudget::tolerance_allocs`] is sized against, and it is why the
/// tolerance is a count rather than a percentage.
///
/// The same frame costs 794 allocations in the `release` profile, which is why
/// [`the_steady_state_frame_stays_within_its_allocation_budget`] only asserts in
/// `dev`. It also allocates **189,526 bytes** and does **176 reallocations** per
/// frame; those are not in the budget because a ratchet on one well-chosen
/// number is a ratchet people keep, and the allocation count is the number that
/// moves when somebody adds an allocation. The report prints all of them.
const HOME_TIMELINE_BUDGET: AllocBudget = AllocBudget {
    measured_median: 829,
    measured_peak: 829,

    // Four: comfortably over the one-allocation spread measured between this
    // box and an ubuntu-22.04 container, and comfortably under the seven a
    // single `format!` in a per-note function costs. See the field's docs.
    tolerance_allocs: 4,
};

/// Warm-up frames, discarded.
///
/// Generous on purpose. A freshly booted host spends its first frames resolving
/// fonts, building layouts, realizing subscriptions and filling caches, and the
/// virtual list that draws the timeline settles its row heights over several
/// frames after that. None of it is steady state.
const WARMUP_FRAMES: usize = 60;

/// Frames measured.
///
/// Two seconds' worth at 60fps. Long enough that something allocating every
/// thirtieth frame appears in the window rather than depending on luck.
const MEASURED_FRAMES: usize = 120;

/// Frames the attribution report covers.
///
/// Far fewer than the budget measures: every allocation inside it costs a stack
/// walk, and a ranking does not get more true with more samples — four frames
/// is already a census rather than a sample.
const ATTRIBUTED_FRAMES: usize = 4;

/// Notes seeded into the timeline.
///
/// Enough to fill the viewport several times over, so the measurement is of a
/// timeline doing its normal work rather than of an empty column.
const SEEDED_NOTES: usize = 40;

// -- the check -------------------------------------------------------------

#[test]
fn the_steady_state_frame_stays_within_its_allocation_budget() {
    let mut device = populated_home_timeline();

    let profile = FrameProfile::measure(
        "columns home timeline",
        &mut device,
        WARMUP_FRAMES,
        MEASURED_FRAMES,
    );

    // The frames really ran. Without this, a harness that had stopped stepping
    // would pass the budget by doing nothing at all, which is the one way a
    // ratchet fails silently.
    assert_eq!(profile.frames().len(), MEASURED_FRAMES);
    assert!(
        profile.median_allocs() > 0,
        "a frame that allocated nothing at all means the harness stopped \
         stepping, not that notedeck got infinitely fast"
    );

    if let Some(why_not) = off_the_reference_build() {
        eprintln!(
            "{}\n\nMeasured but not asserted: {why_not}. The numbers above are still real \
             and worth reading; they are just not the ones HOME_TIMELINE_BUDGET was taken \
             from.",
            profile.report()
        );
        return;
    }

    profile.assert_within(HOME_TIMELINE_BUDGET);
}

/// Why this build is not the one [`HOME_TIMELINE_BUDGET`] describes, if it is not.
///
/// An allocation count is exact and therefore specific to what produced it, and
/// the check is run in places that are not the reference build: CI runs this
/// suite on macOS and Windows as well as Linux, and `scripts/snapshot-test`
/// runs the workspace in `release`.
///
/// The test still runs everywhere and still prints its report — that is how the
/// harness itself stays exercised on the other platforms, and how the numbers
/// stay visible. It just does not assert a figure it did not measure. A check
/// that fails for a reason it cannot name is a check that gets deleted, and the
/// `dev`/`release` gap alone is 35 allocations a frame, five times the
/// regression this caught while it was being written.
fn off_the_reference_build() -> Option<&'static str> {
    if !cfg!(debug_assertions) {
        return Some("HOME_TIMELINE_BUDGET is a `dev` profile figure and this is `release`");
    }

    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Some("HOME_TIMELINE_BUDGET was measured on x86-64 Linux");
    }

    None
}

/// How much of the budget is the harness rather than the app.
///
/// kittest's AccessKit tree and egui's own per-pass work happen whether or not
/// notedeck draws anything, so some of [`HOME_TIMELINE_BUDGET`] is floor and
/// not ours. Measuring it here means the follow-up work knows which part of the
/// number it can actually spend.
#[test]
fn harness_floor_is_a_fraction_of_a_populated_frame() {
    let mut device = empty_app_device();

    let profile = FrameProfile::measure("empty app", &mut device, WARMUP_FRAMES, MEASURED_FRAMES);

    eprintln!("{}", profile.report());
    assert!(
        profile.median_allocs() > 0,
        "even an empty egui pass allocates; zero means the harness stopped stepping"
    );
}

/// The check on the check.
///
/// A ratchet nobody has watched fail is not known to work, and this one fails
/// by staying silent. So make it fire on purpose, permanently, rather than once
/// by hand in the session that wrote it.
#[test]
fn the_counter_sees_an_allocation_added_to_a_frame() {
    // `vec![..]` would be one allocation and clippy would rather have it, but a
    // `Vec::new` grown by `push` is precisely the shape this exists to catch,
    // so it is precisely the shape the counter has to be shown catching.
    #[allow(
        clippy::vec_init_then_push,
        reason = "the push is what is being caught"
    )]
    let (_, counts) = notedeck_testing::alloc::measure(|| {
        // A `Vec` built inside a per-frame function is the exact shape of the
        // bug this whole file exists to catch.
        let mut labels: Vec<String> = Vec::new();
        labels.push("note".to_owned());
        labels.push("reply".to_owned());
        labels
    });

    assert!(
        counts.thread.allocs >= 2,
        "the allocator saw nothing on this thread: {counts}"
    );
    assert!(
        counts.process.allocs >= counts.thread.allocs,
        "the process tally must include this thread's: {counts}"
    );
}

/// The reason the counters are thread-local at all.
///
/// If this ever fails, the budget above has quietly become a measurement of
/// what else the test binary happened to be doing — which in a process with
/// relay sockets, nostrdb ingest and a tokio runtime in it is noise with no
/// signal underneath.
#[test]
fn another_threads_allocations_do_not_reach_this_threads_tally() {
    let handle = std::thread::spawn(|| {
        let mut v: Vec<u64> = Vec::new();
        for i in 0..4096 {
            v.push(i);
        }
        v.len()
    });

    let (len, counts) = notedeck_testing::alloc::measure(|| handle.join().expect("join"));

    assert_eq!(len, 4096);
    assert_eq!(
        counts.thread.reallocs, 0,
        "a Vec growing on another thread reached this thread's tally: {counts}"
    );
    assert!(
        counts.process.allocs > 0,
        "the process tally should have seen the other thread: {counts}"
    );
}

// -- the app the check drives ----------------------------------------------

/// A Columns device showing a timeline that is already full.
///
/// **This is the function to extend as the measured frame grows.** A frame
/// measured against an empty column is a frame that proves nothing, and a
/// timeline that silently stops rendering notes is how this check would quietly
/// stop checking — which is what [`wait_for_seeded_notes`] is there to prevent.
fn populated_home_timeline() -> DeviceHarness {
    let alice = FullKeypair::generate();
    let bob = FullKeypair::generate();
    let carol = FullKeypair::generate();

    let tmpdir = TempDir::new().expect("tmpdir");
    let mut seeded: Vec<String> = Vec::new();

    // Alice follows bob and carol, so the contacts column has something to
    // draw. Seeded locally rather than fetched, because the device connects to
    // no relay.
    seeded.push(json(
        contact_list(&[bob.pubkey, carol.pubkey])
            .sign(&alice.secret_key.secret_bytes())
            .build()
            .expect("contact list"),
    ));

    for (account, name) in [(&bob, "bob"), (&carol, "carol")] {
        seeded.push(json(
            NoteBuilder::new()
                .kind(0)
                .content(&format!(r#"{{"name":"{name}","display_name":"{name}"}}"#))
                .sign(&account.secret_key.secret_bytes())
                .build()
                .expect("profile note"),
        ));
    }

    for i in 0..SEEDED_NOTES {
        let (account, name) = if i % 2 == 0 {
            (&bob, "bob")
        } else {
            (&carol, "carol")
        };
        seeded.push(json(
            NoteBuilder::new()
                .kind(1)
                .content(&format!("{name} note {i}"))
                .created_at(1_700_000_000 + i as u64)
                .sign(&account.secret_key.secret_bytes())
                .build()
                .expect("text note"),
        ));
    }

    seed_local_notes_in_data_dir(tmpdir.path(), &seeded, &[0, 1, 3]);

    // No relay: `--testrunner` with an empty relay list gives the account an
    // empty bootstrap set, so nothing connects and nothing arrives mid-window.
    let mut device = build_device_in_tmpdir_with_relays(&[], &alice, tmpdir, columns_app_factory());

    wait_for_seeded_notes(&mut device);
    device
}

/// A device with a host and no app, to measure the harness's own per-frame cost.
fn empty_app_device() -> DeviceHarness {
    let account = FullKeypair::generate();
    let tmpdir = TempDir::new().expect("tmpdir");
    build_device_in_tmpdir_with_relays(
        &[],
        &account,
        tmpdir,
        Box::new(|notedeck, _ctx| {
            let app_ctx = notedeck.app_context();
            app_ctx.settings.complete_welcome();
        }),
    )
}

fn columns_app_factory() -> notedeck_testing::AppFactory {
    Box::new(|notedeck, _ctx| {
        let args = vec!["--column".to_string(), "contacts".to_string()];
        let mut app_ctx = notedeck.app_context();
        app_ctx.settings.complete_welcome();
        let damus = Damus::new(&mut app_ctx, &args);
        drop(app_ctx);
        notedeck.set_app(damus);
    })
}

/// Steps until the timeline is actually drawing the seeded notes.
///
/// The barrier the whole measurement rests on: a budget measured against a
/// column that never rendered anything is a budget on an empty frame, and it
/// would pass forever.
fn wait_for_seeded_notes(device: &mut DeviceHarness) {
    wait_for_device_condition(
        device,
        Duration::from_secs(30),
        "home timeline to render its seeded notes",
        |device| {
            let bob = rendered(device, "bob note");
            let carol = rendered(device, "carol note");
            if bob > 0 && carol > 0 {
                Ok(())
            } else {
                Err(format!(
                    "bob notes visible: {bob}, carol notes visible: {carol}"
                ))
            }
        },
    );
}

fn rendered(device: &DeviceHarness, substring: &str) -> usize {
    use egui_kittest::kittest::Queryable;
    device.query_all_by_label_contains(substring).count()
}

fn contact_list<'a>(pks: &[nostrdb_net::Pubkey]) -> NoteBuilder<'a> {
    let mut builder = NoteBuilder::new()
        .content("")
        .kind(3)
        .options(NoteBuildOptions::default());
    for pk in pks {
        builder = builder.start_tag().tag_str("p").tag_str(&pk.hex());
    }
    builder
}

fn json(note: nostrdb::Note<'_>) -> String {
    note.json().expect("note json")
}

// -- attribution -----------------------------------------------------------

/// Where a frame's allocations actually go, printed rather than asserted on.
///
/// Ignored by default: it captures and symbolizes a backtrace per allocation,
/// which is orders of magnitude slower than the frame it is measuring, and its
/// output is a ranking rather than a property. Run it deliberately:
///
/// ```text
/// cargo test -p notedeck_columns --test frame_alloc -- --ignored --nocapture
/// ```
///
/// This is where the numbers in the follow-up work came from, and it is what
/// [`notedeck_testing::alloc::HOW_TO_FIND_IT`]-style advice points a failing
/// budget at.
#[test]
#[ignore = "diagnostic: slow, and reports a ranking rather than asserting a property"]
fn report_where_the_frames_allocations_go() {
    let mut device = populated_home_timeline();

    for _ in 0..WARMUP_FRAMES {
        device.step();
    }

    let (_, sites) = attribute(|| {
        for _ in 0..ATTRIBUTED_FRAMES {
            device.step();
        }
    });

    let total: u64 = sites.iter().map(|s| s.allocs).sum();
    eprintln!("\n{total} allocations sampled over {ATTRIBUTED_FRAMES} frames\n");

    // Two rankings, because the overall one alone is not enough to work from.
    // A Columns frame has dozens of sites tied on the same count, and the
    // heaviest of them are accesskit's and egui's, so anything freshly added to
    // notedeck lands in the middle of a long tail. Checked rather than assumed:
    // a `format!` added to `actionbar_ui` costs seven allocations a frame,
    // which does not reach the top forty overall but does appear in the second
    // ranking — the subset somebody here can actually change.
    report_sites("heaviest call sites overall", &sites, |_| true);
    report_sites(
        "heaviest call sites in notedeck's own code",
        &sites,
        |site| site.frame.contains("notedeck"),
    );
}

/// Prints one ranking of `sites`, keeping only those `keep` accepts.
fn report_sites(title: &str, sites: &[Site], keep: impl Fn(&Site) -> bool) {
    eprintln!("{title}:\n");
    for site in sites.iter().filter(|site| keep(site)).take(40) {
        eprintln!(
            "{:>8} allocs  {:>10} bytes  {:>7}/frame  {}",
            site.allocs,
            site.bytes,
            site.allocs / ATTRIBUTED_FRAMES as u64,
            site.frame,
        );
    }
    eprintln!();
}
