//! A live notedeck for the tool tests that port the `headway` CLI's e2e
//! cases: signed in as a fresh account with the demo board seeded, calling
//! tools through the registry as `nd` calls them.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// A notedeck signed in as a fresh account, with the demo board seeded and
/// settled. Bind it for the whole test: the database lives in `_dir`.
pub(super) struct Live {
    _dir: TempDir,
    pub(super) notedeck: notedeck::Notedeck,
}

/// [`Live`], ready to edit.
pub(super) fn live() -> Live {
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
    let mut notedeck = notedeck::Notedeck::init(&egui::Context::default(), dir.path(), &args);
    let ndb = notedeck.app_context().ndb.clone();
    store::seed_demo_board(
        &ndb,
        &account.pubkey,
        &account.secret_key.secret_bytes(),
        store::BOARD_ID,
        1_700_000_000,
        &mut store::NoPublish,
    );
    settled_view(
        &ndb,
        &account.pubkey,
        store::BOARD_ID,
        "the demo board",
        |v| v.columns.iter().map(|c| c.cards.len()).sum::<usize>() == 7,
    );
    Live {
        _dir: dir,
        notedeck,
    }
}

impl Live {
    /// Call the registered tool `name`, as `nd` would.
    pub(super) fn call(
        &mut self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut app = self.notedeck.app_context();
        let mut cx = app.tool_context();
        call_registered(&mut cx, name, args)
    }

    /// Call the edit tool `name` and hand back its ack and the text `nd`
    /// prints for it, failing the test on an error.
    pub(super) fn edit(&mut self, name: &str, args: serde_json::Value) -> Ack {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        let mut app = self.notedeck.app_context();
        let mut cx = app.tool_context();
        let rendered = tool.call_rendered(&mut cx, &args);
        match rendered.outcome {
            notedeck::ToolOutcome::Data(json) => Ack {
                json,
                text: rendered.text.expect("an edit renders its ack"),
            },
            notedeck::ToolOutcome::Error(err) => panic!("{name} {args}: {err}"),
            notedeck::ToolOutcome::Intent(_) => panic!("{name} answered with an intent"),
        }
    }

    /// The card `card` as `show <card> --json` prints it, once `ready`
    /// holds of it. Panics after a bounded wait: ingest is async.
    pub(super) fn card_until(
        &mut self,
        card: &str,
        what: &str,
        ready: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(shown) = self.call("headway_show_card", json!({ "card": card }))
                && ready(&shown[0])
            {
                return shown[0].clone();
            }
            assert!(Instant::now() < deadline, "never settled: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The primary board as `show --json` prints it, once `ready` holds.
    pub(super) fn board_until(
        &mut self,
        what: &str,
        ready: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let board = self
                .call("headway_show_board", json!({}))
                .expect("the board");
            if ready(&board) {
                return board;
            }
            assert!(Instant::now() < deadline, "never settled: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every live card on the primary board, in column order.
    pub(super) fn cards(&mut self) -> Vec<serde_json::Value> {
        let board = self
            .call("headway_show_board", json!({}))
            .expect("the board");
        board["columns"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|col| col["cards"].as_array().unwrap().clone())
            .collect()
    }
}

/// What an edit tool answered: its JSON and the text `nd` prints.
pub(super) struct Ack {
    pub(super) json: serde_json::Value,
    pub(super) text: String,
}

/// The keys of a JSON object, sorted.
pub(super) fn keys(value: &serde_json::Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// A card's labels, sorted.
pub(super) fn labels(card: &serde_json::Value) -> Vec<&str> {
    let mut labels: Vec<&str> = card["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    labels.sort_unstable();
    labels
}

/// Sleep into the next wall-clock second. Every edit of one field on one card
/// is a latest-wins event stamped in whole seconds, so a second edit of the
/// same field in the same second ties with the first and may lose. A person
/// never edits that fast; a test does.
pub(super) fn next_second() {
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let second = now().as_secs();
    while now().as_secs() == second {
        std::thread::sleep(Duration::from_millis(20));
    }
}
