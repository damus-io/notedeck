//! End-to-end: drive the real `headway` binary against a real embedded relay,
//! exercising the full loop — CLI → relay → app nostrdb → relay → CLI.

use std::process::Command;
use std::time::Duration;

use nostrdb::{Config, Filter, Ndb, Transaction};
use serde_json::Value;

/// A [`Config`] with a small mapsize, for tests.
///
/// On Windows LMDB actually allocates the full mapsize on disk rather than only
/// mapping it virtually, so a test taking nostrdb's large default eats the CI
/// runner's disk. Mirrors `notedeck::test_util::test_config`, which this crate
/// can't reach (it doesn't depend on notedeck).
fn test_config() -> Config {
    if cfg!(target_os = "windows") {
        Config::new().set_mapsize(32 * 1024 * 1024) // 32 MiB
    } else {
        Config::new()
    }
}

/// Test signing key — the same all-`0x42` secret the relay's own roundtrip test
/// uses (a valid secp256k1 key).
const SECRET: [u8; 32] = [0x42; 32];

fn nsec() -> String {
    let hrp = bech32::Hrp::parse("nsec").expect("hrp");
    bech32::encode::<bech32::Bech32>(hrp, &SECRET).expect("encode nsec")
}

/// The account pubkey behind [`SECRET`] — the author whose board the relay's
/// store is inspected for below.
fn author() -> nostrdb_net::Pubkey {
    nostrdb_net::FullKeypair::from_secret_bytes(&SECRET)
        .expect("keypair")
        .pubkey
}

/// How many genuinely-plaintext headway board notes (board 30619 / issue 1621 /
/// placement 30620) authored by `author` the relay's store holds.
///
/// The relay is not a channel keyholder, so it never unwraps an SNS envelope —
/// every note of these kinds it holds is real plaintext. For a sealed board that
/// must be zero: the write-side leak guard keeps the board's locally-unwrapped
/// rumors off the plaintext reconcile, so only its kind-1081 envelopes reach the
/// relay.
fn plaintext_board_notes(ndb: &Ndb, author: &nostrdb_net::Pubkey) -> usize {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = Filter::new()
        .authors([author.bytes()])
        .kinds([30619u64, 1621, 30620])
        .build();
    ndb.query(&txn, &[filter], 500).map_or(0, |r| r.len())
}

/// How many kind-1081 SNS envelopes the relay's store holds — the sealed wire
/// form of a shared board's edits.
fn envelope_count(ndb: &Ndb) -> usize {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = Filter::new().kinds([1081u64]).build();
    ndb.query(&txn, &[filter], 500).map_or(0, |r| r.len())
}

