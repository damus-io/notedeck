//! The `headway` CLI's e2e cases for the card edits — `add`, `move`, `title`,
//! `desc`, `label`, `priority`, `due`, `estimate`, `comment`, `archive`,
//! `restore` and `delete` — ported to the tools that replace them. Each edit
//! runs through the registry, as `nd headway <verb>` runs it, against a live
//! notedeck signed in as a fresh account, and is read back with
//! `headway_show_card` / `headway_show_board`, whose JSON is the CLI's
//! `show --json`. This coverage is what makes deleting the CLI safe.

use super::live::{keys, labels, live, next_second};
use super::*;
use headway::event::ReviewFields;

/// `add` with every field it takes lands the card where asked, answers with
/// the CLI's `{ok, events, card, ref}` and its `ok (n events) — <ref>` line,
/// and the new card reads back with them all; `-l` values may be comma
/// lists. A second card added under it with `parent` is its subissue.
/// (Ports `seed_show_and_add_round_trip`.)
#[tokio::test]
async fn add_answers_with_its_ref_and_every_field_round_trips() {
    let mut live = live();
    let added = live.edit(
        "headway_add_card",
        json!({
            "title": "Wire up the CLI",
            "column": "Todo",
            "labels": ["cli,ux", "p1"],
            "description": "the details",
            "priority": "high",
            "estimate": "3",
            "due": "2026-11-02",
        }),
    );
    assert_eq!(keys(&added.json), ["card", "events", "ok", "ref"]);
    assert_eq!(added.json["ok"], json!(true));
    assert_eq!(
        json!(added.written),
        added.json["events"],
        "every event it wrote is named for nd to wait on"
    );
    assert!(added.json["events"].as_u64().unwrap() > 1, "{}", added.json);
    let card_ref = added.json["ref"].as_str().unwrap().to_owned();
    assert!(card_ref.starts_with("headway:headway/"), "{card_ref}");
    assert_eq!(added.json["card"].as_str().unwrap().len(), 64);
    assert!(
        added.text.starts_with("ok (") && added.text.contains(&card_ref),
        "{}",
        added.text
    );

    let card = live.card_until(&card_ref, "the added card", |c| {
        c["estimate"] == json!(3) && c["description"] == "the details"
    });
    assert_eq!(card["title"], "Wire up the CLI");
    assert_eq!(card["column"], "Todo");
    assert_eq!(labels(&card), ["cli", "p1", "ux"]);
    assert_eq!(card["priority"], "high");
    assert_eq!(card["due"], "2026-11-02");
    assert_eq!(card["ref"], card_ref.as_str());

    let sub = live.edit(
        "headway_add_card",
        json!({ "title": "a subissue", "parent": card_ref }),
    );
    let sub_ref = sub.json["ref"].as_str().unwrap().to_owned();
    let sub = live.card_until(&sub_ref, "the subissue's parent", |c| {
        !c["parent_ref"].is_null()
    });
    assert_eq!(sub["parent_ref"], card_ref.as_str());
    assert_eq!(sub["column"], "Backlog", "the first column by default");
}

