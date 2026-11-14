//! The `headway` CLI's structure and work-order commands — `parent`,
//! `block`/`unblock`, `relate`/`unrelate`, `seq`, `next`, `terminal` and
//! `show`'s `--archived`/`--all`/`--source` — ported to the tools that
//! replace them, run through the registry as `nd headway <verb>` runs them.
//! The `next` frontier cases matter most: autowork runs `next --in <epic>
//! -n 1` on every link, and reads `show --json`'s `blocks` and `terminal`.

use super::live::{Ack, Live, keys, live, next_second};
use super::*;
use headway::event::{ReviewFields, ReviewSource, ReviewSourceRole};

/// The refs of an epic and its subissues, in the order they were added.
struct Epic {
    epic: String,
    subs: Vec<String>,
}

/// An epic with `n` subissues under it, each settled so later edits resolve
/// it, listed in the epic's work-order. That isn't always the order they
/// were added in: cards made in one second tie on their creation time.
fn epic(live: &mut Live, n: usize) -> Epic {
    let epic = add(live, json!({ "title": "the epic" }));
    for i in 0..n {
        add(
            live,
            json!({ "title": format!("step {i}"), "parent": epic }),
        );
    }
    let order = next_until(
        live,
        json!({ "container": epic, "ready": true }),
        "the subissues",
        |r| r.len() == n,
    );
    let subs = order
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["ref"].as_str().unwrap().to_owned())
        .collect();
    Epic { epic, subs }
}

/// Add a card and wait for it to read back: its ref.
fn add(live: &mut Live, args: serde_json::Value) -> String {
    live.add(args)
}

/// `next` with `args` once `ready` holds of the refs it hands out, with the
/// text `nd headway next` prints. Panics after a bounded wait.
fn next_until(
    live: &mut Live,
    args: serde_json::Value,
    what: &str,
    ready: impl Fn(&[&str]) -> bool,
) -> Ack {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let next = live.edit("headway_ready_items", args.clone());
        let refs: Vec<&str> = next
            .json
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["ref"].as_str().unwrap())
            .collect();
        if ready(&refs) {
            return next;
        }
        assert!(Instant::now() < deadline, "never settled: {what}: {refs:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The refs in a card's `blocked_by`, `blocks` or `related` list.
fn edge_refs<'a>(card: &'a serde_json::Value, list: &str) -> Vec<&'a str> {
    card[list]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["ref"].as_str().unwrap())
        .collect()
}

/// `next --in <epic>` walks the epic's subissues in creation order and
/// answers one card unless asked for the frontier; a block holds a card out
/// of it until its blocker sits in a terminal column; a block that closes a
/// cycle is refused, and one already there is a no-op. Once every subissue
/// is done the epic is done too, so neither it nor anything under it is
/// handed out. `show --json` carries the edge on both cards, as autowork
/// reads `.blocks`.
#[tokio::test]
async fn next_follows_blocks_terminal_columns_and_containers() {
    let mut live = live();
    let Epic { epic, subs } = epic(&mut live, 2);
    let (a, b) = (&subs[0], &subs[1]);

    let first = next_until(&mut live, json!({ "container": epic }), "one card", |r| {
        r == [a.as_str()]
    });
    assert_eq!(keys(&first.json[0]), ["id", "ref", "title"]);
    let title = first.json[0]["title"].as_str().unwrap();
    assert_eq!(plain(&first.text), format!("{a}  {title}\n"));
    next_until(
        &mut live,
        json!({ "container": epic, "ready": true }),
        "the frontier",
        |r| r == [a.as_str(), b.as_str()],
    );

    let ack = live.edit("headway_block", json!({ "card": a, "on": b }));
    assert_eq!(ack.text, "ok (1 events)\n");
    next_until(
        &mut live,
        json!({ "container": epic, "ready": true }),
        "a held back by b",
        |r| r == [b.as_str()],
    );
    let shown = live.card_until(b, "the edge on the blocker", |c| {
        !c["blocks"].as_array().unwrap().is_empty()
    });
    assert_eq!(edge_refs(&shown, "blocks"), [a.as_str()]);
    let shown = live.card_until(a, "the edge on the blocked", |c| {
        c["blocked"] == json!(true)
    });
    assert_eq!(edge_refs(&shown, "blocked_by"), [b.as_str()]);

    let err = live
        .call("headway_block", json!({ "card": b, "on": a }))
        .unwrap_err();
    assert!(err.starts_with("refused: "), "{err}");
    let again = live.edit("headway_block", json!({ "card": a, "on": b }));
    assert_eq!(again.json["events"], json!(0));
    assert!(again.json["noop"].is_string(), "{}", again.json);

    // Done (In Review is terminal on a new board) clears what it blocks.
    live.edit(
        "headway_move_card",
        json!({ "card": b, "column": "In Review" }),
    );
    next_until(
        &mut live,
        json!({ "container": epic, "ready": true }),
        "a cleared",
        |r| r == [a.as_str()],
    );
    live.edit("headway_move_card", json!({ "card": a, "column": "Done" }));
    let empty = next_until(&mut live, json!({ "container": epic }), "drained", |r| {
        r.is_empty()
    });
    assert_eq!(empty.text, "", "nothing on stdout when nothing is ready");
    let root = next_until(&mut live, json!({ "ready": true }), "the done epic", |r| {
        !r.contains(&a.as_str())
    });
    assert!(
        !root.json.to_string().contains(epic.as_str()),
        "an epic whose subissues are done is done: {}",
        root.json
    );

    next_second();
    live.edit("headway_unblock", json!({ "card": a, "on": b }));
    live.card_until(a, "the unblock", |c| {
        c["blocked_by"].as_array().unwrap().is_empty()
    });
}

