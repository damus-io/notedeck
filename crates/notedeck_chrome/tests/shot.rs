//! A shot end to end: boot the real app with no window, script a key, and
//! read the PNGs it writes.
//!
//! Needs a GPU adapter, so it is ignored by default and run on lavapipe by
//! `scripts/snapshot-test` (`-p=notedeck_chrome`).

use notedeck_chrome::shot::{self, ShotPlan};

/// An odd width, so the readback's row padding is exercised.
const SIZE: &str = "801x500";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a GPU adapter (lavapipe): scripts/snapshot-test"]
async fn a_shot_films_the_app_and_its_scripted_input() {
    let dir = tempfile::TempDir::new().expect("tmp dir");
    let before = dir.path().join("before.png");
    let after = dir.path().join("after.png");
    let args: Vec<String> = [
        "notedeck-test",
        "--testrunner",
        "--size",
        SIZE,
        "--frames",
        "3",
        "--shot",
        before.to_str().unwrap(),
        "--key",
        // The command bar's key (`notedeck_chrome::open_shortcut`).
        if cfg!(target_os = "macos") {
            "<D-k>"
        } else {
            "<C-Space>"
        },
        "--frames",
        "3",
        "--shot",
        after.to_str().unwrap(),
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let plan = ShotPlan::from_args(&args)
        .expect("a valid shot")
        .expect("--shot asks for one");
    assert_eq!(
        plan.args,
        vec!["notedeck-test".to_owned(), "--testrunner".to_owned()]
    );
    shot::run(dir.path().to_owned(), plan)
        .await
        .expect("the shot runs");

    let before = image::open(&before).expect("before.png").to_rgba8();
    let after = image::open(&after).expect("after.png").to_rgba8();
    assert_eq!(before.dimensions(), (801, 500));
    assert!(
        before.pixels().any(|p| p != before.get_pixel(0, 0)),
        "the frame is one flat colour: nothing was drawn"
    );
    assert!(
        before.pixels().all(|p| p[3] == u8::MAX),
        "a window's shot is opaque"
    );
    assert_ne!(
        before, after,
        "the bar's key opens the command bar, so the second shot differs"
    );
}
