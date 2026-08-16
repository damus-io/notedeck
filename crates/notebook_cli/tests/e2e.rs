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

/// Poll `show --json` until the canvas has materialised (non-null, has a title).
fn show_until_seeded(url: &str, db: &str) -> Value {
    for _ in 0..50 {
        let out = notebook(url, db, &["show", "--json"]);
        if out.status.success()
            && let Ok(canvas) = serde_json::from_slice::<Value>(&out.stdout)
            && canvas.get("title").and_then(Value::as_str).is_some()
        {
            return canvas;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("canvas never materialised");
}

/// Poll `show --json` until the canvas has `n` nodes (the relay ingests
/// asynchronously, so it may take a moment to fully materialise).
fn show_until_nodes(url: &str, db: &str, n: usize) -> Value {
    for _ in 0..50 {
        let out = notebook(url, db, &["show", "--json"]);
        if out.status.success()
            && let Ok(canvas) = serde_json::from_slice::<Value>(&out.stdout)
            && nodes(&canvas) == n
        {
            return canvas;
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

/// The vault is a *local* read — longform never syncs over the relay — so this
/// exercises the browse path directly: write a PNS-wrapped longform note into the
/// CLI's own db (as the app would), then drive the real binary to list it and
/// print its body. A dummy relay is passed but never dialed: `vault` short-circuits
/// before the reconcile.
#[test]
fn vault_lists_and_prints_local_longform() {
    use notedeck_notebook::event;
    use notedeck_notebook::store::{self, LongformInput, NoPublish};
    use std::time::Instant;

    let cli_dir = tempfile::tempdir().expect("cli dir");
    let db = cli_dir.path().to_str().unwrap();

    // The author whose vault we write and read.
    let (_sk, pk) = nostrdb_net::relay::sync::parse_nsec(&nsec()).expect("nsec");
    let author = enostr::Pubkey::new(*pk.bytes());

    // Populate the CLI db in-process, then drop the handle so the binary opens it
    // cleanly. nostrdb only unwraps the PNS envelope once the device key is
    // registered — mirror the app's account-add before creating the note.
    let d = {
        let ndb = Ndb::new(db, &Config::new().set_ingester_threads(1)).expect("ndb");
        assert!(ndb.add_key(&SECRET), "register the device key");
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
    let relay = nostrdb_relay::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
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
    let relay = nostrdb_relay::spawn(app_ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");
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