/// `-l` keeps only frontier cards carrying every label, before the count;
/// `-n` caps the frontier.
#[tokio::test]
async fn next_filters_by_label_before_the_count() {
    let mut live = live();
    let epic = add(&mut live, json!({ "title": "the epic" }));
    let plain_card = add(&mut live, json!({ "title": "plain", "parent": epic }));
    let tagged = add(
        &mut live,
        json!({ "title": "tagged", "parent": epic, "labels": "cli,ux" }),
    );
    next_until(
        &mut live,
        json!({ "container": epic, "labels": ["cli"] }),
        "the labelled card first",
        |r| r == [tagged.as_str()],
    );
    next_until(
        &mut live,
        json!({ "container": epic, "labels": "cli,nope", "ready": true }),
        "every label",
        |r| r.is_empty(),
    );
    next_until(
        &mut live,
        json!({ "container": epic, "limit": 2 }),
        "both",
        |r| r.len() == 2 && r.contains(&plain_card.as_str()) && r.contains(&tagged.as_str()),
    );
}

/// `relate` is symmetric, shown on both cards, never changes what is ready,
/// and either side can `unrelate`.
#[tokio::test]
async fn relate_is_symmetric_and_never_holds_a_card_back() {
    let mut live = live();
    let Epic { epic, subs } = epic(&mut live, 2);
    let (a, b) = (&subs[0], &subs[1]);

    live.edit("headway_relate", json!({ "card": a, "other": b }));
    let shown = live.card_until(b, "the edge on the other side", |c| {
        !c["related"].as_array().unwrap().is_empty()
    });
    assert_eq!(edge_refs(&shown, "related"), [a.as_str()]);
    next_until(
        &mut live,
        json!({ "container": epic, "ready": true }),
        "both still ready",
        |r| r == [a.as_str(), b.as_str()],
    );
    let again = live.edit("headway_relate", json!({ "card": b, "other": a }));
    assert!(again.json["noop"].is_string(), "{}", again.json);

    next_second();
    live.edit("headway_unrelate", json!({ "card": b, "other": a }));
    live.card_until(a, "the unrelate", |c| {
        c["related"].as_array().unwrap().is_empty()
    });
}

