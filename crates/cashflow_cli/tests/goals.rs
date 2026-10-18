//! End-to-end: `cashflow goals` over a headway cache seeded in the harness's
//! temp `$XDG_DATA_HOME`, where the command looks for the `headway` CLI's
//! cache by default. Synthetic boards and keys only.

mod harness;

use std::path::Path;

use harness::{Cashflow, expect_failure};
use headway::event::{self, BoardView};
use headway::store::{self, BoardAction, NewCard, NoPublish, Signer};
use nostrdb::{Filter, Ndb, SubscriptionStream, Transaction};
use nostrdb_net::FullKeypair;
use serde_json::Value;

/// A synthetic key (32 bytes of 0x11); it never signs anything real.
const NSEC: &str = "nsec1zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs4rm7hz";

/// A valid goal block.
const GOAL: &str =
    "Why: a runway.\n\n```cashflow-goal\nmetric: net\ntarget: 2500 CAD\nby: 2027-01-01\n```\n";

/// The owner [`NSEC`] is for.
fn owner() -> FullKeypair {
    FullKeypair::from_secret_bytes(&[0x11; 32]).expect("a valid key")
}

/// Fold `owner`'s plaintext board `slug` until `pred` holds, waking on each
/// ingest (subscribed before the first fold, so nothing is missed).
async fn settle(
    ndb: &Ndb,
    owner: &FullKeypair,
    slug: &str,
    pred: impl Fn(&BoardView) -> bool,
) -> BoardView {
    let sub = ndb
        .subscribe(&[Filter::new().authors([owner.pubkey.bytes()]).build()])
        .unwrap();
    let mut stream = SubscriptionStream::new(ndb.clone(), sub);
    loop {
        {
            let txn = Transaction::new(ndb).unwrap();
            if let Some(view) = event::load_board(ndb, &txn, &owner.pubkey, slug)
                && pred(&view)
            {
                return view;
            }
        }
        stream
            .wait_for_notes(1, std::time::Duration::from_secs(30))
            .await
            .expect("a note ingested before the deadline");
    }
}

/// Seed the headway cache at `path` with `owner`'s board `personal`: one
/// goal, one goal-labelled card whose block is broken, and an unlabelled card.
async fn seed(path: &Path, owner: &FullKeypair) {
    std::fs::create_dir_all(path).unwrap();
    let ndb = Ndb::new(path.to_str().unwrap(), &nostrdb::Config::new()).unwrap();
    let secret = owner.secret_key.secret_bytes();
    store::seed_board(
        &ndb,
        &owner.pubkey,
        &secret,
        "personal",
        "Personal",
        &mut NoPublish,
    );
    let cards = [
        ("Runway", GOAL, vec!["goal".to_owned()]),
        (
            "Typo",
            "```cashflow-goal\nmetric: nett\ntarget: 1\n```\n",
            vec!["goal".to_owned()],
        ),
        ("Groceries", GOAL, Vec::new()),
    ];
    for (title, description, labels) in cards {
        let view = settle(&ndb, owner, "personal", |_| true).await;
        store::apply(
            &ndb,
            "personal",
            &view,
            &owner.pubkey,
            &Signer::plain(&secret),
            BoardAction::AddCard(NewCard {
                col: 0,
                title: title.to_owned(),
                description: description.to_owned(),
                labels,
                ..NewCard::default()
            }),
            &mut NoPublish,
        );
        settle(&ndb, owner, "personal", |v| {
            v.columns[0].cards.iter().any(|c| c.title == title)
        })
        .await;
    }
}

/// With the signing key stored, `goals` reads the headway CLI's cache at its
/// default path and lists the goal and the broken one with full card refs,
/// in text and JSON; `--author` reads the same boards without a key.
#[tokio::test]
async fn goals_lists_the_goal_cards_from_the_headway_cache() {
    let cf = Cashflow::knowing("goals");
    let owner = owner();
    seed(&cf.data_home().join("headway-cli"), &owner).await;

    let err = expect_failure(cf.run(&["goals"]));
    assert!(err.contains("no signing key"), "{err}");
    assert!(err.contains("--author"), "{err}");

    cf.ok(&["login", NSEC]);
    let text = cf.ok(&["goals"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    let goal_line = lines
        .iter()
        .position(|l| l.contains("Runway"))
        .expect("the goal is listed");
    assert!(lines[goal_line].starts_with("headway:personal/"), "{text}");
    assert_eq!(
        lines[goal_line + 1],
        "    net ≥ 2500 CAD per month by 2027-01-01"
    );
    let typo = lines.iter().position(|l| l.contains("Typo")).unwrap();
    assert!(
        lines[typo + 1].starts_with("    error: line 2: unknown metric `nett`"),
        "{text}"
    );
    assert!(
        !text.contains("Groceries"),
        "an unlabelled card is not a goal"
    );

    let json: Value =
        serde_json::from_str(&cf.ok(&["goals", "--json", "--author", &owner.pubkey.hex()]))
            .unwrap();
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let runway = rows.iter().find(|r| r["title"] == "Runway").unwrap();
    assert!(
        runway["card"]
            .as_str()
            .unwrap()
            .starts_with("headway:personal/")
    );
    assert_eq!(runway["owner"], owner.pubkey.hex());
    assert_eq!(runway["goal"]["metric"], "net");
    assert_eq!(runway["goal"]["direction"], "at_least");
    assert_eq!(runway["goal"]["target"], "2500 CAD");
    assert_eq!(runway["goal"]["by"], "2027-01-01");
    let typo = rows.iter().find(|r| r["title"] == "Typo").unwrap();
    assert_eq!(typo["error"]["line"], 2);

    // A board with no goals on it says so on stderr and lists nothing.
    let out = cf.run(&["goals", "--board", "elsewhere"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no goals on elsewhere"));
}