/// Each field edit lands, answering `{ok, events}`: a title, a
/// description, a label set that replaces the card's (and clears it when
/// empty), a priority, a due date and an estimate (each cleared with
/// `none`), a move to a column and row, archive then restore, and delete.
#[tokio::test]
async fn field_edits_round_trip() {
    let mut live = live();
    let cards = live.cards();
    let refs: Vec<String> = cards
        .iter()
        .map(|c| c["ref"].as_str().unwrap().to_owned())
        .collect();
    let labelled = cards
        .iter()
        .position(|c| !c["labels"].as_array().unwrap().is_empty())
        .expect("a demo card with labels");
    let card = |n: usize| refs[n].clone();
    // Six distinct cards: one edit of a field per card per second.
    let fields = card(if labelled == 0 { 1 } else { 0 });
    let others: Vec<String> = (0..refs.len())
        .filter(|&n| n != labelled && refs[n] != fields)
        .map(card)
        .collect();
    let (labelled, moved, archived, deleted) = (
        card(labelled),
        others[0].clone(),
        others[1].clone(),
        others[2].clone(),
    );

    let ack = live.edit(
        "headway_set_title",
        json!({ "card": fields, "title": "a clearer title" }),
    );
    assert_eq!(keys(&ack.json), ["events", "ok"]);
    assert_eq!(ack.text, "ok (1 events)\n");
    live.edit(
        "headway_set_description",
        json!({ "card": fields, "description": "the details" }),
    );
    live.edit(
        "headway_set_priority",
        json!({ "card": fields, "priority": "urgent" }),
    );
    live.edit(
        "headway_set_due",
        json!({ "card": fields, "due": "2026-10-01" }),
    );
    live.edit(
        "headway_set_estimate",
        json!({ "card": fields, "estimate": "5" }),
    );
    live.edit(
        "headway_set_labels",
        json!({ "card": labelled, "labels": ["only"] }),
    );
    live.edit(
        "headway_move_card",
        json!({ "card": moved, "column": "in progress", "row": 0 }),
    );
    live.edit("headway_archive_card", json!({ "card": archived }));
    live.edit("headway_delete_card", json!({ "card": deleted }));

    let shown = live.card_until(&fields, "the field edits", |c| {
        c["estimate"] == json!(5)
            && c["due"] == "2026-10-01"
            && c["priority"] == "urgent"
            && c["title"] == "a clearer title"
            && c["description"] == "the details"
    });
    assert_eq!(shown["ref"], fields.as_str());
    let shown = live.card_until(&labelled, "the label set", |c| labels(c) == ["only"]);
    assert_eq!(labels(&shown), ["only"], "replaced, not added to");
    let shown = live.card_until(&moved, "the move", |c| c["column"] == "In Progress");
    assert_eq!(shown["column"], "In Progress");
    let board = live.board_until("the archive and the delete", |b| {
        let live_refs: Vec<&str> = b["columns"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|col| col["cards"].as_array().unwrap())
            .map(|c| c["ref"].as_str().unwrap())
            .collect();
        !live_refs.contains(&archived.as_str()) && !live_refs.contains(&deleted.as_str())
    });
    let in_progress = board["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|col| col["name"] == "In Progress")
        .unwrap();
    assert_eq!(
        in_progress["cards"][0]["ref"],
        moved.as_str(),
        "row 0 is the top"
    );
    let archived_refs: Vec<&str> = board["archived"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ref"].as_str().unwrap_or_default())
        .collect();
    assert!(archived_refs.contains(&archived.as_str()), "{board:#}");

    // The second edit of a field on one card, a second later: clears and
    // a restore.
    next_second();
    live.edit("headway_set_due", json!({ "card": fields, "due": "none" }));
    live.edit(
        "headway_set_estimate",
        json!({ "card": fields, "estimate": "none" }),
    );
    live.edit("headway_set_labels", json!({ "card": labelled }));
    live.edit("headway_restore_card", json!({ "card": archived }));
    live.card_until(&fields, "the clears", |c| {
        c["due"].is_null() && c["estimate"].is_null()
    });
    live.card_until(&labelled, "the label clear", |c| labels(c).is_empty());
    live.card_until(&archived, "the restore", |c| c["column"] != "archived");
    let err = live
        .call("headway_show_card", json!({ "card": deleted }))
        .unwrap_err();
    assert!(err.contains("no card matching"), "{err}");

    // A malformed value fails before anything is written.
    for (tool, args) in [
        (
            "headway_set_due",
            json!({ "card": fields, "due": "tomorrow" }),
        ),
        (
            "headway_set_estimate",
            json!({ "card": fields, "estimate": "lots" }),
        ),
        (
            "headway_move_card",
            json!({ "card": moved, "to": "up", "row": 0 }),
        ),
    ] {
        assert!(live.call(tool, args).is_err(), "{tool}");
    }
}

/// `comment` posts in the card's thread, `reply_to` threads under a card
/// comment, `path`/`line` leave an inline comment on the newest review
/// record, `reply_to` a review comment threads on its record and lines,
/// review flags beside a reply are refused, and `record` alone comments on
/// the whole commit — none of which lands in the card's own thread. (Ports
/// `review_comments_post_show_and_reply`.)
#[tokio::test]
async fn comments_reply_and_land_on_review_records() {
    let mut live = live();
    let card = live.cards()[0]["ref"].as_str().unwrap().to_owned();

    let ack = live.edit(
        "headway_comment_card",
        json!({ "card": card, "body": "committed abc123" }),
    );
    assert_eq!(keys(&ack.json), ["events", "ok"]);
    let shown = live.card_until(&card, "the comment", |c| {
        !c["comments"].as_array().unwrap().is_empty()
    });
    let top = shown["comments"][0]["id"].as_str().unwrap().to_owned();
    let top_words = wordid::encode(NoteId::from_hex(&top).unwrap().bytes());
    live.edit(
        "headway_comment_card",
        json!({ "card": card, "reply_to": top_words, "body": "thanks" }),
    );
    let shown = live.card_until(&card, "the reply", |c| {
        c["comments"].as_array().unwrap().len() == 2
    });
    let reply = shown["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "thanks")
        .unwrap();
    assert_eq!(reply["parent"], top.as_str(), "threads under it");

    // No record yet: a line comment has nothing to land on.
    let err = live
        .call(
            "headway_comment_card",
            json!({ "card": card, "path": "a.rs", "line": "1", "body": "x" }),
        )
        .unwrap_err();
    assert!(err.contains("no review record"), "{err}");

    let sha = "0123456789abcdef0123456789abcdef01234567";
    {
        let mut app = live.notedeck.app_context();
        let mut cx = app.tool_context();
        mutate(&mut cx, store::BOARD_ID, |view| {
            Ok(BoardAction::AddReview {
                card: resolve_card(view, &card)?,
                review: ReviewFields {
                    commit: Some(sha.to_owned()),
                    ..Default::default()
                },
                sources: Vec::new(),
            })
        })
        .expect("records a review");
    }
    live.card_until(&card, "the record", |c| {
        !c["reviews"].as_array().unwrap().is_empty()
    });

    live.edit(
        "headway_comment_card",
        json!({ "card": card, "path": "src/a.rs", "line": "3-5", "body": "why a clone?" }),
    );
    let shown = live.card_until(&card, "the line comment", |c| {
        c["reviews"][0]["comments"].as_array().unwrap().len() == 1
    });
    let line = &shown["reviews"][0]["comments"][0];
    assert_eq!(line["path"], "src/a.rs");
    assert_eq!(line["line"], "3-5");
    assert_eq!(line["side"], "new");
    assert_eq!(line["commit"], sha);
    assert!(line["parent"].is_null(), "{line:#}");

    // Reply by a hex prefix of the review comment's id.
    let line_id = line["id"].as_str().unwrap().to_owned();
    live.edit(
        "headway_comment_card",
        json!({ "card": card, "reply_to": &line_id[..12], "body": "dropped it" }),
    );
    let shown = live.card_until(&card, "the review reply", |c| {
        c["reviews"][0]["comments"].as_array().unwrap().len() == 2
    });
    let reply = shown["reviews"][0]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "dropped it")
        .unwrap();
    assert_eq!(reply["parent"], line_id.as_str());
    assert_eq!(reply["path"], "src/a.rs", "under the same lines");
    assert_eq!(reply["line"], "3-5");

    let err = live
        .call(
            "headway_comment_card",
            json!({ "card": card, "reply_to": line_id, "line": "1", "body": "x" }),
        )
        .unwrap_err();
    assert!(err.contains("drop --path/--line"), "{err}");

    live.edit(
        "headway_comment_card",
        json!({ "card": card, "record": &sha[..7], "body": "lgtm overall" }),
    );
    let shown = live.card_until(&card, "the whole-commit comment", |c| {
        c["reviews"][0]["comments"].as_array().unwrap().len() == 3
    });
    let whole = shown["reviews"][0]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "lgtm overall")
        .unwrap();
    assert!(
        whole["path"].is_null() && whole["parent"].is_null(),
        "{whole:#}"
    );

    // Only the two card comments are in the card's own thread.
    assert_eq!(shown["comments"].as_array().map(Vec::len), Some(2));
}

/// A card tool's board comes from the card ref before the caller's default
/// (`$HEADWAY_BOARD` on a command line), and an explicit `board` that
/// disagrees with the ref is refused; with no ref, the default is used.
#[tokio::test]
async fn a_ref_routes_past_the_callers_default_board() {
    let mut live = live();
    let card = live.cards()[0]["ref"].as_str().unwrap().to_owned();

    live.edit(
        "headway_set_priority",
        json!({ "card": card, "priority": "low", "default_board": "elsewhere" }),
    );
    let err = live
        .call(
            "headway_set_priority",
            json!({ "card": card, "priority": "low", "board": "elsewhere" }),
        )
        .unwrap_err();
    assert!(
        err.contains("conflicts with card ref board 'headway'"),
        "{err}"
    );
    let err = live
        .call(
            "headway_add_card",
            json!({ "title": "lost", "default_board": "elsewhere" }),
        )
        .unwrap_err();
    assert!(err.contains("no board 'elsewhere'"), "{err}");
    live.edit(
        "headway_add_card",
        json!({ "title": "found", "parent": card, "default_board": "elsewhere" }),
    );
}