/// `seq` with no position reads the card's place, as `seq --json` prints
/// it; with one it moves the card, which `next` then hands out first. Two
/// positions, or an unsequenced anchor, are refused.
#[tokio::test]
async fn seq_reads_a_place_and_moves_a_card() {
    let mut live = live();
    let Epic { epic, subs } = epic(&mut live, 3);
    let last = &subs[2];

    let place = live.edit("headway_seq", json!({ "card": last, "container": epic }));
    assert_eq!(
        keys(&place.json),
        [
            "after",
            "before",
            "container",
            "index",
            "not_ready_why",
            "of",
            "ready",
            "ready_ahead",
            "ref",
            "sequenced"
        ]
    );
    assert_eq!(place.json["index"], json!(3));
    assert_eq!(place.json["of"], json!(3));
    assert_eq!(place.json["sequenced"], json!(false));
    assert_eq!(place.json["ready_ahead"], json!(2));
    assert_eq!(place.json["container"], epic.as_str());
    assert!(place.json["after"].is_null());
    assert_eq!(place.json["before"]["ref"], subs[1].as_str());
    let text = plain(&place.text);
    assert!(
        text.contains(&format!("  order   3 of 3 in {epic}\n")),
        "{text}"
    );
    assert!(
        text.contains("  ready   2 ready ahead; this one is ready"),
        "{text}"
    );

    let moved = live.edit(
        "headway_seq",
        json!({ "card": last, "container": epic, "first": true }),
    );
    assert_eq!(keys(&moved.json), ["events", "ok"]);
    next_until(
        &mut live,
        json!({ "container": epic }),
        "moved first",
        |r| r == [last.as_str()],
    );
    let place = live.edit("headway_seq", json!({ "card": last, "container": epic }));
    assert_eq!(place.json["index"], json!(1));
    assert_eq!(place.json["sequenced"], json!(true));

    let err = live
        .call(
            "headway_seq",
            json!({ "card": last, "container": epic, "first": true, "last": true }),
        )
        .unwrap_err();
    assert!(err.contains("at most one"), "{err}");
    let err = live
        .call(
            "headway_seq",
            json!({ "card": subs[0], "container": epic, "after": subs[1] }),
        )
        .unwrap_err();
    assert!(err.contains("isn't sequenced yet"), "{err}");
}

