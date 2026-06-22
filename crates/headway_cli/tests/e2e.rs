//! End-to-end: drive the real `headway` binary against a real embedded relay,
//! exercising the full loop — CLI → relay → app nostrdb → relay → CLI.

use std::process::Command;
use std::time::Duration;

use nostrdb::{Config, Ndb};
use serde_json::Value;

/// Test signing key — the same all-`0x42` secret the relay's own roundtrip test
/// uses (a valid secp256k1 key).
const SECRET: [u8; 32] = [0x42; 32];

fn nsec() -> String {
    let hrp = bech32::Hrp::parse("nsec").expect("hrp");
    bech32::encode::<bech32::Bech32>(hrp, &SECRET).expect("encode nsec")
}

/// Run the `headway` binary with the shared connection args plus `extra`.
fn headway(url: &str, db: &str, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["--nsec", "<nsec>", "--relay", url, "--db", db];
    let nsec = nsec();
    args[1] = &nsec;
    args.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_headway"))
        .args(&args)
        .output()
        .expect("run headway")
}

/// The full hex id of the first card on the board, for addressing a `move`.
fn first_card_id(board: &Value) -> String {
    board["columns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["cards"].as_array().unwrap().iter())
        .next()
        .expect("a card")["id"]
        .as_str()
        .expect("card id")
        .to_string()
}

fn flushed(out: &std::process::Output) -> bool {
    String::from_utf8_lossy(&out.stderr).contains("flushed")
}

fn total_cards(board: &Value) -> usize {
    board["columns"]
        .as_array()
        .map(|cols| {
            cols.iter()
                .map(|c| c["cards"].as_array().map_or(0, Vec::len))
                .sum()
        })
        .unwrap_or(0)
}

/// Poll `show --json` until the board has `cards` cards (the relay ingests
/// asynchronously, so it may take a moment to fully materialise).
fn show_until(url: &str, db: &str, cards: usize) -> Value {
    for _ in 0..50 {
        let out = headway(url, db, &["show", "--json"]);
        if out.status.success()
            && let Ok(board) = serde_json::from_slice::<Value>(&out.stdout)
            && total_cards(&board) == cards
        {
            return board;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board never reached {cards} cards");
}

/// Poll `show --json` until the board has materialised with `cols` columns. The
/// default board seeds no cards, so column count (not card count) is what tells
/// us the seed has synced back.
fn show_until_cols(url: &str, db: &str, cols: usize) -> Value {
    for _ in 0..50 {
        let out = headway(url, db, &["show", "--json"]);
        if out.status.success()
            && let Ok(board) = serde_json::from_slice::<Value>(&out.stdout)
            && board["columns"].as_array().map_or(0, Vec::len) == cols
        {
            return board;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board never reached {cols} columns");
}

#[test]
fn seed_show_and_add_round_trip() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    // The "app" side: a relay serving its own nostrdb, like a running notedeck.
    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &Config::new().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay = nostrdb_relay::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    // The CLI keeps its own separate nostrdb cache.
    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seed the default board through the relay.
    let seed = headway(&url, db, &["seed"]);
    assert!(
        seed.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    // The seeded board comes back through a fresh sync: 5 columns, no cards.
    let board = show_until_cols(&url, db, 5);
    let cols = board["columns"].as_array().unwrap();
    assert_eq!(cols.len(), 5);
    assert_eq!(cols[0]["name"], "Backlog");
    assert_eq!(total_cards(&board), 0);

    // Add a card to Todo with labels; both the card and its labels must
    // round-trip back through the relay. `-l` is repeatable and comma-splittable.
    let add = headway(
        &url,
        db,
        &[
            "add",
            "Wire up the CLI",
            "--col",
            "Todo",
            "-l",
            "cli,ux",
            "--label",
            "p1",
        ],
    );
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let board = show_until(&url, db, 1);
    let todo = board["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Todo")
        .expect("todo column");
    let card = todo["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"] == "Wire up the CLI")
        .unwrap_or_else(|| panic!("added card not found in Todo: {board:#}"));
    let mut labels: Vec<&str> = card["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        vec!["cli", "p1", "ux"],
        "labels did not round-trip: {card:#}"
    );
}

/// Edits made while no relay is reachable land only in the CLI's cache; the next
/// connected run must flush them up so the app catches up.
#[test]
fn offline_edits_flush_on_reconnect() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &Config::new().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay = nostrdb_relay::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    // A port nothing listens on, so the CLI falls back to offline.
    let dead = "ws://127.0.0.1:1";

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seed offline: the board event lands in the CLI cache, none reach the relay.
    let seed = headway(dead, db, &["seed"]);
    assert!(seed.status.success(), "offline seed should still succeed");

    // Reconnect and run a plain `show`: the reconcile must push the stranded
    // seed up, so a fresh cache pointed at the relay sees the full board.
    let _ = headway(&url, db, &["show"]);

    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh = fresh_dir.path().to_str().unwrap();
    let board = show_until_cols(&url, fresh, 5);
    assert_eq!(board["columns"].as_array().unwrap().len(), 5);
}

/// Moving a card writes a new placement revision and supersedes the old one,
/// which lingers in the CLI's append-only cache after the relay has replaced it.
/// A settled board must not keep re-flushing that dropped revision every run —
/// the reconcile has to converge.
#[test]
fn reconcile_converges_after_replacing_a_placement() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &Config::new().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay = nostrdb_relay::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    assert!(headway(&url, db, &["seed"]).status.success(), "seed");
    show_until_cols(&url, db, 5);
    // The default board is card-less, so add a card to have something to move.
    assert!(
        headway(&url, db, &["add", "A card", "--col", "backlog"])
            .status
            .success(),
        "add"
    );
    let board = show_until(&url, db, 1);

    // Move a card: a fresh placement (same d-tag, newer created_at) replaces the
    // seeded one, so the relay drops the old id the cache still holds.
    let card = first_card_id(&board);
    let mv = headway(&url, db, &["move", &card, "--col", "done"]);
    assert!(
        mv.status.success(),
        "move failed: {}",
        String::from_utf8_lossy(&mv.stderr)
    );

    // Once the relay has ingested the new placement, `show` should stop finding
    // anything to flush. Allow a few runs for async ingest, then require it.
    let mut converged = false;
    for _ in 0..50 {
        if !flushed(&headway(&url, db, &["show"])) {
            converged = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(converged, "show kept re-flushing the superseded placement");

    // And it stays converged — the next run is silent too.
    assert!(
        !flushed(&headway(&url, db, &["show"])),
        "a settled board must not re-flush superseded events"
    );
}
