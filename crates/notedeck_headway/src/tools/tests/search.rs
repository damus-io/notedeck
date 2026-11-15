//! The `headway` CLI's `grep` and `list` e2e cases, ported to the tools that
//! replace them and run through the registry as `nd headway grep` and `nd
//! headway list` run them. The text is [`headway::grep`]'s and
//! [`headway::list`]'s, which the CLI prints too, so these pin the shapes
//! scripts and skills read: the hindsights skill greps
//! `hindsight/<file>\.md` and `--board <b> '^Lane:'`.

use super::live::{Ack, Live, bare, next_second};
use super::*;

/// Wait until `tool` with `args` answers `n` rows, and hand back the answer.
/// Ingest is async, so a search right after an edit may not see it yet.
fn rows_until(live: &mut Live, tool: &str, args: serde_json::Value, n: usize) -> Ack {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ack = live.edit(tool, args.clone());
        let len = ack.json.as_array().map_or(0, Vec::len);
        if len == n {
            return ack;
        }
        assert!(
            Instant::now() < deadline,
            "{tool} {args}: never reached {n} rows: {}",
            ack.json
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The rows `tool` answers for `args`, as JSON.
fn rows(live: &mut Live, tool: &str, args: serde_json::Value) -> Vec<serde_json::Value> {
    live.edit(tool, args)
        .json
        .as_array()
        .unwrap_or_else(|| panic!("{tool}: an array"))
        .clone()
}

/// The refs of the rows `tool` answers for `args`.
fn refs(live: &mut Live, tool: &str, args: serde_json::Value) -> Vec<String> {
    rows(live, tool, args)
        .iter()
        .map(|r| r["ref"].as_str().unwrap().to_owned())
        .collect()
}

/// The row for `card_ref` among `rows`.
fn row<'a>(rows: &'a [serde_json::Value], card_ref: &str) -> &'a serde_json::Value {
    rows.iter()
        .find(|r| r["ref"] == card_ref)
        .unwrap_or_else(|| panic!("no row for {card_ref}: {rows:?}"))
}

/// A grep row's matches as `field: text`.
fn matches(row: &serde_json::Value) -> Vec<String> {
    row["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                m["field"].as_str().unwrap(),
                m["text"].as_str().unwrap()
            )
        })
        .collect()
}

