//! The `notebook` CLI's e2e cases, ported to the tools that replace it: each
//! command runs through the registry, as `nd notebook <verb>` runs it, against
//! a live notedeck signed in as a fresh account, and is read back with
//! `notebook_show`, whose JSON is the CLI's `show --json`. This coverage is
//! what makes deleting the CLI safe.

use super::*;
use nostrdb_net::FullKeypair;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// A notedeck signed in as a fresh account with an empty vault. Bind it for
/// the whole test: the database lives in `_dir`.
struct Live {
    _dir: TempDir,
    notedeck: notedeck::Notedeck,
    account: FullKeypair,
}

/// What a tool answered: its JSON and the text `nd` prints for it.
struct Answer {
    json: Value,
    text: String,
}

fn live() -> Live {
    let dir = TempDir::new().expect("tmp dir");
    let account = FullKeypair::generate();
    let args: Vec<String> = [
        "notedeck-test",
        "--testrunner",
        "--nsec",
        &account.secret_key.to_secret_hex(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
    Live {
        _dir: dir,
        notedeck,
        account,
    }
}

impl Live {
    /// Call the registered tool `name`, as `nd` would: its answer and text,
    /// or its error.
    fn try_call(&mut self, name: &str, args: Value) -> Result<Answer, String> {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        let mut app = self.notedeck.app_context();
        let mut cx = app.tool_context();
        let rendered = tool.call_rendered(&mut cx, &args);
        match rendered.outcome {
            notedeck::ToolOutcome::Data(json) => Ok(Answer {
                json,
                text: rendered.text.expect("every notebook tool renders text"),
            }),
            notedeck::ToolOutcome::Error(err) => Err(err),
            notedeck::ToolOutcome::Intent(_) => panic!("{name} answered with an intent"),
        }
    }

    /// [`try_call`](Self::try_call), failing the test on an error.
    fn call(&mut self, name: &str, args: Value) -> Answer {
        self.try_call(name, args.clone())
            .unwrap_or_else(|err| panic!("{name} {args}: {err}"))
    }

    /// `name`'s answer once `ready` holds of it. Panics after a bounded wait:
    /// ingest is async.
    fn until(
        &mut self,
        name: &str,
        args: Value,
        what: &str,
        ready: impl Fn(&Value) -> bool,
    ) -> Answer {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(answer) = self.try_call(name, args.clone())
                && ready(&answer.json)
            {
                return answer;
            }
            assert!(Instant::now() < deadline, "never settled: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The vault listing once it has `n` rows.
    fn vault_rows(&mut self, n: usize) -> Vec<Value> {
        let shown = self.until("notebook_show", json!({}), "the vault rows", |v| {
            v.as_array().is_some_and(|rows| rows.len() == n)
        });
        shown.json.as_array().unwrap().clone()
    }

    /// Seed a canvas titled `title` and wait for it to read back: its ref.
    fn seed(&mut self, title: &str) -> String {
        let seeded = self.call("notebook_seed", json!({ "title": title }));
        let canvas_ref = seeded.json["ref"].as_str().unwrap().to_owned();
        self.canvas_until(&canvas_ref, "the seeded canvas", |_| true);
        canvas_ref
    }

    /// The canvas `canvas_ref` names as `show <ref> --json` prints it, once
    /// `ready` holds of it.
    fn canvas_until(
        &mut self,
        canvas_ref: &str,
        what: &str,
        ready: impl Fn(&Value) -> bool,
    ) -> Value {
        self.until(
            "notebook_show",
            json!({ "refs": [canvas_ref] }),
            what,
            |c| c["title"].is_string() && ready(c),
        )
        .json
    }

    /// A longform note sealed into the vault as the app's editor writes it,
    /// once it reads back: its `d`.
    fn longform(&mut self, title: &str, summary: Option<&str>, content: &str) -> String {
        let ndb = self.notedeck.app_context().ndb.clone();
        let secret = self.account.secret_key.secret_bytes();
        store::register_workspace(&ndb, &secret);
        let input = event::LongformInput {
            title: title.to_owned(),
            summary: summary.map(str::to_owned),
            content: content.to_owned(),
            published_at: None,
            hashtags: vec!["rust".to_owned()],
        };
        let saved = store::create_longform(
            &ndb,
            &self.account.pubkey,
            &secret,
            &input,
            None,
            &mut store::NoPublish,
        )
        .expect("create longform");
        let d = saved.d;
        self.until("notebook_vault", json!({}), "the longform note", |notes| {
            notes
                .as_array()
                .is_some_and(|notes| notes.iter().any(|n| n["d"] == d.as_str()))
        });
        d
    }
}

/// The nodes of a canvas as `show --json` prints it.
fn nodes(canvas: &Value) -> &[Value] {
    canvas["nodes"].as_array().map_or(&[], Vec::as_slice)
}

/// `vault` lists the longform notes with their metadata and canonical ref,
/// and a ref or a unique `d` prefix answers that note with its body, which
/// renders raw. (Ports `vault_lists_and_prints_local_longform`.)
#[tokio::test]
async fn vault_lists_and_prints_longform() {
    let mut live = live();
    let d = live.longform(
        "My Article",
        Some("a short summary"),
        "# My Article\n\nthe body",
    );

    let listed = live.call("notebook_vault", json!({}));
    let row = &listed.json[0];
    assert_eq!(row["title"], "My Article");
    assert_eq!(row["summary"], "a short summary");
    assert_eq!(row["d"], d.as_str());
    assert_eq!(row["hashtags"][0], "rust");
    let reference = row["ref"].as_str().expect("ref").to_owned();
    assert!(reference.starts_with("notebook:"), "{reference}");
    assert!(
        listed.text.starts_with("Vault (1 note)\n"),
        "{}",
        listed.text
    );
    assert!(listed.text.contains("My Article"), "{}", listed.text);

    // Addressing the note by its ref answers it, and renders its raw body.
    let body = live.call("notebook_vault", json!({ "notes": [reference] }));
    assert_eq!(body.text, "# My Article\n\nthe body\n");
    assert_eq!(body.json[0]["content"], "# My Article\n\nthe body");

    // So does a unique `d` prefix.
    let by_prefix = live.call("notebook_vault", json!({ "notes": [&d[..6]] }));
    assert!(by_prefix.text.contains("the body"), "{}", by_prefix.text);

    // An unknown note fails the call.
    let err = live
        .try_call("notebook_vault", json!({ "notes": ["zzzz"] }))
        .err()
        .expect("an unknown note fails");
    assert!(err.contains("no vault note matching 'zzzz'"), "{err}");
}

/// `seed` creates a titled, empty canvas and answers its ref; `add` places a
/// node where asked and answers the node's ref, which `show` resolves.
/// (Ports `seed_show_and_add_round_trip`.)
#[tokio::test]
async fn seed_show_and_add_round_trip() {
    let mut live = live();
    let seeded = live.call("notebook_seed", json!({ "title": "My Canvas" }));
    let mut keys: Vec<&str> = seeded
        .json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["canvas", "events", "ok", "ref"]);
    let canvas_ref = seeded.json["ref"].as_str().unwrap().to_owned();
    assert!(
        seeded.text.starts_with("seeded canvas '") && seeded.text.contains(&canvas_ref),
        "{}",
        seeded.text
    );

    let canvas = live.canvas_until(&canvas_ref, "the seeded canvas", |_| true);
    assert_eq!(canvas["title"], "My Canvas");
    assert!(nodes(&canvas).is_empty());

    let added = live.call(
        "notebook_add_node",
        json!({ "text": "hello from the tool", "x": 40, "y": 20 }),
    );
    let node_ref = added.json["ref"].as_str().unwrap().to_owned();
    assert!(node_ref.starts_with("notebook:"), "{node_ref}");
    assert_eq!(added.json["node"].as_str().unwrap().len(), 64);
    assert!(
        added.text.starts_with("ok (2 events) — ") && added.text.contains(&node_ref),
        "{}",
        added.text
    );

    let canvas = live.canvas_until(&canvas_ref, "the added node", |c| nodes(c).len() == 1);
    let node = &nodes(&canvas)[0];
    assert_eq!(node["text"], "hello from the tool");
    assert_eq!(
        (node["x"].clone(), node["y"].clone()),
        (json!(40), json!(20))
    );
    assert_eq!(
        (node["w"].clone(), node["h"].clone()),
        (json!(250), json!(120))
    );
    assert_eq!(node["ref"], node_ref.as_str());

    // The canvas renders its title and the node's line.
    let shown = live.call("notebook_show", json!({ "refs": [canvas_ref] }));
    assert!(
        shown.text.starts_with("My Canvas\n\nNodes (1)\n"),
        "{}",
        shown.text
    );
    assert!(shown.text.contains("hello from the tool"), "{}", shown.text);
}

/// `show` with no refs lists the whole vault as typed rows each carrying its
/// ref; with refs it answers what each names — a canvas, a node, or a note
/// whose body renders raw — one ref as its object and several as an array.
/// (Ports `show_lists_mixed_vault` and `show_ref_dispatches_by_type`.)
#[tokio::test]
async fn show_lists_the_vault_and_dispatches_refs_by_type() {
    let mut live = live();
    live.longform("Ref Article", None, "# Ref Article\n\nthe article body");
    let canvas_ref = live.seed("Ref Canvas");
    live.call("notebook_add_node", json!({ "text": "a ref node" }));
    let canvas = live.canvas_until(&canvas_ref, "the node", |c| nodes(c).len() == 1);
    let node_ref = nodes(&canvas)[0]["ref"].as_str().unwrap().to_owned();

    let rows = live.vault_rows(2);
    let note = rows
        .iter()
        .find(|r| r["kind"] == "note")
        .expect("a note row");
    let canvas_row = rows
        .iter()
        .find(|r| r["kind"] == "canvas")
        .expect("a canvas row");
    assert_eq!(note["title"], "Ref Article");
    assert_eq!(canvas_row["title"], "Ref Canvas");
    assert_eq!(canvas_row["ref"], canvas_ref.as_str());
    let note_ref = note["ref"].as_str().unwrap().to_owned();
    assert!(note_ref.starts_with("notebook:"), "{note_ref}");
    let listing = live.call("notebook_show", json!({}));
    assert!(
        listing.text.starts_with("Vault (2 items)\n"),
        "{}",
        listing.text
    );
    assert!(listing.text.contains("[canvas]"), "{}", listing.text);

    let shown = live.call("notebook_show", json!({ "refs": [canvas_ref] }));
    assert_eq!(shown.json["title"], "Ref Canvas");
    assert_eq!(nodes(&shown.json).len(), 1);

    let shown = live.call("notebook_show", json!({ "refs": [node_ref] }));
    assert_eq!(shown.json["text"], "a ref node");
    assert!(shown.text.ends_with("\na ref node\n"), "{}", shown.text);

    // A note renders its raw body, so `show <note> > note.md` round-trips.
    let shown = live.call("notebook_show", json!({ "refs": [note_ref] }));
    assert_eq!(shown.text, "# Ref Article\n\nthe article body\n");
    assert_eq!(shown.json["title"], "Ref Article");

    let both = live.call("notebook_show", json!({ "refs": [note_ref, node_ref] }));
    assert_eq!(both.json.as_array().map(Vec::len), Some(2));

    // One unknown ref fails the whole call.
    let err = live
        .try_call("notebook_show", json!({ "refs": [node_ref, "zzzz"] }))
        .err()
        .expect("an unknown ref fails");
    assert!(err.contains("no notebook item matching 'zzzz'"), "{err}");
}

/// Every canvas edit lands and reads back: a move that keeps the parts it
/// isn't given, a text edit, a color set then cleared, a restack, an edge
/// drawn with its sides then removed, a delete, and a rename.
#[tokio::test]
async fn node_edits_round_trip() {
    let mut live = live();
    let canvas_ref = live.seed("Edits");
    let first = live.call("notebook_add_node", json!({ "text": "first" }));
    let first = first.json["ref"].as_str().unwrap().to_owned();
    let second = live.call("notebook_add_node", json!({ "text": "second", "x": 300 }));
    let second = second.json["ref"].as_str().unwrap().to_owned();
    live.canvas_until(&canvas_ref, "two nodes", |c| nodes(c).len() == 2);
    let node = |c: &Value, r: &str| -> Value {
        nodes(c)
            .iter()
            .find(|n| n["ref"] == r)
            .cloned()
            .unwrap_or(Value::Null)
    };

    let moved = live.call(
        "notebook_move_node",
        json!({ "node": first, "x": 500, "height": 60 }),
    );
    assert_eq!(moved.json, json!({ "ok": true, "events": 1 }));
    assert_eq!(moved.text, "ok (1 events)\n");
    live.call(
        "notebook_edit_node",
        json!({ "node": first, "text": "first, edited" }),
    );
    live.call(
        "notebook_color_node",
        json!({ "node": second, "color": "4" }),
    );
    let c = live.canvas_until(&canvas_ref, "the move, edit and color", |c| {
        let (f, s) = (node(c, &first), node(c, &second));
        f["x"] == 500 && f["text"] == "first, edited" && s["color"] == "4"
    });
    let f = node(&c, &first);
    assert_eq!(
        (f["y"].clone(), f["w"].clone(), f["h"].clone()),
        (json!(0), json!(250), json!(60))
    );

    live.call(
        "notebook_color_node",
        json!({ "node": second, "color": "none" }),
    );
    live.canvas_until(&canvas_ref, "the cleared color", |c| {
        node(c, &second)["color"].is_null()
    });

    // `second` was added last, so it's on top; restack it to the bottom.
    live.call(
        "notebook_restack_node",
        json!({ "node": second, "index": 0 }),
    );
    live.canvas_until(&canvas_ref, "the restack", |c| {
        nodes(c)[0]["ref"] == second.as_str()
    });

    live.call(
        "notebook_connect",
        json!({ "from": first, "to": second, "from_side": "right", "to_side": "left" }),
    );
    let c = live.canvas_until(&canvas_ref, "the edge", |c| {
        c["edges"].as_array().is_some_and(|e| e.len() == 1)
    });
    let edge = &c["edges"][0];
    assert_eq!(
        (edge["from_side"].clone(), edge["to_side"].clone()),
        (json!("right"), json!("left"))
    );
    assert_eq!(edge["to_end"], "arrow");
    let edge_id = edge["id"].as_str().unwrap().to_owned();
    live.call("notebook_disconnect", json!({ "edge": &edge_id[..12] }));
    live.canvas_until(&canvas_ref, "the edge removed", |c| {
        c["edges"].as_array().is_some_and(Vec::is_empty)
    });

    live.call("notebook_delete_node", json!({ "node": first }));
    live.call("notebook_rename_canvas", json!({ "title": "Renamed" }));
    let c = live.canvas_until(&canvas_ref, "the delete and rename", |c| {
        nodes(c).len() == 1 && c["title"] == "Renamed"
    });
    assert_eq!(nodes(&c)[0]["ref"], second.as_str());

    // A node that isn't there fails the edit.
    let err = live
        .try_call("notebook_edit_node", json!({ "node": "zzzz", "text": "x" }))
        .err()
        .expect("an unknown node fails");
    assert!(err.contains("no node matching 'zzzz'"), "{err}");
}

/// An edit infers the canvas only when there is one: with none it says to
/// seed, with several it asks for `canvas`, which takes a ref or a `d`.
/// `seed` refuses a `d` already in use.
#[tokio::test]
async fn the_canvas_is_inferred_only_when_there_is_one() {
    let mut live = live();
    let err = live
        .try_call("notebook_add_node", json!({ "text": "nowhere" }))
        .err()
        .expect("no canvas yet");
    assert!(err.contains("no canvas yet"), "{err}");

    let one = live.call(
        "notebook_seed",
        json!({ "title": "One", "canvas": "c0ffee" }),
    );
    assert_eq!(one.json["canvas"], "c0ffee");
    live.canvas_until(one.json["ref"].as_str().unwrap(), "canvas one", |_| true);
    let two = live.seed("Two");
    live.vault_rows(2);

    let err = live
        .try_call("notebook_add_node", json!({ "text": "which?" }))
        .err()
        .expect("several canvases");
    assert!(err.contains("several canvases"), "{err}");

    live.call(
        "notebook_add_node",
        json!({ "text": "on two", "canvas": two }),
    );
    live.call(
        "notebook_add_node",
        json!({ "text": "on one", "canvas": "c0f" }),
    );
    let c = live.canvas_until(&two, "the node on two", |c| nodes(c).len() == 1);
    assert_eq!(nodes(&c)[0]["text"], "on two");

    let err = live
        .try_call("notebook_seed", json!({ "canvas": "c0ffee" }))
        .err()
        .expect("a d in use");
    assert!(err.contains("canvas 'c0ffee' already exists"), "{err}");
}

/// A context with no `remote` is read-only: an edit is refused rather than
/// written where the caller asked for none.
#[tokio::test]
async fn a_read_only_context_refuses_edits() {
    let mut live = live();
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name() == "notebook_seed")
        .expect("seed is registered");
    let mut app = live.notedeck.app_context();
    let mut cx = app.tool_context();
    cx.remote = None;
    match tool.call(&mut cx, &json!({ "title": "nope" })) {
        notedeck::ToolOutcome::Error(err) => {
            assert!(err.contains("publishing is unavailable"), "{err}")
        }
        _ => panic!("a read-only context must refuse an edit"),
    }
}

/// Every `notebook` CLI command but `login` and `logout` is a command path,
/// and only the reads are read-only.
#[test]
fn every_cli_command_is_a_tool() {
    let mut paths: Vec<(String, bool)> = tools()
        .iter()
        .map(|tool| {
            let spec = tool.spec();
            (spec.command_path().join(" "), spec.is_read_only())
        })
        .collect();
    paths.sort();
    let expected = [
        ("notebook add", false),
        ("notebook color", false),
        ("notebook connect", false),
        ("notebook delete", false),
        ("notebook disconnect", false),
        ("notebook edit", false),
        ("notebook move", false),
        ("notebook rename", false),
        ("notebook restack", false),
        ("notebook seed", false),
        ("notebook show", true),
        ("notebook vault", true),
    ];
    let expected: Vec<(String, bool)> = expected.iter().map(|(p, r)| (p.to_string(), *r)).collect();
    assert_eq!(paths, expected);
}
