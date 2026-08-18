//! End-to-end: drive the real `notebook` binary against a real embedded relay,
//! exercising the full loop — CLI → relay → app nostrdb → relay → CLI. Mirrors
//! `headway_cli`'s e2e, but over the notebook's event kinds (so it also covers
//! the notebook-specific addressable-kind dedup in the shared reconcile).

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

/// Run the `notebook` binary with the shared connection args plus `extra`.
fn notebook(url: &str, db: &str, extra: &[&str]) -> std::process::Output {
    let nsec = nsec();
    let mut args = vec!["--nsec", nsec.as_str(), "--relay", url, "--db", db];
    args.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_notebook"))
        .args(&args)
        .output()
        .expect("run notebook")
}

fn flushed(out: &std::process::Output) -> bool {
    String::from_utf8_lossy(&out.stderr).contains("flushed")
}

fn nodes(canvas: &Value) -> usize {
    canvas["nodes"].as_array().map_or(0, Vec::len)
}

/// The `ref` of the sole canvas in `author`'s vault, or `None` until it has synced.
/// `show` with no arg now lists the whole vault (typed rows), so the canvas is
/// addressed by the `notebook:` ref on its row rather than by dumping "the" canvas.
fn canvas_ref(url: &str, db: &str) -> Option<String> {
    let out = notebook(url, db, &["show", "--json"]);
    if !out.status.success() {
        return None;
    }
    let vault: Value = serde_json::from_slice(&out.stdout).ok()?;
    vault
        .as_array()?
        .iter()
        .find(|row| row["kind"] == "canvas")
        .and_then(|row| row["ref"].as_str())
        .map(str::to_string)
}

