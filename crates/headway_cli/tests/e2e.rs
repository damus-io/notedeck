//! End-to-end: drive the real `headway` binary against a real embedded relay,
//! exercising the full loop — CLI → relay → app nostrdb → relay → CLI.

use std::process::Command;
use std::time::Duration;

use nostrdb::{Config, Filter, Ndb, Transaction};
use serde_json::Value;

/// Test signing key — the same all-`0x42` secret the relay's own roundtrip test
/// uses (a valid secp256k1 key).
const SECRET: [u8; 32] = [0x42; 32];

fn nsec() -> String {
    let hrp = bech32::Hrp::parse("nsec").expect("hrp");
    bech32::encode::<bech32::Bech32>(hrp, &SECRET).expect("encode nsec")
}

/// The account pubkey behind [`SECRET`] — the author whose board the relay's
/// store is inspected for below.
fn author() -> enostr::Pubkey {
    enostr::FullKeypair::from_secret_bytes(&SECRET)
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
fn plaintext_board_notes(ndb: &Ndb, author: &enostr::Pubkey) -> usize {
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
    let mut args = vec!["--nsec", "<nsec>", "--relay", url, "--db", db];
    let nsec = nsec();
    args[1] = &nsec;
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
        &Config::new().set_ingester_threads(1),
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
        &Config::new().set_ingester_threads(1),
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
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
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
        &Config::new().set_ingester_threads(1),
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
    let seed = headway(dead, db, &["seed"]);
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
        &Config::new().set_ingester_threads(1),
    )
    .expect("app ndb");
    let _guard = rt.enter();
    let relay =
        nostrdb_net::relay::server::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
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
        &Config::new().set_ingester_threads(1),
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
        headway(&url, db, &["seed"]).status.success(),
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
    let seed = headway(url, db, &["seed"]);
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
        &Config::new().set_ingester_threads(1),
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
        &Config::new().set_ingester_threads(1),
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
        &Config::new().set_ingester_threads(1),
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
        headway(dead, db, &["seed"]).status.success(),
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