/// `text` without its SGR escapes, as nd prints it into a pipe.
fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("\x1b[") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let end = after.find('m').expect("an SGR ends in m");
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The search runs over every board unless `board` or `container` narrows
/// it, archived cards included, smart-case, grouped per card; the text is
/// `nd headway grep`'s, and the hindsights skill's two patterns work.
#[tokio::test]
async fn grep_searches_card_text_across_boards() {
    let mut live = bare();
    live.seed("headway");
    live.seed("other");
    let epic = live.add(json!({
        "title": "Zephyr reconnect epic",
        "description": "first line, no hit\nthe ZEPHYR stalls past the cap\nLane: worktree=/tmp/lane branch=lane",
    }));
    let child = live.add(json!({ "title": "child task", "parent": epic }));
    live.edit(
        "headway_comment_card",
        json!({ "card": child, "body": "the zephyr looks fine now" }),
    );
    let quiet = live.add(json!({
        "title": "Quiet card",
        "description": "written up in docs/hindsight/apply-reports-ids.md",
    }));
    let outside = live.add(json!({ "title": "Zephyr docs" }));
    let archived = live.add(json!({ "title": "Archived zephyr card" }));
    live.edit("headway_archive_card", json!({ "card": archived }));
    let elsewhere = live.add(json!({ "title": "zephyr on another board", "board": "other" }));

    rows_until(
        &mut live,
        "headway_grep",
        json!({ "pattern": "fine now" }),
        1,
    );
    let found = rows_until(&mut live, "headway_grep", json!({ "pattern": "zephyr" }), 5).json;
    let found = found.as_array().unwrap();
    assert_eq!(
        matches(row(found, &epic)),
        [
            "title: Zephyr reconnect epic",
            "desc: the ZEPHYR stalls past the cap",
        ]
    );
    assert_eq!(
        matches(row(found, &child)),
        ["comment: the zephyr looks fine now"]
    );
    assert_eq!(row(found, &child)["column"], "Backlog");
    assert_eq!(row(found, &child)["board"], "headway");
    assert!(
        row(found, &child)["matches"][0]["created_at"]
            .as_u64()
            .is_some_and(|t| t > 0)
    );
    assert!(found.iter().all(|r| r["ref"] != quiet));
    assert!(found.iter().any(|r| r["ref"] == outside));
    assert_eq!(row(found, &archived)["column"], "archived");
    assert_eq!(row(found, &elsewhere)["board"], "other");

    // The caller's board ($HEADWAY_BOARD) never narrows a search.
    let found = rows(
        &mut live,
        "headway_grep",
        json!({ "pattern": "zephyr", "default_board": "other" }),
    );
    assert_eq!(found.len(), 5);

    // Case written into the pattern is honoured; -i and -s override it.
    assert_eq!(
        refs(&mut live, "headway_grep", json!({ "pattern": "ZEPHYR" })),
        [epic.clone()]
    );
    let insensitive = json!({ "pattern": "ZEPHYR", "ignore_case": true });
    assert_eq!(rows(&mut live, "headway_grep", insensitive).len(), 5);
    let sensitive = json!({ "pattern": "zephyr", "case_sensitive": true });
    assert!(!refs(&mut live, "headway_grep", sensitive).contains(&outside));
    let both = json!({ "pattern": "x", "ignore_case": true, "case_sensitive": true });
    assert!(live.call("headway_grep", both).is_err());

    // `--in` keeps the epic and its subtree.
    let within = refs(
        &mut live,
        "headway_grep",
        json!({ "pattern": "zephyr", "container": epic }),
    );
    assert_eq!(within.len(), 2, "{within:?}");
    assert!(within.contains(&epic) && within.contains(&child));
    let err = live
        .call(
            "headway_grep",
            json!({ "pattern": "zephyr", "container": epic, "all": true }),
        )
        .unwrap_err();
    assert!(err.contains("drop --all"), "{err}");

    // `--board` narrows to one board, archived cards still included, and
    // `--all` widens it again.
    let one = rows(
        &mut live,
        "headway_grep",
        json!({ "pattern": "zephyr", "board": "headway" }),
    );
    assert_eq!(one.len(), 4, "{one:?}");
    assert_eq!(row(&one, &archived)["column"], "archived");
    assert_eq!(
        refs(
            &mut live,
            "headway_grep",
            json!({ "pattern": "zephyr", "board": "other" })
        ),
        [elsewhere.clone()]
    );
    let widened = json!({ "pattern": "zephyr", "board": "other", "all": true, "archived": true });
    assert_eq!(rows(&mut live, "headway_grep", widened).len(), 5);

    // The hindsights skill's searches: a hindsight file's path, and the
    // lanes on one board.
    let hindsight = json!({ "pattern": "hindsight/apply-reports-ids\\.md" });
    assert_eq!(refs(&mut live, "headway_grep", hindsight), [quiet.clone()]);
    let lanes = json!({ "pattern": "^Lane:", "board": "headway" });
    assert_eq!(refs(&mut live, "headway_grep", lanes), [epic.clone()]);
    let no_lanes = json!({ "pattern": "^Lane:", "board": "other" });
    assert!(refs(&mut live, "headway_grep", no_lanes).is_empty());

    // The text: each card headed by its full ref, title and column, its
    // matching lines beneath, the match highlighted. nd strips the color
    // into a pipe, leaving `nd headway grep`'s plain bytes.
    let text = live
        .edit("headway_grep", json!({ "pattern": "fine now" }))
        .text;
    assert!(text.contains("mfine now\x1b[0m"), "{text:?}");
    let plain = strip(&text);
    assert!(
        plain.starts_with(&format!("{child}  child task  (Backlog)\n")),
        "{plain}"
    );
    let line = plain
        .lines()
        .find(|l| l.starts_with("  comment  "))
        .unwrap_or_else(|| panic!("a comment row: {plain}"));
    let when = line["  comment  ".len()..]
        .strip_suffix("  the zephyr looks fine now")
        .unwrap_or_else(|| panic!("time, then the line: {line:?}"));
    assert!(when == "just now" || when.ends_with(" ago"), "{when:?}");
    let none = live.edit("headway_grep", json!({ "pattern": "zzz-no-such-text" }));
    assert_eq!(none.text, "no matches\n");
    assert_eq!(none.json, json!([]));
}

