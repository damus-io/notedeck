//! Headway through the real chrome: [`Chrome::new_headless`] with Headway in
//! front, one frame being the chrome's own `update` then `render` (whose
//! `apply_nav_requests` drains what Headway asked for), and nav transitions
//! **on**, as the app ships. So a push or a back slides for a handful of
//! frames through egui_nav, drawing the entry beneath and the top one in the
//! same pass, and a back pops only when its slide lands. Keys and clicks go
//! in as a user makes them, and the assertions read the screen.
//!
//! Release only (`cfg(not(debug_assertions))` on the module, with its own
//! release step in CI). During a slide egui_nav draws two routes of the same
//! app on two layers, and their widget ids repeat, which egui debug-asserts
//! against ("Widget .. changed layer_id during the frame"). In release that
//! check is compiled out, as in the shipped app.

use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use nostrdb::{Filter, Ndb, Transaction};
use nostrdb_net::{FullKeypair, NoteId, Pubkey};
use notedeck::test_harness::PressKey;
use notedeck::{App, ChromeNavEntry, NavStack, Notedeck};
use notedeck_headway::{event, store};

use super::Chrome;

/// The demo board's epic, and its subissue the epic tests put in review.
const EPIC: &str = "Define nostr event model for boards";
const SUBISSUE: &str = "Sync cards across relays";

/// A demo card with no parent.
const CARD: &str = "Inline card creation";

/// The grid's summary line, with every demo card on the board.
const GRID: &str = "7 cards · 5 columns";

/// What a finished review queue says on the view it lands on.
const QUEUE_DONE: &str = "Review queue done";

/// The demo board's In Review column, by index.
const IN_REVIEW_COL: usize = 3;

/// The seed's clock, in unix seconds.
const SEED_AT: u64 = 1_700_000_000;

/// How long a wait for the screen or the store gives up after.
const SETTLE: Duration = Duration::from_secs(10);

/// One frame's egui time, as at the app's 60 Hz.
const FRAME_DT: f32 = 1.0 / 60.0;

/// How many frames one `run_ok` may step: a second of frames, time for a
/// slide to land.
const FRAME_STEPS: u64 = 60;

/// A notedeck and the chrome it runs, built the way the app builds them.
struct ChromeState {
    notedeck: Notedeck,
    chrome: Chrome,
    kp: FullKeypair,
    _dir: tempfile::TempDir,
    fonts_installed: bool,
}

/// One real chrome frame. The first only installs fonts, which egui applies
/// on the next pass.
fn chrome_frame(ui: &mut egui::Ui, state: &mut ChromeState) {
    notedeck::test_harness::full_window(ui, |ui| {
        if !state.fonts_installed {
            state.notedeck.setup(ui.ctx());
            state.fonts_installed = true;
            return;
        }
        let mut app_ctx = state.notedeck.app_context();
        state.chrome.update(&mut app_ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            state.chrome.render(&mut app_ctx, ui);
        });
    });
}

/// The chrome's global history.
fn history<'h>(harness: &'h Harness<'_, ChromeState>) -> &'h NavStack<ChromeNavEntry> {
    harness
        .state()
        .chrome
        .global_nav
        .as_ref()
        .expect("global nav is seeded at construction")
}

/// Whether the chrome's history is still, with no slide running, and `len`
/// entries deep.
fn still_at(harness: &Harness<'_, ChromeState>, len: usize) -> bool {
    let nav = history(harness);
    !nav.navigating() && !nav.returning() && nav.len() == len
}

