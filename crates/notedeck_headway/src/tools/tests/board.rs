//! The `headway` CLI's board commands — `board`, `seed`, `rename`,
//! `delete-board`/`restore-board`, `link`, `move-board`, `migrate` and
//! `share` — ported to the tools that replace them, run through the registry
//! as `nd headway <verb>` runs them.

use super::live::{Ack, Live, bare, live, next_second};
use super::*;

/// `headway_list_boards` once `ready` holds of its JSON, with the text `nd
/// headway board` prints. Panics after a bounded wait: ingest is async.
fn boards_until(live: &mut Live, what: &str, ready: impl Fn(&serde_json::Value) -> bool) -> Ack {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let listed = live.edit("headway_list_boards", json!({}));
        if ready(&listed.json) {
            return listed;
        }
        assert!(Instant::now() < deadline, "never settled: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The board `board` as `show --board <board> --json` prints it, once `ready`
/// holds of it.
fn board_until(
    live: &mut Live,
    board: &str,
    what: &str,
    ready: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(shown) = live.call("headway_show_board", json!({ "board": board }))
            && ready(&shown)
        {
            return shown;
        }
        assert!(Instant::now() < deadline, "never settled: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every live card on `board`.
fn card_count(board: &serde_json::Value) -> usize {
    board["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|col| col["cards"].as_array().unwrap().len())
        .sum()
}

/// The entry for `id` in a board list, if any.
fn listed<'a>(list: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
    list.as_array().unwrap().iter().find(|b| b["id"] == id)
}

/// Whether `board` seals its edits into a channel, as the edit tools see it.
fn sealed(live: &mut Live, board: &str) -> bool {
    let ndb = live.notedeck.app_context().ndb.clone();
    let me = live.account.pubkey;
    let teams = teams::teams_from_ndb(&ndb, &me);
    resolve_target(&ndb, &me, board, &teams).is_ok_and(|target| target.channel.is_some())
}

/// A board `slug` that `owner` seals and shares with the live account, as a
/// co-member's board arrives: through a gift-wrapped key-share.
fn shared_with_me(live: &mut Live, owner: &FullKeypair, slug: &str) {
    let ndb = live.notedeck.app_context().ndb.clone();
    let secret = owner.secret_key.secret_bytes();
    let root = nostrdb_net::sns::derive_board_root(&secret, slug);
    assert!(store::create_shared_board(
        &ndb,
        &owner.pubkey,
        &secret,
        slug,
        "Theirs",
        &root,
        &mut store::NoPublish,
    ));
    assert!(store::share_board(
        &ndb,
        &secret,
        &live.account.pubkey,
        &event::board_address(&owner.pubkey, slug),
        &root,
        &mut store::NoPublish,
    ));
}

/// A seeded board is born sealed and titled by its slug unless given a
/// title; with only a title it seeds the slug derived from it; with neither
/// it is refused, as is seeding a board that exists.
#[test]
fn seed_titles_by_slug_and_seals() {
    let mut live = bare();
    let seeded = live.edit("headway_seed_board", json!({ "board": "work" }));
    assert_eq!(seeded.json["board"], "work");
    assert!(
        seeded.json["events"].as_u64().unwrap() > 0,
        "{}",
        seeded.json
    );
    assert!(
        seeded.text.starts_with("seeded sealed board 'work' ("),
        "{}",
        seeded.text
    );
    let work = board_until(&mut live, "work", "the seeded board", |b| {
        b["columns"].as_array().is_some_and(|c| c.len() == 5)
    });
    assert_eq!(work["title"], "work", "titled by its slug");
    assert!(sealed(&mut live, "work"), "born sealed");

    live.edit("headway_seed_board", json!({ "title": "Tune Assistant" }));
    let tune = board_until(&mut live, "tune-assistant", "the titled board", |_| true);
    assert_eq!(tune["title"], "Tune Assistant");

    let err = live.call("headway_seed_board", json!({})).unwrap_err();
    assert!(err.contains("pass --board"), "{err}");
    let err = live
        .call("headway_seed_board", json!({ "board": "work" }))
        .unwrap_err();
    assert!(err.contains("already exists"), "{err}");
}

/// `rename` retitles the board it is given, slug unchanged.
#[test]
fn rename_retitles_the_board() {
    let mut live = live();
    let renamed = live.edit("headway_rename_board", json!({ "title": "Renamed board" }));
    assert_eq!(renamed.text, "ok (1 events)\n");
    let board = board_until(&mut live, store::BOARD_ID, "the new title", |b| {
        b["title"] == "Renamed board"
    });
    assert_eq!(board["id"], store::BOARD_ID);
}

/// The listing is what the app's switcher shows: own boards, then the ones
/// shared with the account (with their owner), then the deleted ones. The
/// retired `board <id>` switch says what to use instead.
#[test]
fn board_lists_own_shared_and_deleted_boards() {
    let mut live = bare();
    live.seed("work");
    live.seed("scratch");
    let owner = FullKeypair::generate();
    shared_with_me(&mut live, &owner, "theirs");
    live.edit("headway_delete_board", json!({ "board": "scratch" }));

    let list = boards_until(&mut live, "every kind of board", |list| {
        listed(list, "theirs").is_some()
            && listed(list, "scratch").is_some_and(|b| b["deleted"] == true)
    });
    let ids: Vec<&str> = list
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["work", "theirs", "scratch"], "own, shared, deleted");
    assert_eq!(
        listed(&list.json, "theirs").unwrap()["owner"],
        owner.pubkey.hex()
    );
    assert!(listed(&list.json, "work").unwrap().get("owner").is_none());
    assert!(listed(&list.json, "work").unwrap().get("deleted").is_none());

    let text = plain(&list.text);
    let (live_part, gone) = text.split_once("\ndeleted").expect("a deleted section");
    assert!(live_part.contains("shared with me"), "{text}");
    assert!(!live_part.contains("scratch"), "{text}");
    assert!(gone.contains("scratch"), "{text}");

    let err = live
        .call("headway_list_boards", json!({ "id": "work" }))
        .unwrap_err();
    assert!(
        err.contains("--board work") && err.contains("HEADWAY_BOARD"),
        "{err}"
    );
}

/// A deleted board keeps its cards, refuses edits with how to undo it, and
/// comes back as it was. Deleting twice writes nothing.
#[test]
fn delete_board_hides_it_until_restored() {
    let mut live = bare();
    live.edit("headway_seed_board", json!({ "board": "scratch" }));
    board_until(&mut live, "scratch", "the board", |_| true);
    live.add(json!({ "title": "Keep me", "board": "scratch", "column": "todo" }));

    let deleted = live.edit("headway_delete_board", json!({ "board": "scratch" }));
    assert!(
        deleted
            .text
            .starts_with("deleted board 'scratch' (1 cards kept)"),
        "{}",
        deleted.text
    );
    let board = board_until(&mut live, "scratch", "the delete", |b| b["deleted"] == true);
    assert_eq!(card_count(&board), 1, "a deleted board keeps its cards");

    let err = live
        .call(
            "headway_add_card",
            json!({ "title": "Nope", "board": "scratch" }),
        )
        .unwrap_err();
    assert!(err.contains("restore-board"), "{err}");
    let again = live.edit("headway_delete_board", json!({ "board": "scratch" }));
    assert_eq!(again.json["events"], 0);
    assert_eq!(again.json["noop"], "already deleted");

    next_second();
    live.edit("headway_restore_board", json!({ "board": "scratch" }));
    let board = board_until(&mut live, "scratch", "the restore", |b| {
        b["deleted"] == false
    });
    assert_eq!(card_count(&board), 1);
}

/// Only a board's owner deletes, seals or shares it: a board shared with
/// the account is refused by name.
#[test]
fn owner_only_commands_refuse_a_shared_board() {
    let mut live = bare();
    let owner = FullKeypair::generate();
    shared_with_me(&mut live, &owner, "theirs");
    boards_until(&mut live, "the shared board", |list| {
        listed(list, "theirs").is_some()
    });
    for (tool, args) in [
        ("headway_delete_board", json!({ "board": "theirs" })),
        (
            "headway_migrate_board",
            json!({ "board": "theirs", "new_channel": true }),
        ),
        (
            "headway_share_board",
            json!({ "board": "theirs", "recipient": FullKeypair::generate().pubkey.hex() }),
        ),
    ] {
        let err = live.call(tool, args).unwrap_err();
        assert!(err.contains("only the board owner can"), "{tool}: {err}");
    }
}

/// `link` puts a card on a second board and keeps it on the first;
/// `move-board` takes it off the first. Both refuse a target that doesn't
/// exist, and a move between two sealed boards, which can't read each
/// other's cards, is refused whole.
#[test]
fn link_and_move_a_card_between_boards() {
    let mut live = live();
    live.seed("dave");
    let cards = live.cards();
    let linked = cards[0]["ref"].as_str().unwrap().to_owned();
    let moved = cards[1]["ref"].as_str().unwrap().to_owned();

    let ack = live.edit("headway_link_card", json!({ "card": linked, "to": "dave" }));
    assert!(ack.text.starts_with("linked to 'dave' ("), "{}", ack.text);
    let ack = live.edit(
        "headway_move_card_to_board",
        json!({ "card": moved, "to": "dave" }),
    );
    assert!(ack.text.starts_with("moved to 'dave' ("), "{}", ack.text);

    let dave = board_until(&mut live, "dave", "both cards", |b| card_count(b) == 2);
    let on = |board: &serde_json::Value, card: &str| {
        let id = card.rsplit('/').next().unwrap();
        board["columns"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["cards"].as_array().unwrap())
            .any(|c| c["ref"].as_str().unwrap().ends_with(id))
    };
    assert!(on(&dave, &linked) && on(&dave, &moved), "{dave:#}");
    let home = board_until(&mut live, store::BOARD_ID, "the move off", |b| {
        card_count(b) == 6
    });
    assert!(on(&home, &linked), "a linked card stays");
    assert!(!on(&home, &moved), "a moved card leaves");

    let err = live
        .call("headway_link_card", json!({ "card": linked, "to": "nope" }))
        .unwrap_err();
    assert!(err.contains("no board 'nope'"), "{err}");

    for board in ["one", "two"] {
        live.edit("headway_seed_board", json!({ "board": board }));
        board_until(&mut live, board, "the sealed board", |_| true);
    }
    let card = live.add(json!({ "title": "stays put", "board": "one" }));
    let err = live
        .call(
            "headway_move_card_to_board",
            json!({ "card": card, "to": "two" }),
        )
        .unwrap_err();
    assert!(err.contains("Re-create it on the target board"), "{err}");
    let one = board_until(&mut live, "one", "the card", |_| true);
    assert_eq!(card_count(&one), 1, "a refused move leaves the card");
}

/// `migrate --dry-run` reports what a run would re-seal and writes nothing;
/// the run seals the board, after which nothing is left to seal. A board
/// with no channel needs `new_channel` to say so.
#[test]
fn migrate_seals_a_plaintext_board() {
    // The demo board, waited on to its last event rather than its cards: the
    // edges and history land after them, and a note still in flight when
    // the run reads the board would land plaintext after it.
    let mut live = bare();
    let ndb = live.notedeck.app_context().ndb.clone();
    let sub = ndb
        .subscribe(&[event::headway_filter(&live.account.pubkey)])
        .expect("subscribe");
    let mut stream = SubscriptionStream::new(ndb.clone(), sub);
    let seeded = store::seed_demo_board(
        &ndb,
        &live.account.pubkey,
        &live.account.secret_key.secret_bytes(),
        store::BOARD_ID,
        1_700_000_000,
        &mut store::NoPublish,
    );
    notedeck_testing::await_notes(&mut stream, seeded);
    assert!(!sealed(&mut live, store::BOARD_ID));

    let err = live
        .call(
            "headway_migrate_board",
            json!({ "board": store::BOARD_ID, "dry_run": true }),
        )
        .unwrap_err();
    assert!(err.contains("--new-channel"), "{err}");

    let dry = live.edit(
        "headway_migrate_board",
        json!({ "board": store::BOARD_ID, "dry_run": true, "new_channel": true }),
    );
    let unsealed = dry.json["unsealed"].as_u64().unwrap();
    assert!(unsealed > 0, "{}", dry.json);
    assert_eq!(dry.json["unsealed"], dry.json["notes"]);
    assert_eq!(dry.json["events"], 0);
    assert!(
        dry.text.starts_with("dry run: would re-seal"),
        "{}",
        dry.text
    );
    assert!(dry.text.contains("into its NEW channel"), "{}", dry.text);
    assert!(
        !sealed(&mut live, store::BOARD_ID),
        "a dry run seals nothing"
    );

    let run = live.edit(
        "headway_migrate_board",
        json!({ "board": store::BOARD_ID, "new_channel": true }),
    );
    assert_eq!(run.json["resealed"], unsealed);
    assert!(
        run.text.starts_with("migrated board 'headway' to SNS"),
        "{}",
        run.text
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    // The board reads as missing for a moment: the roster has its channel
    // before the re-sealed notes unwrap into it.
    let again = loop {
        let again = live.call(
            "headway_migrate_board",
            json!({ "board": store::BOARD_ID, "dry_run": true }),
        );
        if let Ok(again) = &again
            && again["unsealed"] == 0
        {
            break again.clone();
        }
        assert!(Instant::now() < deadline, "never sealed: {again:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(again["channel"], "existing");
    assert!(sealed(&mut live, store::BOARD_ID));
    assert_eq!(
        card_count(&live.call("headway_show_board", json!({})).unwrap()),
        7
    );
}

/// `share` refuses what it must not share — our own key, a board that
/// isn't there, a plaintext board — and, with no private relay to carry
/// it, a key-share that would never arrive. What it sends is a gift-wrap
/// to the member naming the board's channel.
#[test]
fn share_refuses_what_it_must_not_share() {
    let mut live = live();
    live.edit("headway_seed_board", json!({ "board": "shared" }));
    board_until(&mut live, "shared", "the sealed board", |_| true);
    let me = live.account.pubkey;
    let member = FullKeypair::generate().pubkey;

    for (args, why) in [
        (
            json!({ "board": "shared", "recipient": me.hex() }),
            "your own key",
        ),
        (
            json!({ "board": "nope", "recipient": member.hex() }),
            "no board 'nope'",
        ),
        (
            json!({ "board": store::BOARD_ID, "recipient": member.hex() }),
            "is not sealed",
        ),
        (
            json!({ "board": "shared", "recipient": "nobody" }),
            "not an npub",
        ),
        (
            json!({ "board": "shared", "recipient": member.npub().unwrap() }),
            "needs a private relay",
        ),
    ] {
        let err = live.call("headway_share_board", args.clone()).unwrap_err();
        assert!(err.contains(why), "{args}: expected '{why}' in {err}");
    }

    let mut app = live.notedeck.app_context();
    let cx = app.tool_context();
    let key = crate::tools::board::share_key(&cx, "shared", &member).expect("an own sealed board");
    assert_eq!(key.board_addr, event::board_address(&me, "shared"));
    let secret = live.account.secret_key.secret_bytes();
    assert_eq!(
        key.root,
        nostrdb_net::sns::derive_board_root(&secret, "shared"),
        "the board's channel"
    );
    let mut sent = Frames::default();
    assert!(store::share_board(
        cx.ndb,
        &secret,
        &member,
        &key.board_addr,
        &key.root,
        &mut sent,
    ));
    let wrap: serde_json::Value = serde_json::from_str(&sent.0[0]).unwrap();
    assert_eq!(wrap[1]["kind"], 1059);
    assert_eq!(wrap[1]["tags"][0], json!(["p", member.hex()]));
}

/// A [`store::Publisher`] that keeps every frame.
#[derive(Default)]
struct Frames(Vec<String>);

impl store::Publisher for Frames {
    fn publish(&mut self, frame: &str) {
        self.0.push(frame.to_owned());
    }
}