/// `list` filters every board's open cards by the query, narrows with
/// `board`/`container`, groups, sorts and marks the ready frontier, and
/// saves, lists, applies and deletes views; the text is `nd headway list`'s.
#[tokio::test]
async fn list_filters_cards_across_boards() {
    let mut live = bare();
    live.seed("headway");
    live.seed("other");
    // Cards made in one second tie in board order, and the rows below are
    // in board order, so each card gets its own second, as each run of the
    // CLI did.
    let epic = live.add(json!({ "title": "List epic", "labels": ["ui"] }));
    next_second();
    let child = live.add(json!({ "title": "child task", "parent": epic, "priority": "high" }));
    next_second();
    let shipped = live.add(json!({
        "title": "Shipped card",
        "labels": ["ui"],
        "priority": "high",
        "column": "done",
    }));
    next_second();
    let archived = live.add(json!({ "title": "Archived card" }));
    live.edit("headway_archive_card", json!({ "card": archived }));
    next_second();
    let elsewhere = live.add(json!({
        "title": "Card on another board",
        "board": "other",
        "labels": ["ui"],
        "priority": "high",
    }));

    let list = |live: &mut Live, args: serde_json::Value| refs(live, "headway_list", args);
    rows_until(
        &mut live,
        "headway_list",
        json!({ "query": "is:archived" }),
        1,
    );
    rows_until(&mut live, "headway_list", json!({ "query": "is:done" }), 1);
    // An add's priority lands after its card does, so wait for it too.
    let high = rows_until(&mut live, "headway_list", json!({ "query": "p:high" }), 2);
    let high: Vec<&str> = high
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ref"].as_str().unwrap())
        .collect();
    assert_eq!(high, [child.as_str(), elsewhere.as_str()]);
    assert_eq!(
        list(&mut live, json!({})),
        [epic.clone(), child.clone(), elsewhere.clone()]
    );

    // Terms AND; `all` brings the done card back; `is:done` asks for it.
    assert_eq!(
        list(&mut live, json!({ "query": "p:high label:ui" })),
        [elsewhere.clone()]
    );
    assert_eq!(
        list(
            &mut live,
            json!({ "query": "p:high label:ui", "all": true })
        ),
        [shipped.clone(), elsewhere.clone()]
    );
    assert_eq!(
        list(
            &mut live,
            json!({ "query": "col:backlog", "board": "headway" })
        ),
        [epic.clone(), child.clone()]
    );
    // The caller's board doesn't narrow it; `--in` and `--board` do.
    assert_eq!(
        list(&mut live, json!({ "default_board": "other" })).len(),
        3
    );
    assert_eq!(
        list(&mut live, json!({ "container": epic })),
        [epic.clone(), child.clone()]
    );
    assert_eq!(
        list(&mut live, json!({ "board": "other" })),
        [elsewhere.clone()]
    );
    assert_eq!(
        list(&mut live, json!({ "query": elsewhere })),
        [elsewhere.clone()]
    );

    // The JSON carries the card's fields plus where it sits.
    let done = rows(&mut live, "headway_list", json!({ "query": "is:done" }));
    assert_eq!(done[0]["column"], "Done");
    assert_eq!(done[0]["board"], "headway");
    assert_eq!(done[0]["priority"], "high");
    let gone = live.edit("headway_list", json!({ "query": "is:archived" }));
    assert_eq!(gone.json[0]["column"], "archived");
    assert!(
        strip(&gone.text).contains("[archived]  Archived card"),
        "{}",
        gone.text
    );
    assert!(!list(&mut live, json!({ "query": "is:any" })).contains(&archived));

    // The text: markers, ref and column before the title; `limit` caps it.
    let text = live
        .edit("headway_list", json!({ "query": "p:high", "limit": 1 }))
        .text;
    let plain = strip(&text);
    assert_eq!(plain.lines().count(), 1, "{plain}");
    assert!(plain.starts_with(&format!("↑ {child}")), "{plain:?}");
    assert!(plain.contains("[Backlog]  child task"), "{plain:?}");
    assert!(text.contains('\x1b'), "nd strips the color, not the tool");
    let none = live.edit("headway_list", json!({ "query": "zzz-nothing" }));
    assert_eq!(none.text, "no cards\n");

    // Display properties.
    let text =
        |live: &mut Live, args: serde_json::Value| strip(&live.edit("headway_list", args).text);
    let out = text(&mut live, json!({ "board": "other" }));
    assert!(out.contains("Card on another board  [ui]"), "{out:?}");
    let out = text(&mut live, json!({ "board": "other", "hide": "labels" }));
    assert!(!out.contains("[ui]") && out.contains(&elsewhere), "{out:?}");
    let out = text(&mut live, json!({ "board": "other", "show": "status" }));
    assert_eq!(out.trim_end(), "[Backlog]  Card on another board");
    assert!(
        live.call("headway_list", json!({ "show": "nope" }))
            .is_err()
    );

    // Grouping, `empty` and sub-groups.
    let out = text(&mut live, json!({ "group": "status", "all": true }));
    let headers: Vec<&str> = out.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(headers, ["## Backlog (3)", "## Done (1)"], "{out}");
    assert!(!out.contains("[Backlog]"), "{out:?}");
    let out = text(
        &mut live,
        json!({ "group": "status", "empty": true, "board": "headway" }),
    );
    assert!(out.contains("## In Progress (0)"), "{out}");
    let out = text(
        &mut live,
        json!({ "group": "board", "sub_group": "priority" }),
    );
    assert!(out.contains("\n  ## High (1)\n"), "{out}");
    let groups = live
        .edit(
            "headway_list",
            json!({ "group": "label", "sub_group": "board" }),
        )
        .json;
    assert_eq!(groups[0]["group"], "ui");
    assert_eq!(groups[0]["count"], 2);
    assert_eq!(groups[0]["rows"][0]["ref"], epic.as_str());
    assert_eq!(groups[0]["subgroups"][1]["group"], "other");
    assert_eq!(groups[1]["group"], "No label");
    assert_eq!(groups[1]["rows"][0]["ref"], child.as_str());

    // Ordering.
    let by_priority = list(&mut live, json!({ "sort": "priority" }));
    assert_eq!(by_priority.last(), Some(&epic), "{by_priority:?}");
    live.edit(
        "headway_set_due",
        json!({ "card": epic, "due": "2026-11-01" }),
    );
    rows_until(&mut live, "headway_list", json!({ "query": "due:any" }), 1);
    assert_eq!(
        list(&mut live, json!({ "sort": "due", "descending": true })),
        [epic.clone(), child.clone(), elsewhere.clone()]
    );
    assert_eq!(
        list(
            &mut live,
            json!({ "sort": "priority", "ascending": true, "limit": 2 })
        )
        .len(),
        2
    );
    assert_eq!(
        list(&mut live, json!({ "sort": "due", "limit": 1 })),
        [epic.clone()]
    );

    // Work order marks the ready frontier.
    let ordered = live.edit(
        "headway_list",
        json!({ "board": "headway", "sort": "work-order" }),
    );
    let out = strip(&ordered.text);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with(&format!("    {epic}")), "{out:?}");
    assert!(lines[1].starts_with(&format!("▶ ↑ {child}")), "{out:?}");
    assert_eq!(ordered.json[0]["ready"], false);
    assert_eq!(ordered.json[1]["ready"], true);

    // Saved views: save, list, apply (the run's words narrow it), delete.
    let saved = live.edit(
        "headway_save_view",
        json!({ "save_view": "UI work", "query": "label:ui", "group": "board" }),
    );
    assert_eq!(saved.text, "saved view 'UI work' (ui-work)\n");
    assert_eq!(saved.json["slug"], "ui-work");
    assert_eq!(saved.json["builtin"], false);
    let views = rows_until(&mut live, "headway_list", json!({ "views": true }), 4);
    let names: Vec<&str> = views
        .text
        .lines()
        .map(|l| l.split("  ").next().unwrap_or("").trim())
        .collect();
    assert_eq!(names, ["Active", "Backlog", "All issues", "UI work"]);
    assert!(
        views.text.contains("label:ui  group:board"),
        "{}",
        views.text
    );
    assert_eq!(
        list(&mut live, json!({ "view": "ui work", "group": "none" })),
        [epic.clone(), elsewhere.clone()]
    );
    assert_eq!(
        list(
            &mut live,
            json!({ "view": "ui-work", "group": "none", "query": "p:high" })
        ),
        [elsewhere.clone()]
    );
    let out = text(&mut live, json!({ "view": "UI work" }));
    assert!(
        out.starts_with("## headway (1)"),
        "the view's grouping: {out}"
    );
    assert_eq!(
        list(
            &mut live,
            json!({ "view": "all", "query": "p:high label:ui" })
        ),
        [shipped.clone(), elsewhere.clone()]
    );
    assert_eq!(
        list(&mut live, json!({ "view": "backlog", "board": "other" })),
        [elsewhere.clone()]
    );
    assert!(
        live.call("headway_list", json!({ "view": "nope" }))
            .is_err()
    );
    let deleted = live.edit("headway_delete_view", json!({ "delete_view": "UI work" }));
    assert_eq!(deleted.text, "deleted view 'UI work'\n");
    let views = rows_until(&mut live, "headway_list", json!({ "views": true }), 3);
    assert!(!views.text.contains("UI work"), "{}", views.text);
    assert!(
        live.call("headway_delete_view", json!({ "delete_view": "UI work" }))
            .is_err()
    );
}