/// Poll the relay's store until it holds at least one kind-1081 envelope, i.e.
/// the sealed board has synced up. Returns once satisfied, panics on timeout.
fn wait_for_envelope(ndb: &Ndb) {
    for _ in 0..50 {
        if envelope_count(ndb) > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("no kind-1081 envelope ever reached the relay");
}

/// Run the `headway` binary with the shared connection args plus `extra`.
fn headway(url: &str, db: &str, extra: &[&str]) -> std::process::Output {
    headway_as(&nsec(), url, db, extra)
}

/// [`headway`], signing as `key` (an nsec or a hex secret) instead of [`SECRET`].
fn headway_as(key: &str, url: &str, db: &str, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["--nsec", key, "--relay", url, "--db", db];
    args.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_headway"))
        .args(&args)
        // Pin the default board: without this the binary falls back to the
        // *developer's* persisted `headway board <id>` selection, and the test
        // seeds/reads whatever board they happened to leave current.
        .env("HEADWAY_BOARD", "headway")
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

/// Poll `--board <board> show --json` until that specific board has materialised
/// with `cols` columns (its seed has folded back). Panics on timeout.
fn show_board_until_cols(url: &str, db: &str, board: &str, cols: usize) -> Value {
    for _ in 0..50 {
        let out = headway(url, db, &["--board", board, "show", "--json"]);
        if out.status.success()
            && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
            && v["columns"].as_array().map_or(0, Vec::len) == cols
        {
            return v;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board '{board}' never reached {cols} columns");
}

/// `headway --board <slug> seed` titles the board by its slug (never a hardcoded
/// "Headway") and seals it from note #1 — the CLI half of jb55's recurring
/// "accidental Headway board". Verifies the folded title is the slug and that no
/// plaintext board event leaks (a born-sealed board rides up only as envelopes).
#[test]
fn non_default_seed_titles_by_slug_and_seals() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    let seed = headway(&url, db, &["--board", "work", "seed"]);
    assert!(
        seed.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    // The board folds back titled by its slug — not "Headway".
    let board = show_board_until_cols(&url, db, "work", 5);
    assert_eq!(
        board["title"], "work",
        "a non-default board must be titled by its slug, not 'Headway': {board:#}"
    );

    // Born sealed: its board definition reached the relay only as a kind-1081
    // envelope, never as a plaintext board event.
    wait_for_envelope(&relay_store);
    assert_eq!(
        plaintext_board_notes(&relay_store, &author()),
        0,
        "a born-sealed non-default board leaked plaintext board events"
    );
}

/// An explicit `--title` overrides the slug default when seeding.
#[test]
fn seed_title_flag_overrides_slug() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    let seed = headway(&url, db, &["--board", "work", "seed", "--title", "My Work"]);
    assert!(
        seed.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let board = show_board_until_cols(&url, db, "work", 5);
    assert_eq!(
        board["title"], "My Work",
        "--title should override the slug default: {board:#}"
    );
}

/// `seed -t <title>` seeds the board its title names, never the current one
/// (headway:headway/pepper-rack-usual). The repro: with `headway` already
/// seeded and current, `seed -t "Tune Assistant"` answered "board 'headway'
/// already exists". It must instead seed `tune-assistant` titled "Tune
/// Assistant", and a bare `seed` must refuse rather than guess.
#[test]
fn seed_with_only_a_title_seeds_the_titled_board_not_the_current_one() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // The current board (`HEADWAY_BOARD=headway`, see `headway_as`) exists.
    assert!(
        headway(&url, db, &["--board", "headway", "seed"])
            .status
            .success(),
        "seed the current board"
    );

    let seed = headway(&url, db, &["seed", "-t", "Tune Assistant"]);
    assert!(
        seed.status.success(),
        "seed -t must seed the titled board, not the current one: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&seed.stdout).contains("'tune-assistant'"),
        "seed should report the slug it derived: {}",
        String::from_utf8_lossy(&seed.stdout)
    );
    let board = show_board_until_cols(&url, db, "tune-assistant", 5);
    assert_eq!(board["title"], "Tune Assistant", "{board:#}");

    // Nothing to name the board by: refuse, and seed nothing.
    let bare = headway(&url, db, &["seed"]);
    assert!(!bare.status.success(), "a bare seed must refuse");
    assert!(
        String::from_utf8_lossy(&bare.stderr).contains("never uses the persisted current board"),
        "{}",
        String::from_utf8_lossy(&bare.stderr)
    );
}

#[test]
fn seed_show_and_add_round_trip() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    // The "app" side: a relay serving its own nostrdb, like a running notedeck.
    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    // The CLI keeps its own separate nostrdb cache.
    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seed the default board through the relay.
    let seed = headway(&url, db, &["--board", "headway", "seed"]);
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

/// A board seeded while no relay is reachable lands only in the CLI's cache; the
/// next connected run must flush it up so the app catches up. `seed` is now
/// born-sealed, so the stranded seed rides up as a kind-1081 envelope (not
/// plaintext). A *fresh keyholder cache* folding an offline-born board is a
/// separate capability — it needs the kind-1059 self-share to flush up too, which
/// is tracked as headway:headway/basic-owner-torch — so this test asserts the
/// supported half: the offline seed flushes its sealed board-def to the relay on
/// reconnect.
#[test]
fn offline_edits_flush_on_reconnect() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    // A port nothing listens on, so the CLI falls back to offline.
    let dead = "ws://127.0.0.1:1";

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seed offline: the sealed board-def lands in the CLI cache, none reach the relay.
    let seed = headway(dead, db, &["--board", "headway", "seed"]);
    assert!(seed.status.success(), "offline seed should still succeed");

    // Reconnect and run a plain `show`: the reconcile must push the stranded seed
    // up as its sealed envelope, and never as a plaintext board event.
    let _ = headway(&url, db, &["show"]);
    wait_for_envelope(&relay_store);
    assert_eq!(
        plaintext_board_notes(&relay_store, &author()),
        0,
        "an offline-born sealed board leaked plaintext board events on reconnect"
    );
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
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    assert!(
        headway(&url, db, &["--board", "headway", "seed"])
            .status
            .success(),
        "seed"
    );
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

/// Poll `--board <board> show --json` until that board has `cards` cards.
fn show_board_until(url: &str, db: &str, board: &str, cards: usize) -> Value {
    for _ in 0..50 {
        let out = headway(url, db, &["--board", board, "show", "--json"]);
        if out.status.success()
            && let Ok(b) = serde_json::from_slice::<Value>(&out.stdout)
            && b.is_object()
            && total_cards(&b) == cards
        {
            return b;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board {board} never reached {cards} cards");
}

/// Two boards under one identity stay independent: a card added to `work` doesn't
/// leak onto the default board, and `board` lists both.
#[test]
fn multiple_boards_are_independent() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seed the default board and a separate `work` board on the same key.
    assert!(
        headway(&url, db, &["--board", "headway", "seed"])
            .status
            .success(),
        "seed default"
    );
    assert!(
        headway(&url, db, &["--board", "work", "seed"])
            .status
            .success(),
        "seed work"
    );

    // Add a card only to `work`.
    let add = headway(
        &url,
        db,
        &["--board", "work", "add", "Ship it", "--col", "Todo"],
    );
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    // The card lands on `work`...
    let work = show_board_until(&url, db, "work", 1);
    assert_eq!(total_cards(&work), 1);

    // ...and the default board stays empty.
    let def = headway(&url, db, &["show", "--json"]);
    let def: Value = serde_json::from_slice(&def.stdout).expect("default board json");
    assert_eq!(total_cards(&def), 0, "card leaked onto default board");

    // `board` (no arg) lists both boards from the cache.
    let list = headway(&url, db, &["board"]);
    let out = String::from_utf8_lossy(&list.stdout);
    assert!(out.contains("work"), "board list missing 'work': {out}");
    assert!(
        out.contains("headway"),
        "board list missing 'headway': {out}"
    );
}

/// Seed the CLI's default board online. `seed` now creates a *born* team-of-one
/// SNS board — sealed from note #1 — so no separate `migrate` step is needed: the
/// board's edits travel as kind-1081 envelopes and its team key-share (kind-1059)
/// is on the relay from creation, so a fresh cache holding the account key can
/// join and read it. Panics if the seed fails.
fn seed_and_seal(url: &str, db: &str) {
    let seed = headway(url, db, &["--board", "headway", "seed"]);
    assert!(
        seed.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    show_until_cols(url, db, 5);
}

/// A board sealed with SNS must round-trip to a brand-new cache purely through
/// its kind-1081 envelopes: the fresh cache joins from the relay's key-share,
/// pulls the envelopes by channel pubkey, and folds the same board — a card and
/// all. This exercises the inbound half of the sealed-board sync leg.
#[test]
fn sealed_board_round_trips_to_fresh_cache() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    // A handle onto the relay's own store, to inspect what actually landed on it.
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    seed_and_seal(&url, db);
    // A sealed edit: the card is written only as an envelope, never plaintext.
    assert!(
        headway(&url, db, &["add", "Sealed card", "--col", "Todo"])
            .status
            .success(),
        "add to sealed board"
    );
    wait_for_envelope(&relay_store);

    // A fresh cache with the same key: it must join off the relay's key-share,
    // pull the envelopes, and fold the sealed board with its one card.
    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh = fresh_dir.path().to_str().unwrap();
    let board = show_until(&url, fresh, 1);
    assert_eq!(board["columns"].as_array().unwrap().len(), 5);
    let todo = board["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Todo")
        .expect("todo column");
    assert!(
        todo["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["title"] == "Sealed card"),
        "sealed card did not round-trip to a fresh cache: {board:#}"
    );
}

/// A sealed edit made while the relay is unreachable is stored locally as a
/// kind-1081 envelope; the next connected run must push that envelope up (the
/// plaintext leg can't, the edit isn't plaintext), so the app — and a fresh
/// cache — catch up. This exercises the outbound half of the envelope leg.
#[test]
fn offline_sealed_edit_flushes_on_reconnect() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    // A port nothing listens on, so the CLI falls back to offline.
    let dead = "ws://127.0.0.1:1";

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seal the board online so it is joinable, then make a sealed edit offline.
    seed_and_seal(&url, db);
    assert!(
        headway(dead, db, &["add", "Offline sealed card", "--col", "Todo"])
            .status
            .success(),
        "offline add to sealed board should still succeed"
    );

    // Reconnect: the sealed edit's envelope must flush up.
    let _ = headway(&url, db, &["show"]);

    // A fresh cache must see the offline edit, proving the envelope propagated.
    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh = fresh_dir.path().to_str().unwrap();
    let board = show_until(&url, fresh, 1);
    let todo = board["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Todo")
        .expect("todo column");
    assert!(
        todo["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["title"] == "Offline sealed card"),
        "offline sealed edit never propagated: {board:#}"
    );
}

/// The regression this card fixes: a sealed board must never flush its
/// locally-unwrapped rumors to the relay as plaintext, and its sync must
/// converge. A board born sealed offline (so its notes exist only as
/// locally-unwrapped rumors, never having reached the relay as plaintext) is the
/// exact scenario that leaked before — the promoted rumors still matched the
/// account-scoped plaintext filter, so every run re-pushed them and none ever
/// converged.
#[test]
fn sealed_board_converges_without_plaintext_leak() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    let dead = "ws://127.0.0.1:1";

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // Seed entirely offline: a born-sealed seed writes no plaintext board event, so
    // nothing of these kinds ever reaches the relay and the only way the board can
    // sync up is as sealed envelopes. (No `migrate` needed — seed is born-sealed.)
    assert!(
        headway(dead, db, &["--board", "headway", "seed"])
            .status
            .success(),
        "offline seed"
    );

    // Reconnect: the envelope leg flushes the sealed board up. The plaintext leg
    // must push nothing — the board's notes are now rumors, excluded from it.
    wait_for_envelope_via_show(&url, db, &relay_store);

    // No plaintext board event may have landed on the relay — only envelopes.
    assert_eq!(
        plaintext_board_notes(&relay_store, &author()),
        0,
        "a sealed board leaked plaintext board events to the relay"
    );

    // And the sync converges: once the envelope is up, a `show` finds nothing
    // left to flush, and stays that way.
    let mut converged = false;
    for _ in 0..50 {
        if !flushed(&headway(&url, db, &["show"])) {
            converged = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(converged, "sealed-board sync kept re-flushing");
    assert!(
        !flushed(&headway(&url, db, &["show"])),
        "a settled sealed board must not re-flush"
    );
    assert_eq!(
        plaintext_board_notes(&relay_store, &author()),
        0,
        "a settled sealed board leaked plaintext on a later run"
    );
}

/// Run `show` against the relay until the sealed board's envelope has flushed up,
/// driving the reconnect that pushes it. Panics on timeout.
fn wait_for_envelope_via_show(url: &str, db: &str, relay_store: &Ndb) {
    for _ in 0..50 {
        let _ = headway(url, db, &["show"]);
        if envelope_count(relay_store) > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("sealed board never flushed its envelope to the relay");
}

/// An offline-born sealed board becomes joinable from a **fresh cache** after the
/// owner reconnects — the push half of the giftwrap leg
/// (headway:headway/basic-owner-torch).
///
/// [`sealed_board_converges_without_plaintext_leak`] proves the *content* flushes
/// up on reconnect; this proves the *key* does too. Seed a sealed board while
/// offline, so its self-share kind-1059 and its kind-1081 content land only in the
/// owner's local cache. Reconnect the owner: `sync_envelopes` flushes the content
/// and `flush_own_selfshares` flushes the self-share. A fresh cache with the same
/// key then pulls the self-share, registers the root, pulls the content, and folds
/// the board. Without the self-share flush the fresh cache pulls the envelopes but
/// has no key to join — the board never folds — which is the exact gap
/// box-shock-disorder could not cover.
#[test]
fn offline_born_board_joins_from_a_fresh_cache_after_reconnect() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    let dead = "ws://127.0.0.1:1";

    // Owner's cache: seed a non-default sealed board entirely offline, so its
    // self-share and content stay local (the seed's publish is a no-op offline).
    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner = owner_dir.path().to_str().unwrap();
    assert!(
        headway(dead, owner, &["--board", "work", "seed"])
            .status
            .success(),
        "offline seed"
    );

    // Reconnect the owner: this flushes the self-share up (the leg under test).
    // The marker then makes it a one-shot, so a later run won't re-flush.
    let reconnect = headway(&url, owner, &["--board", "work", "show", "--json"]);
    assert!(
        reconnect.status.success(),
        "reconnect: {}",
        String::from_utf8_lossy(&reconnect.stderr)
    );
    assert!(
        String::from_utf8_lossy(&reconnect.stderr).contains("own self-share"),
        "reconnect should flush the offline-born board's self-share up:\n{}",
        String::from_utf8_lossy(&reconnect.stderr)
    );

    // A FRESH cache with the same key joins purely from the relay: it pulls the
    // self-share, registers the root, pulls the content, and folds the board.
    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh = fresh_dir.path().to_str().unwrap();
    let board = show_board_until_cols(&url, fresh, "work", 5);
    assert_eq!(
        board["title"], "work",
        "a fresh cache must fold the offline-born board after the owner reconnects: {board:#}"
    );

    // One-shot: a second owner run finds the self-share already flushed and says
    // nothing more about it.
    let again = headway(&url, owner, &["--board", "work", "show"]);
    assert!(
        !String::from_utf8_lossy(&again.stderr).contains("own self-share"),
        "a settled self-share must not re-flush:\n{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

/// A board whose kind-1059 self-share never reached the relay is still joinable
/// from a fresh cache, because its channel root is *derivable* from its slug
/// (headway:headway/rocket-group-ginger).
///
/// [`offline_born_board_joins_from_a_fresh_cache_after_reconnect`] covers the
/// happy path where the self-share does flush. This covers the one that kept
/// biting in production: the self-share was published somewhere the other device
/// cannot read (an embedded relay), and the once-per-cache marker then records the
/// root as flushed so it is never retried. The board's *content* is on the relay,
/// the *key* is not, and every other device folds nothing.
///
/// Reproduced by pre-seeding the owner's `flushed_selfshares` marker with the
/// board's derived root before the owner ever reconnects, which is exactly the
/// state `damus-website` was found in. The reconnect then flushes the 1081
/// content and deliberately not the 1059. A fresh cache therefore has no
/// key-share to join from and must recover by deriving
/// `derive_board_root(secret, slug)` itself.
#[test]
fn a_board_whose_selfshare_never_flushed_is_joinable_by_deriving_its_root() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    let dead = "ws://127.0.0.1:1";

    // Seed offline so the self-share lands only in the owner's cache.
    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner = owner_dir.path().to_str().unwrap();
    assert!(
        headway(dead, owner, &["--board", "stranded", "seed"])
            .status
            .success(),
        "offline seed"
    );

    // Pre-record the root as already-flushed, so the reconnect below pushes the
    // board's content up but never its self-share — the production state.
    // Both carriers' markers: the gift-wrap's, and the PNS copy's
    // (headway:headway/pepper-rack-usual), so no key-share goes up at all.
    let root = nostrdb_net::sns::derive_board_root(&SECRET, "stranded");
    for marker in ["flushed_selfshares", "flushed_pns_selfshares"] {
        std::fs::write(
            owner_dir.path().join(marker),
            format!("{}\n", hex::encode(root)),
        )
        .expect("write marker");
    }

    let reconnect = headway(&url, owner, &["--board", "stranded", "show"]);
    assert!(
        reconnect.status.success(),
        "reconnect: {}",
        String::from_utf8_lossy(&reconnect.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&reconnect.stderr).contains("own self-share"),
        "the marker must suppress the self-share flush, or this test proves nothing:\n{}",
        String::from_utf8_lossy(&reconnect.stderr)
    );

    // The fresh cache has no key-share for this board and must derive its way in.
    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh = fresh_dir.path().to_str().unwrap();
    let board = show_board_until_cols(&url, fresh, "stranded", 5);
    assert_eq!(
        board["title"], "stranded",
        "a fresh cache must fold a board whose self-share never flushed: {board:#}"
    );

    // A slug that names no board must not be recovered into existence — the
    // derivation is a lookup, not a create, so it stays a plain failure.
    let missing = headway(&url, fresh, &["--board", "nosuchboard", "show"]);
    let (out, err) = (
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr),
    );
    assert!(
        out.contains("no board 'nosuchboard'"),
        "an unknown slug must still report no board, not be derived into one:\n{out}{err}"
    );
    assert!(
        !err.contains("by deriving"),
        "an unknown slug must not mint a key-share for a phantom channel:\n{err}"
    );
}

/// Spawn an embedded relay over a fresh nostrdb, standing in for one machine's
/// running notedeck. Returns the relay handle (keep it alive), its URL, and its
/// store for inspection. The temp dir must outlive the store.
fn spawn_machine(
    dir: &tempfile::TempDir,
) -> (nostrdb_net::relay::server::RelayHandle, String, Ndb) {
    let ndb = Ndb::new(
        dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("machine ndb");
    let store = ndb.clone();
    let relay =
        nostrdb_net::relay::server::spawn(ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();
    (relay, url, store)
}

/// Carry what a private relay that auth-gates gift-wraps carries between two
/// machines: every kind-1080 PNS and kind-1081 SNS note in `from`, and no
/// kind-1059. relay.jb55.com refuses unauthenticated 1059 reads, and notedeck
/// does not answer NIP-42, so that is all that crosses
/// (headway:headway/pepper-rack-usual). Returns once `to` holds every one.
fn carry_private_notes(from: &Ndb, to: &Ndb) {
    let filter = || Filter::new().kinds([1080u64, 1081]).build();
    let jsons: Vec<String> = {
        let txn = Transaction::new(from).expect("txn");
        from.query(&txn, &[filter()], 10_000)
            .expect("query")
            .iter()
            .map(|r| r.note.json().expect("note json"))
            .collect()
    };
    assert!(!jsons.is_empty(), "nothing to carry");
    for json in &jsons {
        to.process_event(&format!("[\"EVENT\",\"_carry\",{json}]"))
            .expect("carry note");
    }
    for _ in 0..50 {
        let txn = Transaction::new(to).expect("txn");
        if to.query(&txn, &[filter()], 10_000).map_or(0, |r| r.len()) >= jsons.len() {
            return;
        }
        drop(txn);
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the carried notes never landed");
}

/// Poll `headway board` until its listing names `slug`, without ever naming the
/// board on the command line, so the board must come from the roster rather than
/// from deriving its root. Panics on timeout.
fn lists_board_until(url: &str, db: &str, slug: &str) {
    let mut last = String::new();
    for _ in 0..50 {
        let out = headway(url, db, &["board"]);
        last = String::from_utf8_lossy(&out.stdout).into_owned();
        if last.split_whitespace().any(|word| word == slug) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("`headway board` never listed '{slug}':\n{last}");
}

/// A board seeded on one machine shows up on another that never receives its
/// kind-1059 self-share (headway:headway/pepper-rack-usual).
///
/// Each machine is its own embedded relay, as each runs its own notedeck. The
/// private relay between them carries only PNS and SNS notes, because it serves
/// gift-wraps only to an authenticated reader. Before the fix the board's content
/// crossed but the key to read it didn't, so the second machine's `headway board`
/// never listed it. The seed's PNS-carried self-share is the key that does cross.
#[test]
fn a_board_seeded_on_one_machine_appears_on_another_without_its_gift_wrap() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let (hydra_dir, monad_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_hydra_relay, hydra_url, hydra_store) = spawn_machine(&hydra_dir);
    let (_monad_relay, monad_url, monad_store) = spawn_machine(&monad_dir);

    // Seed and add a card on the first machine.
    let hydra_cli = tempfile::tempdir().expect("hydra cli");
    let hydra = hydra_cli.path().to_str().unwrap();
    let seed = headway(&hydra_url, hydra, &["--board", "tune-assistant", "seed"]);
    assert!(
        seed.status.success(),
        "seed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let add = headway(
        &hydra_url,
        hydra,
        &["--board", "tune-assistant", "add", "Port it"],
    );
    assert!(
        add.status.success(),
        "add: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    wait_for_envelope(&hydra_store);

    carry_private_notes(&hydra_store, &monad_store);
    assert_eq!(
        giftwraps_to(&monad_store, &author()),
        0,
        "the gift-wrap must not cross, or this test proves nothing"
    );

    // The second machine lists the board without being told its slug, and folds it.
    let monad_cli = tempfile::tempdir().expect("monad cli");
    let monad = monad_cli.path().to_str().unwrap();
    lists_board_until(&monad_url, monad, "tune-assistant");
    let board = show_board_until(&monad_url, monad, "tune-assistant", 1);
    assert_eq!(board["title"], "tune-assistant", "{board:#}");
}

/// A board whose cache flushed its kind-1059 self-share before the PNS copy
/// existed still sends the copy, once (headway:headway/pepper-rack-usual).
///
/// This is the state of the boards already seeded on hydra: the gift-wrap marker
/// records their roots, so the gift-wrap flush never runs again. The PNS copy has
/// its own marker, so the next run sends it, and a fresh cache that can't read any
/// 1059 lists the board from that copy alone. A later run doesn't resend it.
#[test]
fn an_already_flushed_board_sends_its_pns_self_share_once() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let relay_dir = tempfile::tempdir().unwrap();
    let (_relay, url, store) = spawn_machine(&relay_dir);
    let dead = "ws://127.0.0.1:1";

    // Seed offline so no gift-wrap ever reaches the relay, then mark the root as
    // gift-wrap-flushed: the pre-fix cache state.
    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner = owner_dir.path().to_str().unwrap();
    assert!(
        headway(dead, owner, &["--board", "oot", "seed"])
            .status
            .success(),
        "offline seed"
    );
    let root = nostrdb_net::sns::derive_board_root(&SECRET, "oot");
    std::fs::write(
        owner_dir.path().join("flushed_selfshares"),
        format!("{}\n", hex::encode(root)),
    )
    .expect("write marker");

    let reconnect = headway(&url, owner, &["--board", "oot", "show"]);
    assert!(
        String::from_utf8_lossy(&reconnect.stderr).contains("flushed 1 own self-share"),
        "the reconnect must flush the PNS copy and only that:\n{}",
        String::from_utf8_lossy(&reconnect.stderr)
    );
    assert_eq!(giftwraps_to(&store, &author()), 0, "no gift-wrap went up");

    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    lists_board_until(&url, fresh_dir.path().to_str().unwrap(), "oot");

    let again = headway(&url, owner, &["--board", "oot", "show"]);
    assert!(
        !String::from_utf8_lossy(&again.stderr).contains("own self-share"),
        "a flushed PNS copy must not be resent:\n{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

/// A second identity's secret, for the member side of a shared board. Any
/// in-range scalar is a valid secp256k1 key; this one differs from [`SECRET`].
const MEMBER_SECRET: [u8; 32] = [0x43; 32];

/// Collects the `["EVENT", …]` frames a [`headway::store`] write produces, so a
/// test can publish them to the relay itself.
#[derive(Default)]
struct Frames(Vec<String>);

impl headway::store::Publisher for Frames {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_string());
    }
}

/// How many kind-1059 gift-wraps addressed (`#p`) to `recipient` the relay's
/// store holds. A wrap's author is a throwaway key, so the recipient tag is the
/// only handle on "who was this for".
fn giftwraps_to(ndb: &Ndb, recipient: &nostrdb_net::Pubkey) -> usize {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = Filter::new()
        .kinds([1059u64])
        .pubkeys([recipient.bytes()])
        .build();
    ndb.query(&txn, &[filter], 500).map_or(0, |r| r.len())
}

/// How many plaintext board notes (board, issue, placement, comment) `author`
/// signed that the relay's store holds. For a member of a sealed board it must
/// be zero: every edit rides up as a kind-1081 envelope signed by the team key.
fn plaintext_member_notes(ndb: &Ndb, author: &nostrdb_net::Pubkey) -> usize {
    let txn = Transaction::new(ndb).expect("txn");
    let filter = Filter::new()
        .authors([author.bytes()])
        .kinds([30619u64, 1621, 30620, 1111])
        .build();
    ndb.query(&txn, &[filter], 500).map_or(0, |r| r.len())
}

/// The card titled `title` on a folded board, with the name of its column.
fn find_titled<'a>(board: &'a Value, title: &str) -> Option<(&'a Value, &'a str)> {
    board["columns"].as_array()?.iter().find_map(|col| {
        let card = col["cards"]
            .as_array()?
            .iter()
            .find(|c| c["title"] == title)?;
        Some((card, col["name"].as_str()?))
    })
}

/// Poll `show --json` until `until` holds for the board, signing as `key` and
/// reading `owner`'s board `slug`. Panics with the last fold on timeout.
fn show_as_until(
    key: &str,
    url: &str,
    db: &str,
    owner: &str,
    slug: &str,
    until: impl Fn(&Value) -> bool,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..50 {
        let out = headway_as(
            key,
            url,
            db,
            &["--author", owner, "--board", slug, "show", "--json"],
        );
        if out.status.success()
            && let Ok(board) = serde_json::from_slice::<Value>(&out.stdout)
        {
            if board.is_object() && until(&board) {
                return board;
            }
            last = board;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("board '{slug}' never reached the expected state; last fold: {last:#}");
}

/// A member folds and edits a sealed board someone else owns, from the CLI.
///
/// The owner seals a board and shares its root with the member as a kind-1082
/// key-share. The member, on a fresh cache with `--author <owner>`, must pull
/// its *own* gift-wraps, build its roster from key-shares addressed to *it*, and
/// fold the owner's coordinate — and its edits must fold back on the owner's
/// side attributed to the member, without leaking plaintext or re-wrapping the
/// owner's root back to the owner (headway:headway/vacuum-priority-ordinary).
#[test]
fn member_folds_and_edits_a_board_shared_by_its_owner() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let owner = author();
    let owner_hex = owner.hex();
    let member = nostrdb_net::FullKeypair::from_secret_bytes(&MEMBER_SECRET)
        .expect("member keypair")
        .pubkey;
    let member_key = hex::encode(MEMBER_SECRET);
    let slug = "shared";

    // 1. The owner seals a board and puts a card on it.
    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner_db = owner_dir.path().to_str().unwrap();
    assert!(
        headway(&url, owner_db, &["--board", slug, "seed"])
            .status
            .success(),
        "owner seed"
    );
    show_board_until_cols(&url, owner_db, slug, 5);
    assert!(
        headway(
            &url,
            owner_db,
            &["--board", slug, "add", "Owner card", "--col", "Todo"]
        )
        .status
        .success(),
        "owner add"
    );
    show_board_until(&url, owner_db, slug, 1);

    // 2. The owner shares the board with the member: `headway share` gift-wraps
    // the board's root to the member's pubkey as a kind-1082 key-share.
    let member_npub = member.npub().expect("member npub");
    let out = headway(
        &url,
        owner_db,
        &["--board", slug, "--json", "share", &member_npub],
    );
    assert!(
        out.status.success(),
        "owner share: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shared: Value = serde_json::from_slice(&out.stdout).expect("share --json");
    assert_eq!(shared["ok"], true);
    assert_eq!(shared["board"], slug);
    assert_eq!(shared["recipient"], member.hex());
    let root = nostrdb_net::sns::derive_board_root(&SECRET, slug);
    let team_pk = nostrdb_net::sns::derive_sns_keys(&root)
        .expect("team keys")
        .team_keypair
        .pubkey
        .hex();
    assert_eq!(
        shared["team_pubkey"], team_pk,
        "the share must hand out the board's own channel"
    );
    // The relay ingests asynchronously, so give the wrap a moment to land.
    let mut wraps = 0;
    for _ in 0..50 {
        wraps = giftwraps_to(&relay_store, &member);
        if wraps > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        wraps, 1,
        "exactly one key-share reached the relay for the member"
    );

    // 3. The member, on a fresh cache, folds the owner's board.
    let member_dir = tempfile::tempdir().expect("member dir");
    let member_db = member_dir.path().to_str().unwrap();
    let board = show_as_until(&member_key, &url, member_db, &owner_hex, slug, |b| {
        find_titled(b, "Owner card").is_some()
    });
    let owner_card = find_titled(&board, "Owner card").unwrap().0["id"]
        .as_str()
        .expect("owner card id")
        .to_string();

    // 4. The member adds a card, moves the owner's, and comments on it.
    let wraps_to_owner = giftwraps_to(&relay_store, &owner);
    let member_edit = |extra: &[&str]| {
        let mut args = vec!["--author", owner_hex.as_str(), "--board", slug];
        args.extend_from_slice(extra);
        let out = headway_as(&member_key, &url, member_db, &args);
        assert!(
            out.status.success(),
            "member {extra:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !flushed(&out),
            "a member run must not flush self-shares (it would re-wrap the owner's root):\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    member_edit(&["add", "Member card", "--col", "Todo"]);
    member_edit(&["move", &owner_card, "--col", "In Progress"]);
    member_edit(&["comment", &owner_card, "hello from a member"]);

    // 5. The owner folds all three, attributed to the member.
    let member_hex = member.hex();
    let mut settled = None;
    for _ in 0..50 {
        let out = headway(&url, owner_db, &["--board", slug, "show", "--json"]);
        if let Ok(b) = serde_json::from_slice::<Value>(&out.stdout)
            && let Some((card, col)) = find_titled(&b, "Owner card")
            && col == "In Progress"
            && card["comments"].as_array().is_some_and(|c| !c.is_empty())
            && find_titled(&b, "Member card").is_some()
        {
            settled = Some(b);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let board = settled.expect("the owner never folded all three of the member's edits");
    let (member_card, _) = find_titled(&board, "Member card").unwrap();
    assert_eq!(
        member_card["author"], member_hex,
        "the member's card must be attributed to the member"
    );
    let (owner_card_view, _) = find_titled(&board, "Owner card").unwrap();
    let comment = &owner_card_view["comments"][0];
    assert_eq!(comment["body"], "hello from a member");
    assert_eq!(
        comment["author"], member_hex,
        "the member's comment must be attributed to the member"
    );

    // 6. Nothing the member wrote reached the relay in the clear, and none of its
    // runs gift-wrapped anything to the owner.
    assert_eq!(
        plaintext_member_notes(&relay_store, &member),
        0,
        "a member's edits to a sealed board must travel only as envelopes"
    );
    assert_eq!(
        giftwraps_to(&relay_store, &owner),
        wraps_to_owner,
        "a member run must not re-wrap the owner's root to the owner"
    );
}

/// `headway share` refuses every case where handing out the key would be wrong
/// or silently lost: a member re-sharing the owner's board, sharing to yourself,
/// a board that isn't named explicitly, a plaintext board (no channel to hand
/// out), a board that doesn't exist, and an unreachable relay (a key-share is
/// never re-sent). None of them may put a key-share on the relay.
#[test]
fn share_refuses_what_it_must_not_share() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let owner = author();
    let owner_hex = owner.hex();
    let member = nostrdb_net::FullKeypair::from_secret_bytes(&MEMBER_SECRET)
        .expect("member keypair")
        .pubkey;
    let member_hex = member.hex();
    let member_key = hex::encode(MEMBER_SECRET);
    let slug = "shared";

    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner_db = owner_dir.path().to_str().unwrap();
    assert!(
        headway(&url, owner_db, &["--board", slug, "seed"])
            .status
            .success(),
        "owner seed"
    );
    show_board_until_cols(&url, owner_db, slug, 5);

    // A plaintext board of the owner's, published straight to the relay the way
    // a pre-SNS client wrote one: no channel, so nothing to share.
    let plain_dir = tempfile::tempdir().expect("plain dir");
    let plain_ndb =
        Ndb::new(plain_dir.path().to_str().unwrap(), &test_config()).expect("plain ndb");
    let mut frames = Frames::default();
    headway::store::seed_board(&plain_ndb, &owner, &SECRET, "plain", "Plain", &mut frames);
    rt.block_on(async {
        let mut relay = nostrdb_net::relay::sync::Relay::connect(&url)
            .await
            .expect("connect");
        relay
            .publish(&frames.0)
            .await
            .expect("publish plaintext board");
    });
    show_board_until_cols(&url, owner_db, "plain", 5);

    let refused = |key: &str, relay_url: &str, db: &str, args: &[&str], why: &str| {
        let out = headway_as(key, relay_url, db, args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} should have been refused");
        assert!(err.contains(why), "{args:?}: expected '{why}' in:\n{err}");
    };

    // A member re-sharing the owner's board — even one it holds the key to.
    let member_dir = tempfile::tempdir().expect("member dir");
    let member_db = member_dir.path().to_str().unwrap();
    refused(
        &member_key,
        &url,
        member_db,
        &["--author", &owner_hex, "--board", slug, "share", &owner_hex],
        "only the board owner can share it",
    );
    let owner_key = nsec();
    refused(
        &owner_key,
        &url,
        owner_db,
        &["--board", slug, "share", &owner_hex],
        "your own key",
    );
    // No --board: the persisted/env current board is never shared.
    refused(
        &owner_key,
        &url,
        owner_db,
        &["share", &member_hex],
        "pass --board",
    );
    refused(
        &owner_key,
        &url,
        owner_db,
        &["--board", "plain", "share", &member_hex],
        "is not sealed",
    );
    refused(
        &owner_key,
        &url,
        owner_db,
        &["--board", "nope", "share", &member_hex],
        "no board 'nope'",
    );
    // Port 1 on loopback refuses the connection, so the run works offline.
    refused(
        &owner_key,
        "ws://127.0.0.1:1",
        owner_db,
        &["--board", slug, "share", &member_hex],
        "share needs a live relay",
    );

    assert_eq!(
        giftwraps_to(&relay_store, &member),
        0,
        "a refused share must not put a key-share on the relay"
    );
}

/// A third identity's secret: a second owner who shares a board under the same
/// slug as [`SECRET`]'s, to make the bare slug ambiguous for the member.
const OTHER_OWNER_SECRET: [u8; 32] = [0x44; 32];

/// Seal a board `slug` as `owner_key`, put a card titled `card` on it, and share
/// it with `member` — waiting until that key-share has reached the relay, so a
/// member run right after it can join.
fn seed_and_share(
    relay_store: &Ndb,
    url: &str,
    owner_key: &str,
    owner_db: &str,
    slug: &str,
    card: &str,
    member: &nostrdb_net::Pubkey,
) {
    let run = |args: &[&str]| {
        let out = headway_as(owner_key, url, owner_db, args);
        assert!(
            out.status.success(),
            "owner {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let wraps = giftwraps_to(relay_store, member);
    run(&["--board", slug, "seed"]);
    run(&["--board", slug, "add", card, "--col", "Todo"]);
    run(&["--board", slug, "share", &member.hex()]);
    for _ in 0..50 {
        if giftwraps_to(relay_store, member) > wraps {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the key-share for '{slug}' never reached the relay");
}

/// A member finds a board shared with it by its slug alone, with no `--author`.
///
/// Card refs name a board's slug but not its owner, so without this a member had
/// to carry the owner's hex on every call, and `headway board` listed nothing
/// (headway:headway/place-wheel-web). After an owner shares a board, the member's
/// `headway board` lists it under "shared with me" with its owner, and a bare
/// `--board <slug>` (or a self-routing card ref) folds the owner's board. A
/// second owner sharing a same-slug board makes the bare slug an error that names
/// both owners, an explicit `--author` still picks either, and a board of the
/// member's own by that slug wins over both.
#[test]
fn member_resolves_a_shared_board_by_its_slug() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let owner = author();
    let other = nostrdb_net::FullKeypair::from_secret_bytes(&OTHER_OWNER_SECRET)
        .expect("other owner keypair")
        .pubkey;
    let member = nostrdb_net::FullKeypair::from_secret_bytes(&MEMBER_SECRET)
        .expect("member keypair")
        .pubkey;
    let member_key = hex::encode(MEMBER_SECRET);
    let slug = "shared";

    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner_db = owner_dir.path().to_str().unwrap();
    seed_and_share(
        &relay_store,
        &url,
        &nsec(),
        owner_db,
        slug,
        "Owner card",
        &member,
    );

    let member_dir = tempfile::tempdir().expect("member dir");
    let member_db = member_dir.path().to_str().unwrap();
    let member_run = |args: &[&str]| headway_as(&member_key, &url, member_db, args);
    let short = |pk: &nostrdb_net::Pubkey| pk.npub().unwrap()[..14].to_string();

    // 1. `headway board` lists the owner's board under "shared with me".
    let mut listing = String::new();
    for _ in 0..50 {
        let out = member_run(&["board"]);
        listing = String::from_utf8_lossy(&out.stdout).into_owned();
        if listing.contains("1 cards") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let shared_section = listing
        .split("shared with me\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no shared-with-me section in:\n{listing}"));
    assert!(
        shared_section.contains(slug)
            && shared_section.contains("1 cards")
            && shared_section.contains(&short(&owner)),
        "the shared board, its card and its owner must be listed:\n{listing}"
    );

    // 2. A bare `--board <slug>` folds the owner's board, no `--author`.
    let out = member_run(&["--board", slug, "show", "--json"]);
    assert!(
        out.status.success(),
        "member show: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let board: Value = serde_json::from_slice(&out.stdout).expect("show --json");
    let (card, _) = find_titled(&board, "Owner card")
        .unwrap_or_else(|| panic!("the owner's card must fold for the member: {board:#}"));
    let card_ref = card["ref"].as_str().expect("card ref").to_string();

    // ...and so does a self-routing card ref, which names only the slug. Editing
    // through it lands on the owner's board.
    let out = member_run(&["comment", &card_ref, "found it by slug"]);
    assert!(
        out.status.success(),
        "member comment via {card_ref}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    show_as_until(&member_key, &url, member_db, &owner.hex(), slug, |b| {
        find_titled(b, "Owner card")
            .is_some_and(|(c, _)| c["comments"][0]["body"] == "found it by slug")
    });

    // 3. A second owner shares a board under the same slug: the bare slug is now
    // ambiguous, and the error names both owners in full.
    let other_dir = tempfile::tempdir().expect("other owner dir");
    let other_db = other_dir.path().to_str().unwrap();
    seed_and_share(
        &relay_store,
        &url,
        &hex::encode(OTHER_OWNER_SECRET),
        other_db,
        slug,
        "Other card",
        &member,
    );
    let mut err = String::new();
    for _ in 0..50 {
        let out = member_run(&["--board", slug, "show"]);
        err = String::from_utf8_lossy(&out.stderr).into_owned();
        if !out.status.success() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        err.contains("--author")
            && err.contains(&owner.npub().unwrap())
            && err.contains(&other.npub().unwrap()),
        "an ambiguous slug must name both owners and point at --author:\n{err}"
    );

    // The listing, which resolves no single slug, still works and shows both.
    let out = member_run(&["board"]);
    assert!(out.status.success(), "board listing with an ambiguous slug");
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(
        listing.contains(&short(&owner)) && listing.contains(&short(&other)),
        "both same-slug boards must be listed with their owners:\n{listing}"
    );

    // An explicit --author still picks the second owner's board.
    show_as_until(&member_key, &url, member_db, &other.hex(), slug, |b| {
        find_titled(b, "Other card").is_some()
    });

    // 4. A board of the member's own by that slug wins over both shared ones.
    let out = member_run(&["--board", slug, "seed"]);
    assert!(
        out.status.success(),
        "member seeds its own '{slug}': {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut own = Value::Null;
    for _ in 0..50 {
        let out = member_run(&["--board", slug, "show", "--json"]);
        if out.status.success()
            && let Ok(board) = serde_json::from_slice::<Value>(&out.stdout)
            && board.is_object()
        {
            own = board;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        own.is_object()
            && find_titled(&own, "Owner card").is_none()
            && find_titled(&own, "Other card").is_none(),
        "the member's own '{slug}' must win over the boards shared with it: {own:#}"
    );
}

/// A comment key signs comments and nothing else, and needs no board key.
///
/// The owner runs with a second key set as the comment key, the way an agent
/// running as its user would (headway:headway/lava-number-clap). On a sealed
/// board the comment folds attributed to that key, sealed into the board's
/// channel with the owner's access. A card added in the same way is still the
/// owner's, the comment key is never shared the board, and nothing it signed
/// reaches the relay as plaintext. On a plaintext board, which folds only its
/// owner's own events, the comment would never show, so it is refused.
#[test]
fn comment_key_signs_comments_on_a_sealed_board_only() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let app_dir = tempfile::tempdir().expect("app dir");
    let app_ndb = Ndb::new(
        app_dir.path().to_str().unwrap(),
        &test_config().set_ingester_threads(1),
    )
    .expect("app ndb");
    let relay_store = app_ndb.clone();
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
    let url = relay.url();

    let owner = author();
    let owner_hex = owner.hex();
    // Any second key: here the member key the share tests use, never shared a board.
    let agent = nostrdb_net::FullKeypair::from_secret_bytes(&MEMBER_SECRET)
        .expect("agent keypair")
        .pubkey;
    let agent_hex = agent.hex();
    let agent_key = hex::encode(MEMBER_SECRET);
    let slug = "sealed";

    let owner_dir = tempfile::tempdir().expect("owner dir");
    let owner_db = owner_dir.path().to_str().unwrap();
    let with_comment_key = |args: &[&str]| {
        let mut full = vec!["--comment-nsec", agent_key.as_str()];
        full.extend_from_slice(args);
        headway(&url, owner_db, &full)
    };

    assert!(
        headway(&url, owner_db, &["--board", slug, "seed"])
            .status
            .success(),
        "owner seed"
    );
    show_board_until_cols(&url, owner_db, slug, 5);

    // Adding a card with the comment key set: still the owner's card.
    let out = with_comment_key(&["--board", slug, "add", "owner card", "--col", "todo"]);
    assert!(
        out.status.success(),
        "add: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let board = show_board_until(&url, owner_db, slug, 1);
    let (card, _) = find_titled(&board, "owner card").expect("card folded");
    assert_eq!(
        card["author"], owner_hex,
        "only comments use the comment key"
    );
    let card_id = card["id"].as_str().expect("card id").to_string();

    let out = with_comment_key(&["--board", slug, "comment", &card_id, "hello from the agent"]);
    assert!(
        out.status.success(),
        "comment: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A fresh cache of the owner's folds the comment from the relay, attributed
    // to the comment key.
    let fresh_dir = tempfile::tempdir().expect("fresh dir");
    let fresh_db = fresh_dir.path().to_str().unwrap();
    let board = show_as_until(&nsec(), &url, fresh_db, &owner_hex, slug, |b| {
        find_titled(b, "owner card")
            .is_some_and(|(c, _)| c["comments"].as_array().is_some_and(|cs| !cs.is_empty()))
    });
    let (card, _) = find_titled(&board, "owner card").expect("card folded");
    let comment = &card["comments"][0];
    assert_eq!(comment["body"], "hello from the agent");
    assert_eq!(
        comment["author"], agent_hex,
        "the comment is the comment key's"
    );

    // The comment key holds no board key, and published nothing in the clear.
    assert_eq!(
        giftwraps_to(&relay_store, &agent),
        0,
        "no key-share to the agent"
    );
    assert_eq!(
        plaintext_member_notes(&relay_store, &agent),
        0,
        "the agent's comment leaked as plaintext"
    );

    // A plaintext board, published the way a pre-SNS client wrote one.
    let plain_dir = tempfile::tempdir().expect("plain dir");
    let plain_ndb =
        Ndb::new(plain_dir.path().to_str().unwrap(), &test_config()).expect("plain ndb");
    let mut frames = Frames::default();
    headway::store::seed_board(&plain_ndb, &owner, &SECRET, "plain", "Plain", &mut frames);
    rt.block_on(async {
        let mut relay = nostrdb_net::relay::sync::Relay::connect(&url)
            .await
            .expect("connect");
        relay
            .publish(&frames.0)
            .await
            .expect("publish plaintext board");
    });
    show_board_until_cols(&url, owner_db, "plain", 5);
    assert!(
        headway(
            &url,
            owner_db,
            &["--board", "plain", "add", "plain card", "--col", "todo"]
        )
        .status
        .success(),
        "add to the plaintext board"
    );
    let board = show_board_until(&url, owner_db, "plain", 1);
    let (card, _) = find_titled(&board, "plain card").expect("plain card folded");
    let plain_card = card["id"].as_str().expect("card id").to_string();

    let out = with_comment_key(&["--board", "plain", "comment", &plain_card, "unseen"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a comment key on a plaintext board must be refused"
    );
    assert!(
        err.contains("plaintext board"),
        "unexpected refusal:\n{err}"
    );
    assert_eq!(
        plaintext_member_notes(&relay_store, &agent),
        0,
        "the refused comment reached the relay"
    );
}