/// `terminal <col>` marks a column done, as `show --json` reports it, and
/// `next` stops handing out its cards; `off` clears it.
#[tokio::test]
async fn terminal_marks_a_column_done() {
    let mut live = live();
    let column = |board: &serde_json::Value| {
        board["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Todo")
            .unwrap()
            .clone()
    };
    let todo = |board: &serde_json::Value| column(board)["terminal"].clone();
    let board = live.board_until("the board", |_| true);
    assert_eq!(todo(&board), json!(false));
    let todo_cards: Vec<String> = column(&board)["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["ref"].as_str().unwrap().to_owned())
        .collect();
    assert!(!todo_cards.is_empty());

    let ack = live.edit("headway_set_terminal", json!({ "column": "todo" }));
    assert_eq!(keys(&ack.json), ["events", "ok"]);
    live.board_until("todo terminal", |b| todo(b) == json!(true));
    let frontier = live.edit("headway_ready_items", json!({ "ready": true }));
    for card in &todo_cards {
        assert!(!frontier.json.to_string().contains(card.as_str()), "{card}");
    }

    next_second();
    live.edit(
        "headway_set_terminal",
        json!({ "column": "Todo", "state": "off" }),
    );
    live.board_until("todo cleared", |b| todo(b) == json!(false));
    let err = live
        .call(
            "headway_set_terminal",
            json!({ "column": "todo", "state": "maybe" }),
        )
        .unwrap_err();
    assert!(err.contains("must be on/off"), "{err}");
}

/// `parent <card> <parent>` makes a subissue, a cycle is refused, and
/// `parent <card>` alone detaches it.
#[tokio::test]
async fn parent_sets_refuses_a_cycle_and_detaches() {
    let mut live = live();
    let top = add(&mut live, json!({ "title": "top" }));
    let child = add(&mut live, json!({ "title": "child" }));

    live.edit(
        "headway_set_parent",
        json!({ "card": child, "parent": top }),
    );
    live.card_until(&child, "the parent", |c| c["parent_ref"] == top.as_str());
    let err = live
        .call(
            "headway_set_parent",
            json!({ "card": top, "parent": child }),
        )
        .unwrap_err();
    assert!(err.starts_with("refused: "), "{err}");

    next_second();
    live.edit("headway_set_parent", json!({ "card": child }));
    live.card_until(&child, "the detach", |c| c["parent_ref"].is_null());
}

/// `show --all` is every readable board, each with its `owner`;
/// `--archived` lists archived cards in full; `<card> --source <name>`
/// answers that review source's text exactly as stored.
#[tokio::test]
async fn show_takes_all_archived_and_source() {
    let mut live = live();
    let all = live
        .call("headway_show_board", json!({ "all": true }))
        .unwrap();
    let all = all.as_array().expect("an array of boards");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["id"], store::BOARD_ID);
    assert_eq!(all[0]["owner"].as_str().map(str::len), Some(64));

    let card = live.cards()[0]["ref"].as_str().unwrap().to_owned();
    let title = live.cards()[0]["title"].as_str().unwrap().to_owned();
    live.edit("headway_archive_card", json!({ "card": card }));
    live.board_until("the archive", |b| {
        !b["archived"].as_array().unwrap().is_empty()
    });
    let listed = live.edit("headway_show_board", json!({ "archived": true }));
    let terse = live.edit("headway_show_board", json!({}));
    assert!(plain(&listed.text).contains(&title), "{}", listed.text);
    assert!(!plain(&terse.text).contains(&title), "{}", terse.text);

    let other = live.cards()[0]["ref"].as_str().unwrap().to_owned();
    {
        let mut app = live.notedeck.app_context();
        let mut cx = app.tool_context();
        mutate(&mut cx, store::BOARD_ID, |view| {
            Ok(BoardAction::AddReview {
                card: resolve_card(view, &other)?,
                review: ReviewFields {
                    commit: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
                    ..Default::default()
                },
                sources: vec![ReviewSource {
                    role: ReviewSourceRole::Explainer,
                    name: "x.md".to_owned(),
                    text: "# the explainer\n\nverbatim  ".to_owned(),
                }],
            })
        })
        .expect("records a review");
    }
    live.card_until(&other, "the record's source", |c| {
        c["reviews"][0]["sources"]
            .as_array()
            .is_some_and(|s| !s.is_empty())
    });
    let source = live.edit(
        "headway_show_card",
        json!({ "card": other, "source": "x.md" }),
    );
    assert_eq!(source.text, "# the explainer\n\nverbatim  ");
    assert_eq!(source.json, json!("# the explainer\n\nverbatim  "));
    let err = live
        .call(
            "headway_show_card",
            json!({ "card": other, "source": "nope.md" }),
        )
        .unwrap_err();
    assert!(err.contains("no review source named 'nope.md'"), "{err}");
}

/// Each command reads on a command line as the CLI's does: the second card
/// of `block`/`relate` is a positional or `--on`/`--to`, `parent`'s parent
/// and `terminal`'s state are positionals, and `next`/`seq` take `--in`.
#[test]
fn the_commands_read_like_the_clis() {
    let schema = |name: &str| {
        let tool = tools()
            .into_iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        (
            tool.spec().command_path().join(" "),
            tool.spec().json_schema(),
        )
    };
    for (name, command, second, long) in [
        ("headway_block", "headway block", "on", Some("on")),
        ("headway_unblock", "headway unblock", "on", Some("on")),
        ("headway_relate", "headway relate", "other", Some("to")),
        ("headway_unrelate", "headway unrelate", "other", Some("to")),
        ("headway_set_parent", "headway parent", "parent", None),
        ("headway_set_terminal", "headway terminal", "state", None),
    ] {
        let (path, schema) = schema(name);
        assert_eq!(path, command);
        let arg = &schema["properties"][second];
        assert_eq!(arg["x-nd-positional"], json!(1), "{name}");
        assert_eq!(arg["x-nd-long"].as_str(), long, "{name}");
    }
    for (name, command) in [
        ("headway_ready_items", "headway next"),
        ("headway_seq", "headway seq"),
    ] {
        let (path, schema) = schema(name);
        assert_eq!(path, command);
        assert_eq!(schema["properties"]["container"]["x-nd-long"], "in");
    }
    let (_, next) = schema("headway_ready_items");
    assert_eq!(next["properties"]["limit"]["x-nd-short"], "n");
    assert_eq!(next["properties"]["labels"]["x-nd-short"], "l");
}