/// Poll until the sole canvas has materialised in the vault, then fetch it by ref
/// (non-null, has a title).
fn show_until_seeded(url: &str, db: &str) -> Value {
    for _ in 0..50 {
        if let Some(cref) = canvas_ref(url, db) {
            let out = notebook(url, db, &["show", &cref, "--json"]);
            if out.status.success()
                && let Ok(canvas) = serde_json::from_slice::<Value>(&out.stdout)
                && canvas.get("title").and_then(Value::as_str).is_some()
            {
                return canvas;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("canvas never materialised");
}

/// Poll until the sole canvas (fetched by ref) has `n` nodes (the relay ingests
/// asynchronously, so it may take a moment to fully materialise).
fn show_until_nodes(url: &str, db: &str, n: usize) -> Value {
    for _ in 0..50 {
        if let Some(cref) = canvas_ref(url, db) {
            let out = notebook(url, db, &["show", &cref, "--json"]);
            if out.status.success()
                && let Ok(canvas) = serde_json::from_slice::<Value>(&out.stdout)
                && nodes(&canvas) == n
            {
                return canvas;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("canvas never reached {n} nodes");
}

/// The full hex id of the first node on the canvas, for addressing a `move`.
fn first_node_id(canvas: &Value) -> String {
    canvas["nodes"].as_array().unwrap()[0]["id"]
        .as_str()
        .expect("node id")
        .to_string()
}

/// The `vault` command is a *local* read (it skips the relay reconcile), so this
/// exercises the browse path directly: seal a longform note into the CLI's own db
/// via the SNS workspace (as the app would), then drive the real binary to list it
/// and print its body. A dummy relay is passed but never dialed: `vault`
/// short-circuits before the reconcile.
#[test]
fn vault_lists_and_prints_local_longform() {
    use notebook::event;
    use notebook::store::{self, LongformInput, NoPublish};
    use std::time::Instant;

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // The author whose vault we write and read.
    let (_sk, pk) = nostrdb_net::relay::sync::parse_nsec(&nsec()).expect("nsec");
    let author = enostr::Pubkey::new(*pk.bytes());

    // Populate the CLI db in-process, then drop the handle so the binary opens it
    // cleanly. The vault is sealed into the account's SNS workspace, so register
    // its derived root — nostrdb only unwraps the kind-1081 envelopes once it is
    // registered — mirroring what the CLI/app does before reading.
    let d = {
        let ndb = Ndb::new(db, &Config::new().set_ingester_threads(1)).expect("ndb");
        store::register_workspace(&ndb, &SECRET);
        let input = LongformInput {
            title: "My Article".to_string(),
            summary: Some("a short summary".to_string()),
            content: "# My Article\n\nthe body".to_string(),
            published_at: None,
            hashtags: vec!["rust".to_string()],
        };
        let saved = store::create_longform(&ndb, &author, &SECRET, &input, None, &mut NoPublish)
            .expect("create longform");

        // Wait for the background ingester to unwrap + commit the note so the
        // separate binary process can query it. Poll rather than sleep-and-hope.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let txn = nostrdb::Transaction::new(&ndb).unwrap();
            let ready = !event::list_longform(&ndb, &txn, &author).is_empty();
            drop(txn);
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "longform note never materialised"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        saved.d
    };

    // `vault --json` lists the note with its metadata and canonical ref.
    let listed = notebook("ws://127.0.0.1:1", db, &["vault", "--json"]);
    assert!(
        listed.status.success(),
        "vault list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let arr: Value = serde_json::from_slice(&listed.stdout).expect("vault json");
    let row = &arr.as_array().expect("array")[0];
    assert_eq!(row["title"], "My Article");
    assert_eq!(row["summary"], "a short summary");
    assert_eq!(row["d"], d.as_str());
    assert_eq!(row["hashtags"][0], "rust");
    let reference = row["ref"].as_str().expect("ref");
    assert!(reference.starts_with("notebook:"), "human ref: {reference}");

    // Addressing the note by its `ref` prints the raw markdown body.
    let body = notebook("ws://127.0.0.1:1", db, &["vault", reference]);
    assert!(
        body.status.success(),
        "vault body failed: {}",
        String::from_utf8_lossy(&body.stderr)
    );
    let text = String::from_utf8_lossy(&body.stdout);
    assert!(
        text.contains("# My Article"),
        "body missing heading: {text}"
    );
    assert!(text.contains("the body"), "body missing content: {text}");

    // Addressing it by a unique `d` prefix works too.
    let by_prefix = notebook("ws://127.0.0.1:1", db, &["vault", &d[..6]]);
    assert!(
        String::from_utf8_lossy(&by_prefix.stdout).contains("the body"),
        "d-prefix selector failed"
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

    // Seed a titled canvas through the relay.
    let seed = notebook(&url, db, &["seed", "My Canvas"]);
    assert!(
        seed.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );

    // The seeded canvas comes back through a fresh sync: titled, no nodes.
    let canvas = show_until_seeded(&url, db);
    assert_eq!(canvas["title"], "My Canvas");
    assert_eq!(nodes(&canvas), 0);

    // Add a node; both the node creation and its placement transform must
    // round-trip back through the relay.
    let add = notebook(
        &url,
        db,
        &["add", "hello from the cli", "-x", "40", "-y", "20"],
    );
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let canvas = show_until_nodes(&url, db, 1);
    let node = &canvas["nodes"].as_array().unwrap()[0];
    assert_eq!(node["text"], "hello from the cli");
    assert_eq!(node["x"], 40);
    assert_eq!(node["y"], 20);
}

/// Write a PNS-wrapped longform note into the CLI's own db in-process (as the app
/// would), waiting for the background ingester to unwrap + commit it so a separate
/// binary process can read it. Returns the note's `d`. Longform never syncs over the
/// relay, so this is the only way a note reaches the CLI's vault.
fn write_local_longform(db: &str, title: &str, summary: Option<&str>, content: &str) -> String {
    use notebook::event;
    use notebook::store::{self, LongformInput, NoPublish};
    use std::time::Instant;

    let (_sk, pk) = nostrdb_net::relay::sync::parse_nsec(&nsec()).expect("nsec");
    let author = enostr::Pubkey::new(*pk.bytes());

    let ndb = Ndb::new(db, &Config::new().set_ingester_threads(1)).expect("ndb");
    // Register the vault's derived SNS workspace root so nostrdb unwraps the sealed
    // kind-1081 longform envelope this injects.
    store::register_workspace(&ndb, &SECRET);
    let input = LongformInput {
        title: title.to_string(),
        summary: summary.map(str::to_string),
        content: content.to_string(),
        published_at: None,
        hashtags: vec![],
    };
    let saved = store::create_longform(&ndb, &author, &SECRET, &input, None, &mut NoPublish)
        .expect("create longform");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let txn = nostrdb::Transaction::new(&ndb).unwrap();
        let ready = !event::list_longform(&ndb, &txn, &author).is_empty();
        drop(txn);
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "longform note never materialised"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    saved.d
}

/// `show` with no arg lists the whole vault: a local longform note and a
/// relay-synced canvas both surface as typed rows, each carrying its `notebook:` ref.
#[test]
fn show_lists_mixed_vault() {
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

    // A longform note (local-only) plus a canvas (synced over the relay) — the two
    // vault document kinds. Write the note before seeding so both are present.
    write_local_longform(db, "An Article", None, "# An Article\n\nbody");
    assert!(
        notebook(&url, db, &["seed", "A Canvas"]).status.success(),
        "seed"
    );
    show_until_seeded(&url, db);

    // `show --json` lists BOTH, typed, each with a notebook ref.
    let out = notebook(&url, db, &["show", "--json"]);
    assert!(
        out.status.success(),
        "show failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vault: Value = serde_json::from_slice(&out.stdout).expect("vault json");
    let rows = vault.as_array().expect("array");
    assert_eq!(
        rows.len(),
        2,
        "vault should list the note and the canvas: {vault}"
    );

    let note = rows.iter().find(|r| r["kind"] == "note").expect("note row");
    let canvas = rows
        .iter()
        .find(|r| r["kind"] == "canvas")
        .expect("canvas row");
    assert_eq!(note["title"], "An Article");
    assert_eq!(canvas["title"], "A Canvas");
    assert!(
        note["ref"].as_str().unwrap().starts_with("notebook:"),
        "note ref: {note}"
    );
    assert!(
        canvas["ref"].as_str().unwrap().starts_with("notebook:"),
        "canvas ref: {canvas}"
    );
}

/// `show <ref>` resolves one selector across the whole vault and dispatches the
/// render on the resolved type: a canvas ref prints the canvas, a node ref prints
/// the node, and a note ref prints its raw markdown body.
#[test]
fn show_ref_dispatches_by_type() {
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

    // One of each vault kind, plus a node on the canvas.
    write_local_longform(db, "Ref Article", None, "# Ref Article\n\nthe article body");
    assert!(
        notebook(&url, db, &["seed", "Ref Canvas"]).status.success(),
        "seed"
    );
    show_until_seeded(&url, db);
    assert!(
        notebook(&url, db, &["add", "a ref node"]).status.success(),
        "add"
    );
    let canvas = show_until_nodes(&url, db, 1);

    // Each document's ref: the note/canvas from the vault listing, the node from the
    // canvas dump.
    let vault: Value = serde_json::from_slice(&notebook(&url, db, &["show", "--json"]).stdout)
        .expect("vault json");
    let rows = vault.as_array().expect("array");
    let note_ref = rows.iter().find(|r| r["kind"] == "note").unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_string();
    let canvas_reference = rows.iter().find(|r| r["kind"] == "canvas").unwrap()["ref"]
        .as_str()
        .unwrap()
        .to_string();
    let node_ref = canvas["nodes"].as_array().unwrap()[0]["ref"]
        .as_str()
        .unwrap()
        .to_string();

    // Canvas ref → a canvas object (title + its node).
    let cj: Value =
        serde_json::from_slice(&notebook(&url, db, &["show", &canvas_reference, "--json"]).stdout)
            .expect("canvas json");
    assert_eq!(cj["title"], "Ref Canvas");
    assert_eq!(nodes(&cj), 1);

    // Node ref → a node object (its text).
    let nj: Value =
        serde_json::from_slice(&notebook(&url, db, &["show", &node_ref, "--json"]).stdout)
            .expect("node json");
    assert_eq!(nj["text"], "a ref node");

    // Note ref, no --json → the raw markdown body, so `show <note> > note.md`
    // round-trips.
    let body = notebook(&url, db, &["show", &note_ref]);
    assert!(
        body.status.success(),
        "note body failed: {}",
        String::from_utf8_lossy(&body.stderr)
    );
    let text = String::from_utf8_lossy(&body.stdout);
    assert!(
        text.contains("# Ref Article"),
        "body missing heading: {text}"
    );
    assert!(
        text.contains("the article body"),
        "body missing content: {text}"
    );
}

/// Moving a node writes a new transform revision and supersedes the old one,
/// which lingers in the CLI's append-only cache after the relay has replaced it.
/// A settled canvas must not keep re-flushing that dropped revision every run —
/// the reconcile has to converge. This is the notebook-specific exercise of the
/// shared `frames_where` addressable dedup (over transforms, not placements).
#[test]
fn reconcile_converges_after_replacing_a_transform() {
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

    assert!(notebook(&url, db, &["seed"]).status.success(), "seed");
    show_until_seeded(&url, db);
    assert!(
        notebook(&url, db, &["add", "a node"]).status.success(),
        "add"
    );
    let canvas = show_until_nodes(&url, db, 1);

    // Move the node: a fresh transform (same d-tag, newer created_at) replaces
    // the original, so the relay drops the old id the cache still holds.
    let node = first_node_id(&canvas);
    let mv = notebook(&url, db, &["move", &node, "-x", "500", "-y", "250"]);
    assert!(
        mv.status.success(),
        "move failed: {}",
        String::from_utf8_lossy(&mv.stderr)
    );

    // Once the relay has ingested the new transform, `show` should stop finding
    // anything to flush. Allow a few runs for async ingest, then require it.
    let mut converged = false;
    for _ in 0..50 {
        if !flushed(&notebook(&url, db, &["show"])) {
            converged = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(converged, "show kept re-flushing the superseded transform");

    // And it stays converged — the next run is silent too.
    assert!(
        !flushed(&notebook(&url, db, &["show"])),
        "a settled canvas must not re-flush superseded events"
    );
}
