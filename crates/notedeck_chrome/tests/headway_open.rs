//! An open by reference through the real chrome: [`ChromeDriver::act`] raises
//! an [`AppAction::Open`] of a Headway card's `headway:` reference, and the
//! chrome's own routing resolves it through Headway's reference parser and
//! lands it, as `nd call open` or a click on the card's inline chip does.

#![cfg(feature = "headway")]

use std::time::Duration;

use nostrdb::Filter;
use nostrdb_net::FullKeypair;
use notedeck::{AppAction, Notedeck, OpenUri, ToolOutcome};
use notedeck_chrome::testing::ChromeDriver;
use notedeck_chrome::{Chrome, NotedeckApp};
use notedeck_headway::{store, Headway, HeadwayRoute};
use serde_json::{json, Value};

/// The grid's summary line once the demo board has folded.
const GRID: &str = "7 cards · 5 columns";

/// How many frames a wait gives up after.
const SETTLE_FRAMES: usize = 600;

/// A headless chrome running the real Headway alone, on the demo board seeded
/// as a fresh account, with nav settled so a push lands under a debug build.
/// Keeps the notedeck's data directory alive beside it.
async fn headway_driver() -> (tempfile::TempDir, ChromeDriver) {
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
        let ndb = &*app_ctx.ndb;
        let sub = ndb
            .subscribe(&[Filter::new().authors([kp.pubkey.bytes()]).build()])
            .expect("subscribe");
        let written = store::seed_demo_board(
            ndb,
            &kp.pubkey,
            &kp.secret_key.secret_bytes(),
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        );
        let written = u32::try_from(written).expect("a demo board's worth of notes");
        ndb.wait_for_all_notes_within(sub, written, Duration::from_secs(10))
            .await
            .expect("the demo board seeds");
    }

    let chrome = Chrome::new_headless_with_apps(
        &mut notedeck,
        [NotedeckApp::Headway(Box::new(Headway::new()))],
    );
    let mut driver = ChromeDriver::new(egui_ctx, notedeck, chrome).settle_nav();
    driver.frames_until("the demo board's grid drawn", SETTLE_FRAMES, |d| {
        d.drew(GRID)
    });
    (dir, driver)
}

/// The first card of the demo board, as Headway's `headway_show_board` data
/// tool lists it.
fn first_card(driver: &mut ChromeDriver) -> Value {
    let board = match driver
        .notedeck
        .app_context()
        .call_tool("headway_show_board", &json!({}))
    {
        ToolOutcome::Data(board) => board,
        _ => panic!("the demo board shows"),
    };
    board["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .flat_map(|column| column["cards"].as_array().expect("cards"))
        .next()
        .expect("the demo board has a card")
        .clone()
}

/// How deep the active app's history is.
fn depth(driver: &ChromeDriver) -> usize {
    driver.chrome.history().expect("an active app").len()
}

/// An `Open` of a card's `headway:` reference lands one history entry, the
/// card's detail, on top of the board, and holds nothing for a retry.
#[tokio::test]
async fn act_open_lands_a_headway_card_one_entry_deep() {
    let (_dir, mut driver) = headway_driver().await;
    let card = first_card(&mut driver);
    let reference = card["ref"].as_str().expect("the card's ref");
    let id = card["id"].as_str().expect("the card's id").to_owned();
    assert!(reference.starts_with("headway:"), "{reference}");
    let board = depth(&driver);

    driver.act(AppAction::Open(OpenUri::new(reference)));
    driver.frames_until("the open routed", SETTLE_FRAMES, |d| {
        d.chrome.pending_open().is_none() && depth(d) > board
    });
    driver.frame(Vec::new());

    assert_eq!(depth(&driver), board + 1, "one entry");
    let top = driver.chrome.history().expect("an active app").top();
    match top.downcast_ref::<HeadwayRoute>() {
        Some(HeadwayRoute::Card { id: on, .. }) => assert_eq!(on.hex(), id, "on the card"),
        _ => panic!("the top entry isn't a card's detail"),
    }
    assert!(driver.chrome.pending_open().is_none(), "nothing held");
}