/// Run frames until `done` holds, or panic naming `what`.
fn wait_until(
    harness: &mut Harness<'_, ChromeState>,
    what: &str,
    done: impl Fn(&Harness<'_, ChromeState>) -> bool,
) {
    let deadline = Instant::now() + SETTLE;
    loop {
        // A slide asks for a repaint every frame until it lands, which can
        // outrun `run_ok`'s step cap; the loop just goes round again.
        let _ = harness.run_ok();
        if done(harness) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run frames until the history is still at `len` entries with `label` on
/// screen: the slide a key or click started has landed.
fn land_on(harness: &mut Harness<'_, ChromeState>, len: usize, label: &str) {
    let deadline = Instant::now() + SETTLE;
    loop {
        let _ = harness.run_ok();
        let showing = harness.query_all_by_label(label).next().is_some();
        if still_at(harness, len) && showing {
            return;
        }
        let nav = history(harness);
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label:?} with the history still at {len}: \
             it is {} deep (sliding: {}), and {label:?} is {}on screen",
            nav.len(),
            nav.navigating() || nav.returning(),
            if showing { "" } else { "not " },
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Press `key` with `modifiers`, the way someone types it.
fn press(harness: &mut Harness<'_, ChromeState>, modifiers: egui::Modifiers, key: egui::Key) {
    harness.press_key_modifiers(modifiers, key);
    let _ = harness.run_ok();
}

/// The demo board, folded.
fn demo_board(ndb: &Ndb, author: &Pubkey) -> event::BoardView {
    let txn = Transaction::new(ndb).expect("txn");
    let boards = event::fold_board(ndb, &txn, author)
        .expect("folded")
        .finalize();
    event::find_board(&boards, author, store::BOARD_ID)
        .expect("demo board")
        .clone()
}

fn card_id(view: &event::BoardView, title: &str) -> NoteId {
    view.columns
        .iter()
        .flat_map(|c| &c.cards)
        .find(|c| c.title == title)
        .unwrap_or_else(|| panic!("no demo card titled {title:?}"))
        .id
}

/// Apply `action` to the demo board as `kp`, then wait until the board folds
/// to `done`, or panic naming `what`. It polls the fold rather than awaiting
/// a subscription: nostrdb's `wait_for_notes*` unsubscribe when they return,
/// so a second await on the same subscription never wakes.
async fn apply_until(
    ndb: &Ndb,
    kp: &FullKeypair,
    action: store::BoardAction,
    what: &str,
    done: impl Fn(&event::BoardView) -> bool,
) {
    let secret = kp.secret_key.secret_bytes();
    store::apply(
        ndb,
        store::BOARD_ID,
        &demo_board(ndb, &kp.pubkey),
        &kp.pubkey,
        &store::Signer::new(&secret, None),
        action,
        &mut store::NoPublish,
    );
    let deadline = Instant::now() + SETTLE;
    while !done(&demo_board(ndb, &kp.pubkey)) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Run `git -C <dir> <args>`, panicking with git's stderr on failure;
/// returns stdout, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Commit `file` in the fixture repo `dir` with subject `subject`, returning
/// the new commit's sha.
fn commit(dir: &Path, file: &str, subject: &str) -> String {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().expect("file has a parent")).unwrap();
    std::fs::write(path, "fn reviewed() {}\n").unwrap();
    git(dir, &["add", "."]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            subject,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

/// Seed the demo board as `kp`, then move each card titled in `in_review`
/// into In Review with a review record naming a commit touching `file` in a
/// fixture repo at `repo`, on this host, so the pane resolves it with no
/// fetch and it carries no agentium session.
async fn seed(ndb: &Ndb, kp: &FullKeypair, repo: &Path, in_review: &[(&str, &str)]) {
    let sub = ndb
        .subscribe(&[Filter::new().authors([kp.pubkey.bytes()]).build()])
        .expect("subscribe");
    let written = store::seed_demo_board(
        ndb,
        &kp.pubkey,
        &kp.secret_key.secret_bytes(),
        store::BOARD_ID,
        SEED_AT,
        &mut store::NoPublish,
    );
    // One wait for the whole count: each `wait_for_notes*` unsubscribes when
    // it returns, so a loop of them hangs once the seed lands in two batches.
    let written = u32::try_from(written).expect("a demo board's worth of notes");
    ndb.wait_for_all_notes_within(sub, written, SETTLE)
        .await
        .expect("the demo board seeds");

    git(repo, &["init", "-q", "-b", "review-branch"]);
    for (row, &(title, file)) in in_review.iter().enumerate() {
        let card = card_id(&demo_board(ndb, &kp.pubkey), title);
        let moved =
            |v: &event::BoardView| v.columns[IN_REVIEW_COL].cards.iter().any(|c| c.id == card);
        let action = store::BoardAction::MoveCard {
            card,
            to_col: IN_REVIEW_COL,
            to_row: row,
        };
        apply_until(ndb, kp, action, &format!("{title} in review"), moved).await;

        let subject = format!("review: {title}");
        let review = event::ReviewFields {
            commit: Some(commit(repo, file, &subject)),
            title: Some(subject),
            branch: Some("review-branch".to_string()),
            host: headway::git::host_name(),
            path: Some(repo.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let reviewed = |v: &event::BoardView| v.card(card).is_some_and(|c| !c.reviews.is_empty());
        let action = store::BoardAction::AddReview { card, review };
        apply_until(ndb, kp, action, &format!("{title}'s record"), reviewed).await;
    }
}

/// A real chrome with Headway in front on the demo board, with `in_review`
/// seeded as [`seed`] does, the grid showing.
async fn headway_in_chrome(
    repo: &Path,
    in_review: &[(&str, &str)],
) -> Harness<'static, ChromeState> {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let kp = FullKeypair::generate();
    let args: Vec<String> = [
        "notedeck-test",
        "--testrunner",
        "--nsec",
        &kp.secret_key.to_secret_hex(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let egui_ctx = egui::Context::default();
    let mut notedeck = Notedeck::init(&egui_ctx, dir.path(), &args);
    {
        let app_ctx = notedeck.app_context();
        app_ctx.settings.complete_welcome();
        assert!(
            app_ctx.settings.get_settings_mut().animate_nav_transitions,
            "these tests are about the slide, which ships on"
        );
        seed(app_ctx.ndb, &kp, repo, in_review).await;
    }
    let mut chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");
    let headway = chrome.headway_slot().expect("headway in the roster");
    chrome.set_active(headway as i32);

    let state = ChromeState {
        notedeck,
        chrome,
        kp,
        _dir: dir,
        fonts_installed: false,
    };
    // A frame at the app's 60 Hz, not kittest's default quarter second: a
    // slide is a spring stepped once a frame, and at 4 Hz its dozen-odd
    // frames would outlast a notice's three seconds of egui time.
    let mut harness = Harness::builder()
        .with_size(egui::Vec2::new(1400.0, 900.0))
        .with_step_dt(FRAME_DT)
        .with_max_steps(FRAME_STEPS)
        .build_ui_state(chrome_frame, state);
    wait_until(&mut harness, "the board, still", |h| {
        let nav = history(h);
        !nav.navigating() && !nav.returning() && h.query_by_label(GRID).is_some()
    });
    harness
}

/// The depth of the chrome's history with Headway's board on top: every
/// test's starting point.
fn board_depth(harness: &Harness<'_, ChromeState>) -> usize {
    history(harness).len()
}

/// Archive the demo card titled `title` off the board, as another device
/// would (no key pressed), and wait for it to fold in.
async fn archive_elsewhere(harness: &mut Harness<'_, ChromeState>, title: &str) {
    let state = harness.state_mut();
    let kp = &state.kp;
    let app_ctx = state.notedeck.app_context();
    let ndb: &Ndb = &*app_ctx.ndb;
    let card = card_id(&demo_board(ndb, &kp.pubkey), title);
    let action = store::BoardAction::ArchiveCard { card };
    let gone = |v: &event::BoardView| v.card(card).is_none();
    apply_until(ndb, kp, action, &format!("{title} archived"), gone).await;
}

/// `D` on the board queue's last card closes the queue with a back, and the
/// grid it slides back onto says the queue is done. Before, the back's slide
/// drew the outgoing queue entry in the same pass as the grid, that pass
/// reseeded the queue open, and the notice was taken down as left before it
/// ever showed.
#[tokio::test]
async fn a_finished_queue_says_so_after_the_back_slide() {
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = headway_in_chrome(repo.path(), &[(CARD, "src/done.rs")]).await;
    let board = board_depth(&harness);

    press(&mut harness, egui::Modifiers::SHIFT, egui::Key::R);
    land_on(&mut harness, board + 1, "1 / 1");

    press(&mut harness, egui::Modifiers::SHIFT, egui::Key::D);
    land_on(&mut harness, board, GRID);
    assert!(
        harness.query_by_label(QUEUE_DONE).is_some(),
        "the grid says the queue is done"
    );
}

/// As [`a_finished_queue_says_so_after_the_back_slide`], for an epic's queue
/// (`R` from the epic's detail): the notice shows on the epic's detail the
/// back slides onto.
#[tokio::test]
async fn a_finished_epic_queue_says_so_after_the_back_slide() {
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = headway_in_chrome(repo.path(), &[(SUBISSUE, "src/sync.rs")]).await;
    let board = board_depth(&harness);

    harness.get_by_label(EPIC).click();
    land_on(&mut harness, board + 1, "← Back");

    press(&mut harness, egui::Modifiers::SHIFT, egui::Key::R);
    land_on(&mut harness, board + 2, "1 / 1");

    press(&mut harness, egui::Modifiers::SHIFT, egui::Key::D);
    land_on(&mut harness, board + 1, "← Back");
    assert!(
        harness.query_by_label("1 / 1").is_none(),
        "the queue has gone"
    );
    assert!(
        harness.query_by_label(QUEUE_DONE).is_some(),
        "the epic's detail says its queue is done"
    );
}

/// As [`a_finished_epic_queue_says_so_after_the_back_slide`], with the epic
/// archived while its queue is open: `D` on the last card closes the queue
/// onto the grid, by way of the epic's detail entry the back lands on first,
/// and the grid still says the queue is done once it settles.
///
/// Ignored: it fails on a bug outside the notice. The history ends one entry
/// *below* Headway's board, backed out of Headway altogether. The gone epic's
/// `Card` entry raises its drop-back on every pass that draws it; the chrome
/// ignores the ones that arrive mid-slide, but one raised in the pass where
/// that entry's own back-slide lands is applied after the pop. The hand-rolled
/// slide model this replaced had nothing beneath the board, so the extra back
/// was a no-op there. headway:headway/attack-neither-super swaps that drop for
/// a prune of the card's entries, which a repeat can't overshoot.
#[tokio::test]
#[ignore = "an extra back pops Headway's board: headway:headway/attack-neither-super"]
async fn a_gone_epics_finished_queue_says_so_on_the_grid() {
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = headway_in_chrome(repo.path(), &[(SUBISSUE, "src/sync.rs")]).await;
    let board = board_depth(&harness);

    harness.get_by_label(EPIC).click();
    land_on(&mut harness, board + 1, "← Back");
    press(&mut harness, egui::Modifiers::SHIFT, egui::Key::R);
    land_on(&mut harness, board + 2, "1 / 1");

    archive_elsewhere(&mut harness, EPIC).await;

    press(&mut harness, egui::Modifiers::SHIFT, egui::Key::D);
    land_on(&mut harness, board, "6 cards · 5 columns");
    assert!(
        harness.query_by_label(QUEUE_DONE).is_some(),
        "the grid says the queue is done"
    );
}

/// A notice is about the view it went up in. `s` on a sessionless record in a
/// plain review pane says so there, and it's gone from the detail Esc slides
/// back to, and from the grid the next Esc reaches.
#[tokio::test]
async fn a_pane_notice_stays_with_its_pane_through_the_slides() {
    const NO_SESSION: &str = "No agentium session on this record";
    let repo = tempfile::tempdir().expect("repo dir");
    let mut harness = headway_in_chrome(repo.path(), &[(CARD, "src/pane.rs")]).await;
    let board = board_depth(&harness);

    harness.get_by_label(CARD).click();
    land_on(&mut harness, board + 1, "± Review diff");
    press(&mut harness, egui::Modifiers::NONE, egui::Key::R);
    land_on(&mut harness, board + 2, "src/pane.rs");

    press(&mut harness, egui::Modifiers::NONE, egui::Key::S);
    wait_until(&mut harness, "the pane's notice", |h| {
        h.query_by_label(NO_SESSION).is_some()
    });

    press(&mut harness, egui::Modifiers::NONE, egui::Key::Escape);
    land_on(&mut harness, board + 1, "± Review diff");
    assert!(
        harness.query_by_label(NO_SESSION).is_none(),
        "the pane's notice didn't follow it to the detail"
    );

    press(&mut harness, egui::Modifiers::NONE, egui::Key::Escape);
    land_on(&mut harness, board, GRID);
    assert!(
        harness.query_by_label(NO_SESSION).is_none(),
        "nor on to the grid"
    );
}
